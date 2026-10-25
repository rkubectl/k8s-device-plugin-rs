//! Liveness probing for node-local DRA plugin sockets.

use hyper_util::rt::TokioIo;
use tower::service_fn;

use super::*;

const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// A non-mutating liveness probe for one node-local DRA plugin instance.
///
/// [`Self::check`] verifies both serving sockets with real gRPC calls: it
/// invokes registration `GetInfo`, then `NodePrepareResources` with an empty
/// claim batch. The latter cannot prepare a device, so the check is safe to
/// run repeatedly from a Kubernetes `exec` liveness or readiness probe.
#[derive(Clone, Debug)]
pub struct DraPluginLivenessProbe {
    driver_name: String,
    registration_socket_path: Utf8PathBuf,
    plugin_socket_path: Utf8PathBuf,
}

impl DraPluginLivenessProbe {
    /// Creates a probe for the standard kubelet paths of `driver_name`.
    ///
    /// # Errors
    ///
    /// Returns a validation error for an invalid driver name.
    pub fn new(driver_name: &str) -> Result<Self, ValidationError> {
        let plugin_socket_path = Utf8Path::new(dra::KUBELET_PLUGINS_PATH)
            .join(driver_name)
            .join("plugin.sock");
        let registration_socket_path = Utf8Path::new(dra::KUBELET_PLUGINS_REGISTRY_PATH)
            .join(DraRegistrationServer::socket_name(driver_name));
        Self::with_socket_paths(driver_name, registration_socket_path, plugin_socket_path)
    }

    /// Creates a probe for explicit registration and plugin socket paths.
    ///
    /// This is useful when a deployment intentionally mounts kubelet
    /// directories at non-standard locations.
    ///
    /// # Errors
    ///
    /// Returns a validation error for an invalid expected driver name.
    pub fn with_socket_paths(
        driver_name: &str,
        registration_socket_path: impl Into<Utf8PathBuf>,
        plugin_socket_path: impl Into<Utf8PathBuf>,
    ) -> Result<Self, ValidationError> {
        validate_driver_name(driver_name)?;
        Ok(Self {
            driver_name: driver_name.to_string(),
            registration_socket_path: registration_socket_path.into(),
            plugin_socket_path: plugin_socket_path.into(),
        })
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
        let mut registration_client =
            dra::registration::RegistrationClient::new(registration_channel);
        let info = registration_client
            .get_info(tonic::Request::new(dra::registration::InfoRequest {}))
            .await
            .map_err(|error| rpc_error("registration GetInfo", error))?
            .into_inner();

        if info.r#type != "DRAPlugin" {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "registration socket {} reported plugin type {}, not DRAPlugin",
                    self.registration_socket_path, info.r#type
                ),
            ));
        }

        if info.name != self.driver_name
            || !info
                .supported_versions
                .iter()
                .any(|service| service == dra::DRA_PLUGIN_SERVICE)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "registration must advertise driver {} and service {}",
                    self.driver_name,
                    dra::DRA_PLUGIN_SERVICE
                ),
            ));
        }

        if Utf8Path::new(&info.endpoint) != self.plugin_socket_path {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "registration socket {} advertised {}, expected {}",
                    self.registration_socket_path, info.endpoint, self.plugin_socket_path
                ),
            ));
        }

        let plugin_channel = connect(&self.plugin_socket_path, "DRAPlugin").await?;
        let mut plugin_client = dra::v1::DraPluginClient::new(plugin_channel);
        plugin_client
            .node_prepare_resources(tonic::Request::new(dra::v1::NodePrepareResourcesRequest {
                claims: Vec::new(),
            }))
            .await
            .map_err(|error| rpc_error("DRAPlugin NodePrepareResources", error))?;
        Ok(())
    }
}

async fn connect(socket_path: &Utf8Path, service: &str) -> io::Result<transport::Channel> {
    let socket_path = socket_path.to_path_buf();
    let connector_path = socket_path.clone();
    let endpoint = transport::Endpoint::try_from("http://[::]:50051")
        .map_err(|error| io::Error::other(format!("build {service} probe endpoint: {error}")))?
        .connect_timeout(PROBE_TIMEOUT)
        .timeout(PROBE_TIMEOUT);

    endpoint
        .connect_with_connector(service_fn(move |_| {
            let socket_path = connector_path.clone();
            async move {
                net::UnixStream::connect(socket_path)
                    .await
                    .map(TokioIo::new)
            }
        }))
        .await
        .map_err(|error| {
            io::Error::other(format!(
                "connect to {service} socket {}: {error}",
                socket_path
            ))
        })
}

fn rpc_error(operation: &str, error: tonic::Status) -> io::Error {
    io::Error::other(format!("{operation} probe failed: {error}"))
}
