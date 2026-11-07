use super::*;

const REGISTRATION_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug)]
pub struct RegistrationClient {
    inner: v1beta1::RegistrationClient<transport::Channel>,
}

impl RegistrationClient {
    fn endpoint() -> tonic::Result<transport::Endpoint> {
        // The URI host is ignored for Unix Domain Socket transport.
        Ok(transport::Endpoint::try_from("http://[::]:50051")
            .map_err(|err| {
                tonic::Status::internal(format!(
                    "failed to build kubelet registration endpoint: {err}"
                ))
            })?
            .connect_timeout(REGISTRATION_TIMEOUT)
            .timeout(REGISTRATION_TIMEOUT))
    }

    pub async fn new(path: impl AsRef<Utf8Path>) -> tonic::Result<Self> {
        let socket_path = path.as_ref().to_path_buf();
        let connector_path = socket_path.clone();
        let endpoint = Self::endpoint()?;

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

    pub async fn register(
        &mut self,
        endpoint: &str,
        resource_name: &ExtendedResourceName,
    ) -> tonic::Result<()> {
        let request = v1beta1::RegisterRequest::new(endpoint, resource_name.as_str());
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
            .register("plugin.sock", &"example.com/device".parse().unwrap())
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
            .register("plugin.sock", &"example.com/device".parse().unwrap())
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
    use tokio::io::AsyncReadExt;
    use tokio::sync::mpsc;
    use tokio::sync::oneshot;
    use tokio::time;

    use super::*;

    struct HangingRegistration {
        started: mpsc::Sender<()>,
    }
    #[tonic::async_trait]
    impl v1beta1::Registration for HangingRegistration {
        async fn register(
            &self,
            _: tonic::Request<v1beta1::RegisterRequest>,
        ) -> tonic::Result<tonic::Response<v1beta1::Empty>> {
            self.started.send(()).await.unwrap();
            std::future::pending().await
        }
    }

    #[tokio::test]
    async fn registration_rpc_timeout_allows_retry_budget_to_exhaust() {
        let directory = Utf8TempDir::new().unwrap();
        let path = directory.path().join("registration.sock");
        let listener = net::UnixListener::bind(&path).unwrap();
        let (started, mut requests) = mpsc::channel(1);
        let server = task::spawn(
            transport::Server::builder()
                .add_service(v1beta1::RegistrationServer::new(HangingRegistration {
                    started,
                }))
                .serve_with_incoming(UnixListenerStream::new(listener)),
        );
        let service = DevicePluginService::new(StaticDevicePlugin::new(vec![]));
        let plugin = DevicePlugin::for_test(
            "example.com/device".parse().unwrap(),
            service,
            "unused.sock".into(),
            path.clone(),
        );
        let max_attempts = 2;
        let registration = task::spawn(async move {
            plugin
                .try_register(&path, max_attempts, Duration::ZERO)
                .await
        });
        for _ in 0..max_attempts {
            // Establish each real RPC before moving its deadline. Resume time
            // between attempts so auto-advance cannot outrun the next handshake.
            time::timeout(REGISTRATION_TIMEOUT, requests.recv())
                .await
                .expect("registration attempt must reach the server")
                .expect("registration server must remain available");
            time::pause();
            time::advance(REGISTRATION_TIMEOUT + Duration::from_millis(1)).await;
            time::resume();
        }
        let result = time::timeout(REGISTRATION_TIMEOUT, registration)
            .await
            .expect("RPC deadlines must exhaust the retry budget")
            .expect("registration task must complete");
        assert!(result.is_err());
        server.abort();
        let _ = server.await;
    }

    #[tokio::test(start_paused = true)]
    async fn registration_connect_timeout_bounds_a_stalled_connector() {
        let endpoint = RegistrationClient::endpoint().unwrap();
        let connector =
            service_fn(|_| std::future::pending::<io::Result<TokioIo<net::UnixStream>>>());
        let start = time::Instant::now();
        let result = time::timeout(
            REGISTRATION_TIMEOUT + Duration::from_secs(1),
            endpoint.connect_with_connector(connector),
        )
        .await
        .expect("the endpoint must bound a stalled connection attempt");
        assert!(result.is_err());
        assert!(start.elapsed() >= REGISTRATION_TIMEOUT);
    }

    #[tokio::test]
    async fn registration_deadline_bounds_a_socket_that_never_speaks_http2() {
        let directory = Utf8TempDir::new().unwrap();
        let path = directory.path().join("registration.sock");
        let listener = net::UnixListener::bind(&path).unwrap();
        let (ready, preface_received) = oneshot::channel();
        let accept = task::spawn(async move {
            let (mut connection, _) = listener.accept().await.unwrap();
            let mut preface = [0; 24];
            connection.read_exact(&mut preface).await.unwrap();
            assert_eq!(&preface, b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n");
            ready.send(()).unwrap();
            std::future::pending::<()>().await;
        });
        let registration = task::spawn(async move {
            let mut client = RegistrationClient::new(&path).await?;
            client
                .register("plugin.sock", &"example.com/device".parse().unwrap())
                .await
        });
        time::timeout(REGISTRATION_TIMEOUT, preface_received)
            .await
            .expect("client must send its HTTP/2 preface")
            .expect("peer must observe the HTTP/2 preface");
        // The peer deliberately never replies. Only the deadline can finish
        // registration; an outer virtual-time guard detects a missing deadline.
        time::pause();
        let result = time::timeout(REGISTRATION_TIMEOUT + Duration::from_secs(1), registration)
            .await
            .expect("registration must time out against a silent HTTP/2 peer")
            .expect("registration task must complete");
        assert!(result.is_err());
        accept.abort();
        let _ = accept.await;
    }
}
