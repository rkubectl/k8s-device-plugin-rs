use std::path::Path;
use std::path::PathBuf;

use hyper_util::rt::TokioIo;
use k8s_device_plugin_proto::dra::v1;
use tokio::net::UnixStream;
use tonic::transport::Channel;
use tonic::transport::Endpoint;
use tonic::transport::Uri;
use tower::service_fn;

/// A mock kubelet DRA manager: connects to a `DRAPlugin` socket and calls
/// `NodePrepareResources`/`NodeUnprepareResources`, mirroring
/// [`crate::device_plugin::MockDevicePluginClient`] for the classic plugin.
#[derive(Debug)]
pub struct MockDraPluginClient {
    inner: v1::DraPluginClient<Channel>,
}

impl MockDraPluginClient {
    /// Connect to a `DRAPlugin` Unix socket at `path`, mirroring the kubelet side.
    pub async fn connect(path: impl AsRef<Path>) -> tonic::Result<Self> {
        let socket_path = PathBuf::from(path.as_ref());
        let connector_path = socket_path.clone();

        let endpoint = Endpoint::try_from("http://[::]:50051").map_err(|err| {
            tonic::Status::internal(format!("failed to build DRA plugin endpoint: {err}"))
        })?;

        let channel = endpoint
            .connect_with_connector(service_fn(move |_: Uri| {
                let path = connector_path.clone();
                async move { UnixStream::connect(path).await.map(TokioIo::new) }
            }))
            .await
            .map_err(|err| {
                tonic::Status::unavailable(format!(
                    "failed to connect to DRA plugin socket {}: {err}",
                    socket_path.display()
                ))
            })?;

        Ok(Self {
            inner: v1::DraPluginClient::new(channel),
        })
    }

    pub async fn prepare_resources(
        &mut self,
        claims: Vec<v1::Claim>,
    ) -> tonic::Result<v1::NodePrepareResourcesResponse> {
        let request = v1::NodePrepareResourcesRequest { claims };
        self.inner
            .node_prepare_resources(tonic::Request::new(request))
            .await
            .map(|r| r.into_inner())
    }

    pub async fn unprepare_resources(
        &mut self,
        claims: Vec<v1::Claim>,
    ) -> tonic::Result<v1::NodeUnprepareResourcesResponse> {
        let request = v1::NodeUnprepareResourcesRequest { claims };
        self.inner
            .node_unprepare_resources(tonic::Request::new(request))
            .await
            .map(|r| r.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::PathBuf;

    use tempfile::TempDir;
    use tokio_stream::wrappers::UnixListenerStream;
    use tonic::transport;

    use super::*;

    #[derive(Debug, Default)]
    struct FakeDraPlugin;

    #[tonic::async_trait]
    impl v1::DraPlugin for FakeDraPlugin {
        async fn node_prepare_resources(
            &self,
            request: tonic::Request<v1::NodePrepareResourcesRequest>,
        ) -> tonic::Result<tonic::Response<v1::NodePrepareResourcesResponse>> {
            let claims = request
                .into_inner()
                .claims
                .into_iter()
                .map(|claim| {
                    (
                        claim.uid,
                        v1::NodePrepareResourceResponse {
                            devices: vec![v1::Device {
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
            Ok(tonic::Response::new(v1::NodePrepareResourcesResponse {
                claims,
            }))
        }

        async fn node_unprepare_resources(
            &self,
            request: tonic::Request<v1::NodeUnprepareResourcesRequest>,
        ) -> tonic::Result<tonic::Response<v1::NodeUnprepareResourcesResponse>> {
            let claims = request
                .into_inner()
                .claims
                .into_iter()
                .map(|claim| {
                    (
                        claim.uid,
                        v1::NodeUnprepareResourceResponse {
                            error: String::new(),
                        },
                    )
                })
                .collect::<HashMap<_, _>>();
            Ok(tonic::Response::new(v1::NodeUnprepareResourcesResponse {
                claims,
            }))
        }
    }

    fn make_claim(uid: &str) -> v1::Claim {
        v1::Claim {
            namespace: "default".to_string(),
            uid: uid.to_string(),
            name: uid.to_string(),
        }
    }

    fn start_fake_plugin() -> (PathBuf, TempDir, tokio::task::JoinHandle<()>) {
        let socket_dir = TempDir::new().expect("create temp dir for plugin socket");
        let socket_path = socket_dir.path().join("plugin.sock");
        let listener = tokio::net::UnixListener::bind(&socket_path).expect("bind unix socket");
        let incoming = UnixListenerStream::new(listener);
        let server =
            transport::Server::builder().add_service(v1::DraPluginServer::new(FakeDraPlugin));
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
