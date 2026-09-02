//! Optional `DRAResourceHealth.NodeWatchResources` service.

use std::fmt;
use std::sync::Arc;

use k8s_device_plugin_core::DraDeviceHealth;
use k8s_device_plugin_core::ResourceHealthError;
use k8s_device_plugin_core::ResourceHealthReport;
use k8s_device_plugin_core::ResourceHealthReporter;
use k8s_device_plugin_core::ResourceHealthStatus;
use k8s_device_plugin_proto::dra::health::v1alpha1;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tonic::Code;
use tonic::Status;

const REPORT_BUFFER_CAPACITY: usize = 16;

/// Adapter between a backend's [`ResourceHealthReporter`] and kubelet's
/// optional `DRAResourceHealth` protocol.
#[derive(Clone)]
pub struct DraResourceHealthService {
    reporter: Option<Arc<dyn ResourceHealthReporter>>,
}

impl fmt::Debug for DraResourceHealthService {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DraResourceHealthService")
            .finish_non_exhaustive()
    }
}

impl DraResourceHealthService {
    /// Creates a health service for one DRA backend.
    pub fn new<R: ResourceHealthReporter + 'static>(reporter: R) -> Self {
        Self {
            reporter: Some(Arc::new(reporter)),
        }
    }

    /// Creates a protocol service that explicitly declines health reporting.
    /// This is used when the Cargo feature is enabled but a backend has not
    /// opted in; kubelet then stops opening health watches for that driver.
    pub fn unsupported() -> Self {
        Self { reporter: None }
    }

    fn watch_stream(
        &self,
    ) -> Result<ReceiverStream<tonic::Result<v1alpha1::NodeWatchResourcesResponse>>, Status> {
        let Some(reporter) = self.reporter.as_ref() else {
            return Err(Status::unimplemented(
                "resource health reporting is not supported by this backend",
            ));
        };
        let (reports_tx, mut reports_rx) = mpsc::channel(REPORT_BUFFER_CAPACITY);
        let (responses_tx, responses_rx) = mpsc::channel(REPORT_BUFFER_CAPACITY);
        let reporter = Arc::clone(reporter);

        tokio::spawn(async move {
            let mut watch =
                tokio::spawn(async move { reporter.watch_resource_health(reports_tx).await });

            loop {
                tokio::select! {
                    report = reports_rx.recv() => match report {
                        Some(report) => {
                            if responses_tx.send(Ok(report_to_wire(report))).await.is_err() {
                                watch.abort();
                                let _ = watch.await;
                                return;
                            }
                        }
                        None => {
                            send_watch_completion(&responses_tx, watch.await).await;
                            return;
                        }
                    },
                    result = &mut watch => {
                        // The reporter may have sent its final snapshot just
                        // before returning. Preserve every buffered report
                        // before signalling that kubelet should reconnect.
                        while let Ok(report) = reports_rx.try_recv() {
                            if responses_tx.send(Ok(report_to_wire(report))).await.is_err() {
                                return;
                            }
                        }
                        send_watch_completion(&responses_tx, result).await;
                        return;
                    }
                }
            }
        });

        Ok(ReceiverStream::new(responses_rx))
    }
}

fn report_to_wire(report: ResourceHealthReport) -> v1alpha1::NodeWatchResourcesResponse {
    v1alpha1::NodeWatchResourcesResponse {
        devices: report.devices.into_iter().map(device_to_wire).collect(),
    }
}

fn device_to_wire(device: DraDeviceHealth) -> v1alpha1::DeviceHealth {
    let health = match device.health {
        ResourceHealthStatus::Unknown => v1alpha1::HealthStatus::Unknown,
        ResourceHealthStatus::Healthy => v1alpha1::HealthStatus::Healthy,
        ResourceHealthStatus::Unhealthy => v1alpha1::HealthStatus::Unhealthy,
    };
    v1alpha1::DeviceHealth {
        device: Some(v1alpha1::DeviceIdentifier {
            pool_name: device.pool_name,
            device_name: device.device_name,
        }),
        health: health.into(),
        last_updated_time: device.last_updated_time,
        health_check_timeout_seconds: device.health_check_timeout_seconds,
        message: device.message,
    }
}

async fn send_watch_completion(
    responses: &mpsc::Sender<tonic::Result<v1alpha1::NodeWatchResourcesResponse>>,
    result: Result<Result<(), ResourceHealthError>, tokio::task::JoinError>,
) {
    let message = match result {
        Ok(Ok(())) => "resource health reporter stopped".to_string(),
        Ok(Err(err)) => err.to_string(),
        Err(err) => format!("resource health reporter panicked: {err}"),
    };
    let _ = responses
        .send(Err(Status::new(Code::Unavailable, message)))
        .await;
}

#[tonic::async_trait]
impl v1alpha1::DraResourceHealth for DraResourceHealthService {
    type NodeWatchResourcesStream =
        ReceiverStream<tonic::Result<v1alpha1::NodeWatchResourcesResponse>>;

