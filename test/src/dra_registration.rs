use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

use hyper_util::rt::TokioIo;
use k8s_device_plugin_proto::dra::registration;
use tempfile::TempDir;
use tokio::net::UnixListener;
use tokio::net::UnixStream;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio_stream::wrappers::UnixListenerStream;
use tonic::transport;
use tonic::transport::Channel;
use tonic::transport::Endpoint;
use tonic::transport::Uri;
use tower::service_fn;

/// A mock kubelet pluginwatcher: connects to a `Registration` socket and
/// calls `GetInfo`/`NotifyRegistrationStatus`, playing the client role
/// kubelet plays for DRA's plugin-watcher registration model. This is the
/// inverse of [`crate::registration::MockRegistrationServer`] -- for the
/// classic device-plugin API the plugin is the client and the mock is a
/// server, but for DRA the plugin under test *is* the server and this mock
/// is the client.
#[derive(Debug)]
pub struct MockRegistrationClient {
    inner: registration::RegistrationClient<Channel>,
}

impl MockRegistrationClient {
    pub async fn connect(path: impl AsRef<Path>) -> tonic::Result<Self> {
        let socket_path = PathBuf::from(path.as_ref());
        let connector_path = socket_path.clone();

        let endpoint = Endpoint::try_from("http://[::]:50051").map_err(|err| {
            tonic::Status::internal(format!("failed to build registration endpoint: {err}"))
        })?;

        let channel = endpoint
            .connect_with_connector(service_fn(move |_: Uri| {
                let path = connector_path.clone();
                async move { UnixStream::connect(path).await.map(TokioIo::new) }
            }))
            .await
            .map_err(|err| {
                tonic::Status::unavailable(format!(
                    "failed to connect to registration socket {}: {err}",
                    socket_path.display()
                ))
            })?;

        Ok(Self {
            inner: registration::RegistrationClient::new(channel),
        })
    }

    pub async fn get_info(&mut self) -> tonic::Result<registration::PluginInfo> {
        self.inner
            .get_info(tonic::Request::new(registration::InfoRequest {}))
            .await
            .map(|r| r.into_inner())
    }

    pub async fn notify_registration_status(
        &mut self,
        plugin_registered: bool,
        error: &str,
    ) -> tonic::Result<()> {
        let request = registration::RegistrationStatus {
            plugin_registered,
            error: error.to_string(),
        };
        self.inner
            .notify_registration_status(tonic::Request::new(request))
            .await?;
        Ok(())
    }
}

/// A fake `Registration` server for exercising [`MockRegistrationClient`]
/// against, without depending on the `dra` crate's own
/// `DraRegistrationServer` implementation.
#[derive(Debug, Default)]
pub struct FakeRegistration {
    pub plugin_info: registration::PluginInfo,
    pub notifications: Arc<Mutex<Vec<registration::RegistrationStatus>>>,
}

#[tonic::async_trait]
impl registration::Registration for FakeRegistration {
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
        self.notifications.lock().await.push(request.into_inner());
        Ok(tonic::Response::new(
            registration::RegistrationStatusResponse {},
        ))
    }
}

#[derive(Debug)]
pub struct MockRegistrationServer {
    // Kept alive so the temp dir (and its socket) are removed on drop.
    _socket_dir: TempDir,
    socket_path: PathBuf,
    notifications: Arc<Mutex<Vec<registration::RegistrationStatus>>>,
    server_handle: JoinHandle<Result<(), transport::Error>>,
}

impl MockRegistrationServer {
    pub fn socket_path(&self) -> String {
        self.socket_path.to_string_lossy().into_owned()
    }

    pub async fn collected_notifications(&self) -> Vec<registration::RegistrationStatus> {
        self.notifications.lock().await.clone()
    }

    pub fn shutdown(self) {
        self.server_handle.abort();
    }
}

pub fn start_mock_registration_server(
    plugin_info: registration::PluginInfo,
) -> MockRegistrationServer {
    let fake = FakeRegistration {
        plugin_info,
        ..Default::default()
    };

    let socket_dir = TempDir::new().expect("create temp dir for registration socket");
    let socket_path = socket_dir.path().join("plugin-reg.sock");
    let notifications = Arc::clone(&fake.notifications);

    let listener = UnixListener::bind(&socket_path).expect("bind unix socket");
    let incoming = UnixListenerStream::new(listener);
    let server =
        transport::Server::builder().add_service(registration::RegistrationServer::new(fake));
    let server_handle = tokio::spawn(server.serve_with_incoming(incoming));

    MockRegistrationServer {
        _socket_dir: socket_dir,
        socket_path,
        notifications,
        server_handle,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn get_info_returns_the_configured_plugin_info() {
        let plugin_info = registration::PluginInfo {
            r#type: "DRAPlugin".to_string(),
            name: "example.com/widget".to_string(),
            endpoint: "plugin.sock".to_string(),
            supported_versions: vec!["v1".to_string()],
        };
        let server = start_mock_registration_server(plugin_info.clone());

        let mut client = MockRegistrationClient::connect(server.socket_path())
            .await
            .expect("connect to registration socket");
        let info = client.get_info().await.expect("get_info call");

        assert_eq!(info, plugin_info);
        server.shutdown();
    }

    #[tokio::test]
    async fn notify_registration_status_is_collected() {
        let server = start_mock_registration_server(registration::PluginInfo::default());

        let mut client = MockRegistrationClient::connect(server.socket_path())
            .await
            .expect("connect to registration socket");
        client
            .notify_registration_status(true, "")
            .await
            .expect("notify call");

        let notifications = server.collected_notifications().await;
        assert_eq!(notifications.len(), 1);
        assert!(notifications[0].plugin_registered);
        server.shutdown();
    }

    #[tokio::test]
    async fn connect_reports_socket_path_on_failure() {
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("missing.sock");

        let status = MockRegistrationClient::connect(&path)
            .await
            .expect_err("connect should fail for missing socket");

        assert_eq!(status.code(), tonic::Code::Unavailable);
        assert!(
            status
                .message()
                .contains(&path.to_string_lossy().into_owned())
        );
    }
}
