//! Liveness probing for node-local DRA plugin sockets.

use std::io;
use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;

use hyper_util::rt::TokioIo;
use k8s_device_plugin_proto::dra::KUBELET_PLUGINS_PATH;
use k8s_device_plugin_proto::dra::KUBELET_PLUGINS_REGISTRY_PATH;
use k8s_device_plugin_proto::dra::registration;
use k8s_device_plugin_proto::dra::v1;
use tokio::net::UnixStream;
use tonic::transport::Channel;
use tonic::transport::Endpoint;
use tonic::transport::Uri;
use tower::service_fn;

use crate::DraRegistrationServer;

const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// A non-mutating liveness probe for one node-local DRA plugin instance.
///
/// [`Self::check`] verifies both serving sockets with real gRPC calls: it
/// invokes registration `GetInfo`, then `NodePrepareResources` with an empty
/// claim batch. The latter cannot prepare a device, so the check is safe to
/// run repeatedly from a Kubernetes `exec` liveness or readiness probe.
#[derive(Clone, Debug)]
pub struct DraPluginLivenessProbe {
    registration_socket_path: PathBuf,
    plugin_socket_path: PathBuf,
}

impl DraPluginLivenessProbe {
    /// Creates a probe for the standard kubelet paths of `driver_name`.
    #[must_use]
    pub fn new(driver_name: &str) -> Self {
        let plugin_socket_path = Path::new(KUBELET_PLUGINS_PATH)
            .join(driver_name)
            .join("plugin.sock");
        let registration_socket_path = Path::new(KUBELET_PLUGINS_REGISTRY_PATH)
            .join(DraRegistrationServer::socket_name(driver_name));
        Self::with_socket_paths(registration_socket_path, plugin_socket_path)
    }

    /// Creates a probe for explicit registration and plugin socket paths.
    ///
    /// This is useful when a deployment intentionally mounts kubelet
    /// directories at non-standard locations.
    #[must_use]
    pub fn with_socket_paths(
        registration_socket_path: impl Into<PathBuf>,
        plugin_socket_path: impl Into<PathBuf>,
    ) -> Self {
        Self {
            registration_socket_path: registration_socket_path.into(),
            plugin_socket_path: plugin_socket_path.into(),
        }
    }

    /// Verifies that both DRA gRPC services respond without mutating state.
    ///
    /// # Errors
    ///
    /// Returns an error when either Unix socket cannot be reached before the
    /// probe timeout, when registration does not identify a DRA plugin, when
    /// it advertises a different plugin endpoint, or when either gRPC call
    /// fails.
    pub async fn check(&self) -> io::Result<()> {
        let registration_channel = connect(&self.registration_socket_path, "registration").await?;
        let mut registration_client = registration::RegistrationClient::new(registration_channel);
        let info = registration_client
            .get_info(tonic::Request::new(registration::InfoRequest {}))
            .await
            .map_err(|error| rpc_error("registration GetInfo", error))?
            .into_inner();

        if info.r#type != "DRAPlugin" {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "registration socket {} reported plugin type {:?}, not DRAPlugin",
                    self.registration_socket_path.display(),
                    info.r#type
                ),
            ));
        }

        if Path::new(&info.endpoint) != self.plugin_socket_path {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "registration socket {} advertised {}, expected {}",
                    self.registration_socket_path.display(),
                    info.endpoint,
                    self.plugin_socket_path.display()
                ),
            ));
        }

        let plugin_channel = connect(&self.plugin_socket_path, "DRAPlugin").await?;
        let mut plugin_client = v1::DraPluginClient::new(plugin_channel);
        plugin_client
            .node_prepare_resources(tonic::Request::new(v1::NodePrepareResourcesRequest {
                claims: Vec::new(),
            }))
            .await
            .map_err(|error| rpc_error("DRAPlugin NodePrepareResources", error))?;
        Ok(())
    }
}

async fn connect(socket_path: &Path, service: &str) -> io::Result<Channel> {
    let socket_path = socket_path.to_path_buf();
    let connector_path = socket_path.clone();
    let endpoint = Endpoint::try_from("http://[::]:50051")
        .map_err(|error| io::Error::other(format!("build {service} probe endpoint: {error}")))?
        .connect_timeout(PROBE_TIMEOUT)
        .timeout(PROBE_TIMEOUT);

    endpoint
        .connect_with_connector(service_fn(move |_: Uri| {
            let socket_path = connector_path.clone();
            async move { UnixStream::connect(socket_path).await.map(TokioIo::new) }
        }))
        .await
        .map_err(|error| {
            io::Error::other(format!(
                "connect to {service} socket {}: {error}",
                socket_path.display()
            ))
        })
}

fn rpc_error(operation: &str, error: tonic::Status) -> io::Error {
    io::Error::other(format!("{operation} probe failed: {error}"))
}
