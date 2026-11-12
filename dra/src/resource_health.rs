//! Optional `DRAResourceHealth.NodeWatchResources` service.

use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use k8s_device_plugin_dra_core::DraDeviceHealth;
use k8s_device_plugin_dra_core::ResourceHealthError;
use k8s_device_plugin_dra_core::ResourceHealthReport;
use k8s_device_plugin_dra_core::ResourceHealthStatus;

use super::*;

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
    /// This is used when a backend has not opted in; kubelet then stops
    /// opening health watches for that driver.
    pub fn unsupported() -> Self {
        Self { reporter: None }
    }

    fn watch_stream<T: Send + 'static>(
        &self,
        convert: fn(ResourceHealthReport) -> T,
    ) -> Result<ReceiverStream<tonic::Result<T>>, tonic::Status> {
        let reporter = self.reporter.as_ref().ok_or_else(|| {
            tonic::Status::unimplemented(
                "resource health reporting is not supported by this backend",
            )
        })?;
        let (reports_tx, mut reports_rx) = mpsc::channel(REPORT_BUFFER_CAPACITY);
        let (responses_tx, responses_rx) = mpsc::channel(REPORT_BUFFER_CAPACITY);
        let reporter = Arc::clone(reporter);

        tokio::spawn(async move {
            let mut watch =
                tokio::spawn(async move { reporter.watch_resource_health(reports_tx).await });

            loop {
                tokio::select! {
                    _ = responses_tx.closed() => {
                        watch.abort();
                        let _ = watch.await;
                        return;
                    },
                    report = reports_rx.recv() => match report {
                        Some(report) => {
                            if responses_tx.send(Ok(convert(report))).await.is_err() {
                                watch.abort();
                                let _ = watch.await;
                                return;
                            }
                        }
                        None => {
                            // A reporter can drop its sender before finishing
                            // cleanup. Keep observing client cancellation here.
                            tokio::select! {
                                _ = responses_tx.closed() => {
                                    watch.abort();
                                    let _ = watch.await;
                                },
                                result = &mut watch => {
                                    send_watch_completion(&responses_tx, result).await;
                                },
                            }
                            return;
                        }
                    },
                    result = &mut watch => {
                        // The reporter may have sent its final snapshot just
                        // before returning. Preserve every buffered report
                        // before signalling that kubelet should reconnect.
                        while let Ok(report) = reports_rx.try_recv() {
                            if responses_tx.send(Ok(convert(report))).await.is_err() {
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

fn report_to_v1alpha1(
    report: ResourceHealthReport,
) -> dra::health::v1alpha1::NodeWatchResourcesResponse {
    dra::health::v1alpha1::NodeWatchResourcesResponse {
        devices: report.devices.into_iter().map(device_to_v1alpha1).collect(),
    }
}

fn device_to_v1alpha1(device: DraDeviceHealth) -> dra::health::v1alpha1::DeviceHealth {
    let health = match device.health {
        ResourceHealthStatus::Unknown => dra::health::v1alpha1::HealthStatus::Unknown,
        ResourceHealthStatus::Healthy => dra::health::v1alpha1::HealthStatus::Healthy,
        ResourceHealthStatus::Unhealthy => dra::health::v1alpha1::HealthStatus::Unhealthy,
    };
    dra::health::v1alpha1::DeviceHealth {
        device: Some(dra::health::v1alpha1::DeviceIdentifier {
            pool_name: device.pool_name.into_string(),
            device_name: device.device_name.into_string(),
        }),
        health: health.into(),
        last_updated_time: device.last_updated_time,
        health_check_timeout_seconds: device.health_check_timeout_seconds,
        message: device.message,
    }
}

fn report_to_v1(report: ResourceHealthReport) -> dra::health::v1::NodeWatchResourcesResponse {
    dra::health::v1::NodeWatchResourcesResponse {
        devices: report.devices.into_iter().map(device_to_v1).collect(),
    }
}

fn device_to_v1(device: DraDeviceHealth) -> dra::health::v1::DeviceHealth {
    let health = match device.health {
        ResourceHealthStatus::Unknown => dra::health::v1::HealthStatus::Unknown,
        ResourceHealthStatus::Healthy => dra::health::v1::HealthStatus::Healthy,
        ResourceHealthStatus::Unhealthy => dra::health::v1::HealthStatus::Unhealthy,
    };
    dra::health::v1::DeviceHealth {
        device: Some(dra::health::v1::DeviceIdentifier {
            pool_name: device.pool_name.into_string(),
            device_name: device.device_name.into_string(),
        }),
        health: health.into(),
        last_updated_time: device.last_updated_time,
        health_check_timeout_seconds: device.health_check_timeout_seconds,
        message: device.message,
    }
}

async fn send_watch_completion<T>(
    responses: &mpsc::Sender<tonic::Result<T>>,
    result: Result<Result<(), ResourceHealthError>, task::JoinError>,
) {
    let message = match result {
        Ok(Ok(())) => "resource health reporter stopped".to_string(),
        Ok(Err(err)) => err.to_string(),
        Err(err) => format!("resource health reporter panicked: {err}"),
    };
    let _ = responses
        .send(Err(tonic::Status::new(tonic::Code::Unavailable, message)))
        .await;
}

#[tonic::async_trait]
impl dra::health::v1alpha1::DraResourceHealth for DraResourceHealthService {
    type NodeWatchResourcesStream =
        ReceiverStream<tonic::Result<dra::health::v1alpha1::NodeWatchResourcesResponse>>;

    async fn node_watch_resources(
        &self,
        _request: tonic::Request<dra::health::v1alpha1::NodeWatchResourcesRequest>,
    ) -> tonic::Result<tonic::Response<Self::NodeWatchResourcesStream>> {
        let stream = self.watch_stream(report_to_v1alpha1)?;
        tracing::info!(
            service = dra::health::v1alpha1::SERVICE_NAME,
            "serving DRA resource health watch"
        );
        Ok(tonic::Response::new(stream))
    }
}

#[tonic::async_trait]
impl dra::health::v1::DraResourceHealth for DraResourceHealthService {
    type NodeWatchResourcesStream =
        ReceiverStream<tonic::Result<dra::health::v1::NodeWatchResourcesResponse>>;

    async fn node_watch_resources(
        &self,
        _request: tonic::Request<dra::health::v1::NodeWatchResourcesRequest>,
    ) -> tonic::Result<tonic::Response<Self::NodeWatchResourcesStream>> {
        let stream = self.watch_stream(report_to_v1)?;
        tracing::info!(
            service = dra::health::v1::SERVICE_NAME,
            "serving DRA resource health watch"
        );
        Ok(tonic::Response::new(stream))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;
    use std::sync::atomic::Ordering;

    use camino_tempfile::Utf8TempDir;
    use hyper_util::rt::TokioIo;
    use prost::Message;
    use tokio_stream::wrappers::UnixListenerStream;
    use tower::service_fn;

    use super::*;

    struct IdleReporter {
        started: tokio::sync::Notify,
        stopped: Arc<tokio::sync::Notify>,
        drop_sender: bool,
    }

    struct NotifyOnDrop(Arc<tokio::sync::Notify>);

    impl Drop for NotifyOnDrop {
        fn drop(&mut self) {
            self.0.notify_one();
        }
    }

    #[tonic::async_trait]
    impl ResourceHealthReporter for IdleReporter {
        async fn watch_resource_health(
            &self,
            reports: mpsc::Sender<ResourceHealthReport>,
        ) -> Result<(), ResourceHealthError> {
            let _stopped = NotifyOnDrop(Arc::clone(&self.stopped));
            let reports = if self.drop_sender {
                drop(reports);
                None
            } else {
                Some(reports)
            };
            self.started.notify_one();
            std::future::pending::<()>().await;
            drop(reports);
            Ok(())
        }
    }

    #[tokio::test]
    async fn dropping_idle_stream_reaps_reporter_even_after_report_channel_closes() {
        for drop_sender in [false, true] {
            let reporter = Arc::new(IdleReporter {
                started: tokio::sync::Notify::new(),
                stopped: Arc::new(tokio::sync::Notify::new()),
                drop_sender,
            });
            let service = DraResourceHealthService::new(Arc::clone(&reporter));
            // Repeated disconnections must not accumulate idle monitors.
            for stable in [false, true, false, true] {
                let stream: Box<dyn Send> = if stable {
                    Box::new(service.watch_stream(report_to_v1).expect("start v1 stream"))
                } else {
                    Box::new(
                        service
                            .watch_stream(report_to_v1alpha1)
                            .expect("start v1alpha1 stream"),
                    )
                };
                tokio::time::timeout(Duration::from_secs(1), reporter.started.notified())
                    .await
                    .expect("reporter starts");
                task::yield_now().await;
                drop(stream);
                tokio::time::timeout(Duration::from_secs(1), reporter.stopped.notified())
                    .await
                    .expect("idle reporter is dropped after disconnect");
            }
        }
    }

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
                        pool_name: "pool-0".parse().unwrap(),
                        device_name: "widget-0".parse().unwrap(),
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

    async fn connect(socket_path: &Utf8Path) -> tonic::Result<transport::Channel> {
        let socket_path = socket_path.to_path_buf();
        let connector_path = socket_path.clone();
        let endpoint = transport::Endpoint::try_from("http://[::]:50051").map_err(|err| {
            tonic::Status::internal(format!("build resource-health endpoint: {err}"))
        })?;
        let channel = endpoint
            .connect_with_connector(service_fn(move |_| {
                let path = connector_path.clone();
                async move { net::UnixStream::connect(path).await.map(TokioIo::new) }
            }))
            .await
            .map_err(|err| {
                tonic::Status::unavailable(format!(
                    "failed to connect to resource-health socket {}: {err}",
                    socket_path
                ))
            })?;
        Ok(channel)
    }

    fn start_server(
        service: DraResourceHealthService,
    ) -> (Utf8PathBuf, Utf8TempDir, task::JoinHandle<()>) {
        let socket_dir = Utf8TempDir::new().expect("create resource-health socket directory");
        let socket_path = socket_dir.path().join("plugin.sock");
        let listener = net::UnixListener::bind(&socket_path).expect("bind resource-health socket");
        let incoming = UnixListenerStream::new(listener);
        let server = transport::Server::builder()
            .add_service(dra::health::v1::DraResourceHealthServer::new(
                service.clone(),
            ))
            .add_service(dra::health::v1alpha1::DraResourceHealthServer::new(service));
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
        let mut client = dra::health::v1alpha1::DraResourceHealthClient::new(
            connect(&socket_path).await.expect("connect health client"),
        );

        let mut first = client
            .node_watch_resources(dra::health::v1alpha1::NodeWatchResourcesRequest {})
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
            dra::health::v1alpha1::HealthStatus::try_from(first_update.devices[0].health)
                .expect("known health enum"),
            dra::health::v1alpha1::HealthStatus::Healthy
        );
        assert_eq!(
            first
                .message()
                .await
                .expect_err("stopped reporter ends the health stream")
                .code(),
            tonic::Code::Unavailable
        );

        let mut second = client
            .node_watch_resources(dra::health::v1alpha1::NodeWatchResourcesRequest {})
            .await
            .expect("reconnect health watch")
            .into_inner();
        let second_update = second
            .message()
            .await
            .expect("second health stream response")
            .expect("second health report");
        assert_eq!(
            dra::health::v1alpha1::HealthStatus::try_from(second_update.devices[0].health)
                .expect("known health enum"),
            dra::health::v1alpha1::HealthStatus::Unhealthy
        );
        assert_eq!(reporter.sessions.load(Ordering::SeqCst), 2);
        handle.abort();
        assert!(handle.await.unwrap_err().is_cancelled());
    }

    #[tokio::test]
    async fn v1_stream_preserves_reports_and_reconnects() {
        let reporter = Arc::new(ReconnectingReporter::default());
        let (socket_path, _socket_dir, handle) =
            start_server(DraResourceHealthService::new(Arc::clone(&reporter)));
        let mut client =
            dra::health::v1::DraResourceHealthClient::new(connect(&socket_path).await.unwrap());
        for health in [
            dra::health::v1::HealthStatus::Healthy,
            dra::health::v1::HealthStatus::Unhealthy,
        ] {
            let mut stream = client
                .node_watch_resources(dra::health::v1::NodeWatchResourcesRequest {})
                .await
                .unwrap()
                .into_inner();
            let report = stream.message().await.unwrap().unwrap();
            assert_eq!(
                report.devices,
                vec![dra::health::v1::DeviceHealth {
                    device: Some(dra::health::v1::DeviceIdentifier {
                        pool_name: "pool-0".into(),
                        device_name: "widget-0".into(),
                    }),
                    health: health.into(),
                    last_updated_time: 1_700_000_000,
                    health_check_timeout_seconds: 30,
                    message: "monitor update".into(),
                }]
            );
            assert_eq!(
                stream.message().await.unwrap_err().code(),
                tonic::Code::Unavailable
            );
        }
        assert_eq!(reporter.sessions.load(Ordering::SeqCst), 2);
        handle.abort();
        assert!(handle.await.unwrap_err().is_cancelled());
    }

    #[test]
    fn health_versions_have_identical_report_bytes_and_explicit_enum_values() {
        for (health, wire) in [
            (ResourceHealthStatus::Unknown, 0),
            (ResourceHealthStatus::Healthy, 1),
            (ResourceHealthStatus::Unhealthy, 2),
        ] {
            let report = ResourceHealthReport {
                devices: vec![DraDeviceHealth {
                    pool_name: "pool-0".parse().unwrap(),
                    device_name: "widget-0".parse().unwrap(),
                    health,
                    last_updated_time: 1_700_000_000,
                    health_check_timeout_seconds: -1,
                    message: "health detail".into(),
                }],
            };
            let v1 = report_to_v1(report.clone());
            let alpha = report_to_v1alpha1(report);
            assert_eq!(v1.devices[0].health, wire);
            assert_eq!(alpha.devices[0].health, wire);
            assert_eq!(v1.encode_to_vec(), alpha.encode_to_vec());
            assert_eq!(v1.devices[0].last_updated_time, 1_700_000_000);
            assert_eq!(v1.devices[0].health_check_timeout_seconds, -1);
            assert_eq!(v1.devices[0].message, "health detail");
            assert_eq!(v1.devices[0].device.as_ref().unwrap().pool_name, "pool-0");
            assert_eq!(
                v1.devices[0].device.as_ref().unwrap().device_name,
                "widget-0"
            );
        }
    }

    #[tokio::test]
    async fn unsupported_resource_health_is_explicitly_unimplemented() {
        let (socket_path, _socket_dir, handle) =
            start_server(DraResourceHealthService::unsupported());
        let channel = connect(&socket_path).await.expect("connect health client");
        let mut stable = dra::health::v1::DraResourceHealthClient::new(channel.clone());
        assert_eq!(
            stable
                .node_watch_resources(dra::health::v1::NodeWatchResourcesRequest {})
                .await
                .unwrap_err()
                .code(),
            tonic::Code::Unimplemented
        );
        let mut client = dra::health::v1alpha1::DraResourceHealthClient::new(channel);
        assert_eq!(
            client
                .node_watch_resources(dra::health::v1alpha1::NodeWatchResourcesRequest {})
                .await
                .expect_err("unsupported backend rejects health watches")
                .code(),
            tonic::Code::Unimplemented
        );
        handle.abort();
        assert!(handle.await.unwrap_err().is_cancelled());
    }
}
