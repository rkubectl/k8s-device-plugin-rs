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

use tokio_stream::wrappers::UnixListenerStream;

use super::*;

const REGISTRATION_SOCKET_SUFFIX: &str = "-reg.sock";

#[derive(Debug)]
pub struct DraRegistrationServer {
    socket_path: Utf8PathBuf,
    plugin_info: dra::registration::PluginInfo,
}

impl DraRegistrationServer {
    #[cfg(test)]
    pub(crate) fn set_socket_path(&mut self, path: Utf8PathBuf) {
        self.socket_path = path;
    }

    /// Advertises health reporting. The caller must serve the health handler
    /// at this registration's plugin endpoint.
    #[cfg(feature = "resource-health")]
    #[must_use]
    pub fn with_resource_health(mut self) -> Self {
        self.plugin_info
            .supported_versions
            .push(dra::health::v1alpha1::SERVICE_NAME.to_string());
        self
    }

    /// `plugin_endpoint` is the socket path of this driver's `DRAPlugin`
    /// gRPC service (see the plugin-service task) -- passed in rather than
    /// computed here, so this type stays decoupled from that one.
    ///
    /// # Errors
    ///
    /// Returns a validation error for an invalid driver name.
    pub fn new(
        driver_name: &str,
        plugin_endpoint: impl AsRef<Utf8Path>,
    ) -> Result<Self, ValidationError> {
        validate_driver_name(driver_name)?;
        let socket_name = Self::socket_name(driver_name);
        let socket_path = Utf8Path::new(dra::KUBELET_PLUGINS_REGISTRY_PATH).join(socket_name);
        let plugin_info = dra::registration::PluginInfo {
            r#type: "DRAPlugin".to_string(),
            name: driver_name.to_string(),
            endpoint: plugin_endpoint.as_ref().as_str().to_owned(),
            supported_versions: vec![dra::DRA_PLUGIN_SERVICE.to_string()],
        };
        Ok(Self {
            socket_path,
            plugin_info,
        })
    }

    #[cfg(test)]
    pub(crate) fn for_test(
        driver_name: &str,
        plugin_endpoint: impl AsRef<Utf8Path>,
        socket_path: Utf8PathBuf,
    ) -> Self {
        let mut server = Self::new(driver_name, plugin_endpoint).expect("valid test driver name");
        server.set_socket_path(socket_path);
        server
    }

    pub(crate) fn socket_name(driver_name: &str) -> String {
        let budget = k8s_device_plugin_core::MAX_SOCKET_PATH_LEN
            .saturating_sub(dra::KUBELET_PLUGINS_REGISTRY_PATH.len())
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
    pub fn socket_path(&self) -> &Utf8Path {
        &self.socket_path
    }

    /// Binds the registration socket and spawns the gRPC server.
    pub async fn spawn(&self) -> io::Result<task::JoinHandle<Result<(), transport::Error>>> {
        let incoming = self.setup_listener().await?;
        let service = RegistrationService {
            plugin_info: self.plugin_info.clone(),
        };
        let router = transport::Server::builder()
            .add_service(dra::registration::RegistrationServer::new(service));
        Ok(tokio::spawn(router.serve_with_incoming(incoming)))
    }

    async fn setup_listener(&self) -> io::Result<UnixListenerStream> {
        let socket_path = self.socket_path.clone();
        let listener =
            task::spawn_blocking(move || k8s_device_plugin_core::bind_unix_listener(&socket_path))
                .await
                .map_err(io::Error::other)??;
        net::UnixListener::from_std(listener).map(UnixListenerStream::new)
    }
}

#[derive(Debug)]
struct RegistrationService {
    plugin_info: dra::registration::PluginInfo,
}

#[tonic::async_trait]
impl dra::registration::Registration for RegistrationService {
    async fn get_info(
        &self,
        _request: tonic::Request<dra::registration::InfoRequest>,
    ) -> tonic::Result<tonic::Response<dra::registration::PluginInfo>> {
        Ok(tonic::Response::new(self.plugin_info.clone()))
    }

    async fn notify_registration_status(
        &self,
        request: tonic::Request<dra::registration::RegistrationStatus>,
    ) -> tonic::Result<tonic::Response<dra::registration::RegistrationStatusResponse>> {
        let status = request.into_inner();
        if status.plugin_registered {
            tracing::info!(
                driver_name = self.plugin_info.name,
                "registered with kubelet"
            );
        } else {
            tracing::error!(
                driver_name = self.plugin_info.name,
                status.error,
                "kubelet rejected registration"
            );
        }
        Ok(tonic::Response::new(
            dra::registration::RegistrationStatusResponse {},
        ))
    }
}

#[cfg(test)]
mod tests;