    async fn node_watch_resources(
        &self,
        _request: tonic::Request<v1alpha1::NodeWatchResourcesRequest>,
    ) -> tonic::Result<tonic::Response<Self::NodeWatchResourcesStream>> {
        Ok(tonic::Response::new(self.watch_stream()?))
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::Arc;
    use std::sync::atomic::AtomicUsize;
    use std::sync::atomic::Ordering;

    use hyper_util::rt::TokioIo;
    use tempfile::TempDir;
    use tokio::net::UnixStream;
    use tokio_stream::wrappers::UnixListenerStream;
    use tonic::transport::Channel;
    use tonic::transport::Endpoint;
    use tonic::transport::Uri;
    use tower::service_fn;

    use super::*;

    #[derive(Debug, Default)]
    struct ReconnectingReporter {
        sessions: AtomicUsize,
    }

    #[tonic::async_trait]
    impl ResourceHealthReporter for ReconnectingReporter {
        async fn watch_resource_health(
            &self,
            reports: mpsc::Sender<ResourceHealthReport>,
        ) -> Result<(), ResourceHealthError> {
            let session = self.sessions.fetch_add(1, Ordering::SeqCst);
            let health = if session == 0 {
                ResourceHealthStatus::Healthy
            } else {
                ResourceHealthStatus::Unhealthy
            };
            reports
                .send(ResourceHealthReport {
                    devices: vec![DraDeviceHealth {
                        pool_name: "pool-0".to_string(),
                        device_name: "widget-0".to_string(),
                        health,
                        last_updated_time: 1_700_000_000,
                        health_check_timeout_seconds: 30,
                        message: "monitor update".to_string(),
                    }],
                })
                .await
                .map_err(|err| ResourceHealthError::MonitorFailed(err.to_string()))?;
            Ok(())
        }
    }

    async fn connect(
        socket_path: &Path,
    ) -> tonic::Result<v1alpha1::DraResourceHealthClient<Channel>> {
        let socket_path = socket_path.to_path_buf();
        let connector_path = socket_path.clone();
        let endpoint = Endpoint::try_from("http://[::]:50051")
            .map_err(|err| Status::internal(format!("build resource-health endpoint: {err}")))?;
        let channel = endpoint
            .connect_with_connector(service_fn(move |_: Uri| {
                let path = connector_path.clone();
                async move { UnixStream::connect(path).await.map(TokioIo::new) }
            }))
            .await
            .map_err(|err| {
                Status::unavailable(format!(
                    "failed to connect to resource-health socket {}: {err}",
                    socket_path.display()
                ))
            })?;
        Ok(v1alpha1::DraResourceHealthClient::new(channel))
    }

    fn start_server(
        service: DraResourceHealthService,
    ) -> (std::path::PathBuf, TempDir, tokio::task::JoinHandle<()>) {
        let socket_dir = TempDir::new().expect("create resource-health socket directory");
        let socket_path = socket_dir.path().join("plugin.sock");
        let listener =
            tokio::net::UnixListener::bind(&socket_path).expect("bind resource-health socket");
        let incoming = UnixListenerStream::new(listener);
        let server = tonic::transport::Server::builder()
            .add_service(v1alpha1::DraResourceHealthServer::new(service));
        let handle = tokio::spawn(async move {
            let _ = server.serve_with_incoming(incoming).await;
        });
        (socket_path, socket_dir, handle)
    }

    #[tokio::test]
    async fn resource_health_updates_reconnect_after_reporter_stops() {
        let reporter = Arc::new(ReconnectingReporter::default());
        let (socket_path, _socket_dir, handle) =
            start_server(DraResourceHealthService::new(Arc::clone(&reporter)));
        let mut client = connect(&socket_path).await.expect("connect health client");

        let mut first = client
            .node_watch_resources(v1alpha1::NodeWatchResourcesRequest {})
            .await
            .expect("start first health watch")
            .into_inner();
        let first_update = first
            .message()
            .await
            .expect("first health stream response")
            .expect("first health report");
        assert_eq!(first_update.devices.len(), 1);
        assert_eq!(
            v1alpha1::HealthStatus::try_from(first_update.devices[0].health)
                .expect("known health enum"),
            v1alpha1::HealthStatus::Healthy
        );
        assert_eq!(
            first
                .message()
                .await
                .expect_err("stopped reporter ends the health stream")
                .code(),
            Code::Unavailable
        );

        let mut second = client
            .node_watch_resources(v1alpha1::NodeWatchResourcesRequest {})
            .await
            .expect("reconnect health watch")
            .into_inner();
        let second_update = second
            .message()
            .await
            .expect("second health stream response")
            .expect("second health report");
        assert_eq!(
            v1alpha1::HealthStatus::try_from(second_update.devices[0].health)
                .expect("known health enum"),
            v1alpha1::HealthStatus::Unhealthy
        );
        assert_eq!(reporter.sessions.load(Ordering::SeqCst), 2);
        handle.abort();
    }

    #[tokio::test]
    async fn unsupported_resource_health_is_explicitly_unimplemented() {
        let (socket_path, _socket_dir, handle) =
            start_server(DraResourceHealthService::unsupported());
        let mut client = connect(&socket_path).await.expect("connect health client");
        assert_eq!(
            client
                .node_watch_resources(v1alpha1::NodeWatchResourcesRequest {})
                .await
                .expect_err("unsupported backend rejects health watches")
                .code(),
            Code::Unimplemented
        );
        handle.abort();
    }
}
