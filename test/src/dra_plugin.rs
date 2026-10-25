use k8s_device_plugin_proto::dra::v1 as drav1;

use super::*;

/// A mock kubelet DRA manager: connects to a `DRAPlugin` socket and calls
/// `NodePrepareResources`/`NodeUnprepareResources`, mirroring
/// [`crate::device_plugin::MockDevicePluginClient`] for the classic plugin.
#[derive(Debug)]
pub struct MockDraPluginClient {
    inner: drav1::DraPluginClient<transport::Channel>,
}

impl MockDraPluginClient {
    /// Connect to a `DRAPlugin` Unix socket at `path`, mirroring the kubelet side.
    pub async fn connect(path: impl AsRef<Utf8Path>) -> tonic::Result<Self> {
        let socket_path = Utf8PathBuf::from(path.as_ref());
        let connector_path = socket_path.clone();

        let endpoint = transport::Endpoint::try_from("http://[::]:50051").map_err(|err| {
            tonic::Status::internal(format!("failed to build DRA plugin endpoint: {err}"))
        })?;

        let channel = endpoint
            .connect_with_connector(service_fn(move |_| {
                let path = connector_path.clone();
                async move { net::UnixStream::connect(path).await.map(TokioIo::new) }
            }))
            .await
            .map_err(|err| {
                tonic::Status::unavailable(format!(
                    "failed to connect to DRA plugin socket {}: {err}",
                    socket_path
                ))
            })?;

        Ok(Self {
            inner: drav1::DraPluginClient::new(channel),
        })
    }

    pub async fn prepare_resources(
        &mut self,
        claims: Vec<drav1::Claim>,
    ) -> tonic::Result<drav1::NodePrepareResourcesResponse> {
        let request = drav1::NodePrepareResourcesRequest { claims };
        self.inner
            .node_prepare_resources(tonic::Request::new(request))
            .await
            .map(|r| r.into_inner())
    }

    pub async fn unprepare_resources(
        &mut self,
        claims: Vec<drav1::Claim>,
    ) -> tonic::Result<drav1::NodeUnprepareResourcesResponse> {
        let request = drav1::NodeUnprepareResourcesRequest { claims };
        self.inner
            .node_unprepare_resources(tonic::Request::new(request))
            .await
            .map(|r| r.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    #[derive(Debug, Default)]
    struct FakeDraPlugin;

    #[tonic::async_trait]
    impl drav1::DraPlugin for FakeDraPlugin {
        async fn node_prepare_resources(
            &self,
            request: tonic::Request<drav1::NodePrepareResourcesRequest>,
        ) -> tonic::Result<tonic::Response<drav1::NodePrepareResourcesResponse>> {
            let claims = request
                .into_inner()
                .claims
                .into_iter()
                .map(|claim| {
                    (
                        claim.uid,
                        drav1::NodePrepareResourceResponse {
                            devices: vec![drav1::Device {
                                request_names: vec![],
                                pool_name: "node-0".to_string(),
                                device_name: "widget-0".to_string(),
                                cdi_device_ids: vec![],
                                share_id: None,
                            }],
                            error: String::new(),
                        },
                    )
                })
                .collect::<HashMap<_, _>>();
            Ok(tonic::Response::new(drav1::NodePrepareResourcesResponse {
                claims,
            }))
        }

        async fn node_unprepare_resources(
            &self,
            request: tonic::Request<drav1::NodeUnprepareResourcesRequest>,
        ) -> tonic::Result<tonic::Response<drav1::NodeUnprepareResourcesResponse>> {
            let claims = request
                .into_inner()
                .claims
                .into_iter()
                .map(|claim| {
                    (
                        claim.uid,
                        drav1::NodeUnprepareResourceResponse {
                            error: String::new(),
                        },
                    )
                })
                .collect::<HashMap<_, _>>();
            Ok(tonic::Response::new(
                drav1::NodeUnprepareResourcesResponse { claims },
            ))
        }
    }

    fn make_claim(uid: &str) -> drav1::Claim {
        drav1::Claim {
            namespace: "default".to_string(),
            uid: uid.to_string(),
            name: uid.to_string(),
        }
    }

    fn start_fake_plugin() -> (Utf8PathBuf, Utf8TempDir, task::JoinHandle<()>) {
        let socket_dir = Utf8TempDir::new().expect("create temp dir for plugin socket");
        let socket_path = socket_dir.path().join("plugin.sock");
        let listener = net::UnixListener::bind(&socket_path).expect("bind unix socket");
        let incoming = UnixListenerStream::new(listener);
        let server =
            transport::Server::builder().add_service(drav1::DraPluginServer::new(FakeDraPlugin));
        let handle = tokio::spawn(async move {
            let _ = server.serve_with_incoming(incoming).await;
        });
        (socket_path, socket_dir, handle)
    }

    #[tokio::test]
    async fn prepare_resources_returns_an_entry_per_claim() {
        let (socket_path, _socket_dir, handle) = start_fake_plugin();
        let mut client = MockDraPluginClient::connect(&socket_path)
            .await
            .expect("connect to plugin socket");

        let response = client
            .prepare_resources(vec![make_claim("claim-a"), make_claim("claim-b")])
            .await
            .expect("prepare_resources call");

        assert_eq!(response.claims.len(), 2);
        assert!(response.claims.contains_key("claim-a"));
        assert!(response.claims.contains_key("claim-b"));
        handle.abort();
    }

    #[tokio::test]
    async fn unprepare_resources_returns_an_entry_per_claim() {
        let (socket_path, _socket_dir, handle) = start_fake_plugin();
        let mut client = MockDraPluginClient::connect(&socket_path)
            .await
            .expect("connect to plugin socket");

        let response = client
            .unprepare_resources(vec![make_claim("claim-a")])
            .await
            .expect("unprepare_resources call");

        assert_eq!(response.claims.len(), 1);
        assert!(response.claims["claim-a"].error.is_empty());
        handle.abort();
    }
}
