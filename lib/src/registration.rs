use super::*;

const REGISTRATION_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug)]
pub struct RegistrationClient {
    inner: v1beta1::RegistrationClient<transport::Channel>,
}

impl RegistrationClient {
    pub async fn new(path: impl AsRef<Utf8Path>) -> tonic::Result<Self> {
        let socket_path = path.as_ref().to_path_buf();
        let connector_path = socket_path.clone();

        // The URI host is ignored for Unix Domain Socket transport.
        let endpoint = transport::Endpoint::try_from("http://[::]:50051")
            .map_err(|err| {
                tonic::Status::internal(format!(
                    "failed to build kubelet registration endpoint: {err}"
                ))
            })?
            .connect_timeout(REGISTRATION_TIMEOUT)
            .timeout(REGISTRATION_TIMEOUT);

        let channel = endpoint
            .connect_with_connector(service_fn(move |_| {
                let path = connector_path.clone();
                async move { net::UnixStream::connect(path).await.map(TokioIo::new) }
            }))
            .await
            .map_err(|err| {
                tonic::Status::unavailable(format!(
                    "failed to connect to kubelet registration socket {}: {err}",
                    socket_path
                ))
            })?;

        Ok(Self {
            inner: v1beta1::RegistrationClient::new(channel),
        })
    }

    pub async fn register(&mut self, endpoint: &str, resource_name: &str) -> tonic::Result<()> {
        let request = v1beta1::RegisterRequest::new(endpoint, resource_name);
        self.inner.register(request).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use camino_tempfile::Utf8TempDir;

    use k8s_device_plugin_test::registration::start_mock_registration_server;

    use super::*;

    #[tokio::test]
    async fn registration_client_connects_over_unix_socket() {
        let server = start_mock_registration_server(None);

        let mut client = RegistrationClient::new(server.socket_path())
            .await
            .expect("connect registration client");
        client
            .register("plugin.sock", "example.com/device")
            .await
            .expect("register call over unix socket");

        let requests = server.collected_requests().await;
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].endpoint, "plugin.sock");
        assert_eq!(requests[0].version, v1beta1::VERSION);
        assert_eq!(requests[0].resource_name, "example.com/device");
        assert!(requests[0].options.is_none());

        server.shutdown();
    }

    #[tokio::test]
    async fn registration_client_reports_socket_path_on_connect_failure() {
        let dir = Utf8TempDir::new().expect("create temp dir");
        let path_str = dir.path().join("missing.sock");

        let status = RegistrationClient::new(path_str.clone())
            .await
            .expect_err("connect should fail for missing socket");

        assert_eq!(status.code(), tonic::Code::Unavailable);
        assert!(
            status
                .message()
                .contains("failed to connect to kubelet registration socket")
        );
        assert!(status.message().contains(path_str.as_str()));
    }

    #[tokio::test]
    async fn registration_client_surfaces_server_registration_error() {
        let server = start_mock_registration_server(Some((
            tonic::Code::FailedPrecondition,
            "registration rejected",
        )));

        let mut client = RegistrationClient::new(server.socket_path())
            .await
            .expect("connect registration client");

        let status = client
            .register("plugin.sock", "example.com/device")
            .await
            .expect_err("register should fail when server rejects");

        assert_eq!(status.code(), tonic::Code::FailedPrecondition);
        assert!(status.message().contains("registration rejected"));

        server.shutdown();
    }
}

#[cfg(test)]
mod timeout_tests {
    use camino_tempfile::Utf8TempDir;

    use super::*;

    struct HangingRegistration;
    #[tonic::async_trait]
    impl v1beta1::Registration for HangingRegistration {
        async fn register(
            &self,
            _: tonic::Request<v1beta1::RegisterRequest>,
        ) -> tonic::Result<tonic::Response<v1beta1::Empty>> {
            std::future::pending().await
        }
    }

    #[tokio::test]
    async fn registration_rpc_timeout_allows_retry_budget_to_exhaust() {
        let directory = Utf8TempDir::new().unwrap();
        let path = directory.path().join("registration.sock");
        let listener = net::UnixListener::bind(&path).unwrap();
        let server = tokio::spawn(
            transport::Server::builder()
                .add_service(v1beta1::RegistrationServer::new(HangingRegistration))
                .serve_with_incoming(UnixListenerStream::new(listener)),
        );
        let service = DevicePluginService::new(StaticDevicePlugin::new(vec![]));
        let plugin = DevicePlugin::for_test(
            "example.com/device",
            service,
            "unused.sock".into(),
            path.clone(),
        );
        let result = tokio::time::timeout(
            REGISTRATION_TIMEOUT * 2 + Duration::from_secs(2),
            plugin.try_register(&path, 2, Duration::ZERO),
        )
        .await;
        assert!(
            result
                .expect("RPC deadlines must bound both registration attempts")
                .is_err()
        );
        server.abort();
    }

    #[tokio::test]
    async fn registration_deadline_bounds_a_socket_that_never_speaks_http2() {
        let directory = Utf8TempDir::new().unwrap();
        let path = directory.path().join("registration.sock");
        let listener = net::UnixListener::bind(&path).unwrap();
        let accept = tokio::spawn(async move {
            let (_connection, _) = listener.accept().await.unwrap();
            std::future::pending::<()>().await;
        });
        let result = tokio::time::timeout(REGISTRATION_TIMEOUT + Duration::from_secs(1), async {
            let mut client = RegistrationClient::new(&path).await?;
            client.register("plugin.sock", "example.com/device").await
        })
        .await;
        assert!(
            result
                .expect("connection must have its own deadline")
                .is_err()
        );
        accept.abort();
    }
}
