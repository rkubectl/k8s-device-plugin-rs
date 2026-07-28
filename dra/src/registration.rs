//! Pluginwatcher-based `Registration` server — see beads issue 9uf.5.
//!
//! DRA inverts the registration relationship the classic device-plugin API
//! uses: instead of the plugin calling kubelet's `Registration.Register`
//! (see [`k8s_device_plugin_lib::RegistrationClient`] -- not a dependency of
//! this crate, referenced here only for orientation), a DRA driver *serves*
//! `pluginregistration.v1.Registration` and kubelet's pluginwatcher connects
//! to it. Consequently this server does not retry or re-register itself:
//! kubelet's pluginwatcher re-scans `/var/lib/kubelet/plugins_registry/` and
//! reconnects on its own, so this type's only job is to keep the socket
//! bound and answer whenever asked.

use std::io;
use std::path::Path;
use std::path::PathBuf;

use k8s_device_plugin_proto::dra::KUBELET_PLUGINS_REGISTRY_PATH;
use k8s_device_plugin_proto::dra::registration;
use tokio::net::UnixListener;
use tokio::task::JoinHandle;
use tokio_stream::wrappers::UnixListenerStream;
use tonic::transport;

const REGISTRATION_SOCKET_SUFFIX: &str = "-reg.sock";

#[derive(Debug)]
pub struct DraRegistrationServer {
    socket_path: PathBuf,
    plugin_info: registration::PluginInfo,
}

impl DraRegistrationServer {
    /// `plugin_endpoint` is the socket path of this driver's `DRAPlugin`
    /// gRPC service (see the plugin-service task) -- passed in rather than
    /// computed here, so this type stays decoupled from that one.
    pub fn new(driver_name: &str, plugin_endpoint: &str) -> Self {
        let socket_name = Self::socket_name(driver_name);
        let socket_path = Path::new(KUBELET_PLUGINS_REGISTRY_PATH).join(socket_name);
        let plugin_info = registration::PluginInfo {
            r#type: "DRAPlugin".to_string(),
            name: driver_name.to_string(),
            endpoint: plugin_endpoint.to_string(),
            supported_versions: vec!["v1".to_string()],
        };
        Self {
            socket_path,
            plugin_info,
        }
    }

    #[cfg(test)]
    fn for_test(driver_name: &str, plugin_endpoint: &str, socket_path: PathBuf) -> Self {
        let plugin_info = registration::PluginInfo {
            r#type: "DRAPlugin".to_string(),
            name: driver_name.to_string(),
            endpoint: plugin_endpoint.to_string(),
            supported_versions: vec!["v1".to_string()],
        };
        Self {
            socket_path,
            plugin_info,
        }
    }

    fn socket_name(driver_name: &str) -> String {
        let budget = k8s_device_plugin_core::MAX_SOCKET_PATH_LEN
            .saturating_sub(KUBELET_PLUGINS_REGISTRY_PATH.len())
            .saturating_sub(REGISTRATION_SOCKET_SUFFIX.len());
        k8s_device_plugin_core::sanitize_socket_name(driver_name, budget)
            + REGISTRATION_SOCKET_SUFFIX
    }

    /// The registration socket's path, exposed so a caller that owns process
    /// shutdown (e.g. the eventual `DraPlugin::run()` lifecycle harness) can
    /// remove the file once this server is no longer serving. This type
    /// deliberately does not clean up after itself: kubelet's pluginwatcher
    /// treats a dead socket as the plugin having gone away, so cleanup is an
    /// orchestration concern for whoever manages this server's lifetime, not
    /// something to build speculatively here.
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    /// Binds the registration socket and spawns the gRPC server.
    pub async fn spawn(&self) -> io::Result<JoinHandle<Result<(), transport::Error>>> {
        let incoming = self.setup_listener().await?;
        let service = RegistrationService {
            plugin_info: self.plugin_info.clone(),
        };
        let router = transport::Server::builder()
            .add_service(registration::RegistrationServer::new(service));
        Ok(tokio::spawn(router.serve_with_incoming(incoming)))
    }

    async fn setup_listener(&self) -> io::Result<UnixListenerStream> {
        if tokio::fs::try_exists(&self.socket_path).await? {
            tokio::fs::remove_file(&self.socket_path).await?;
        }
        UnixListener::bind(&self.socket_path).map(UnixListenerStream::new)
    }
}

#[derive(Debug)]
struct RegistrationService {
    plugin_info: registration::PluginInfo,
}

#[tonic::async_trait]
impl registration::Registration for RegistrationService {
    async fn get_info(
        &self,
        _request: tonic::Request<registration::InfoRequest>,
    ) -> tonic::Result<tonic::Response<registration::PluginInfo>> {
        Ok(tonic::Response::new(self.plugin_info.clone()))
    }

    async fn notify_registration_status(
        &self,
        request: tonic::Request<registration::RegistrationStatus>,
    ) -> tonic::Result<tonic::Response<registration::RegistrationStatusResponse>> {
        let status = request.into_inner();
        if status.plugin_registered {
            tracing::info!(driver_name = %self.plugin_info.name, "registered with kubelet");
        } else {
            tracing::error!(
                driver_name = %self.plugin_info.name,
                error = %status.error,
                "kubelet rejected registration"
            );
        }
        Ok(tonic::Response::new(
            registration::RegistrationStatusResponse {},
        ))
    }
}

#[cfg(test)]
mod tests;
