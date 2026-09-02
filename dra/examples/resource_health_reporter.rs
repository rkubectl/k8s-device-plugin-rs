//! Small, runnable contract example for optional DRA resource-health reporting.
//!
//! A production driver implements [`ResourceHealthReporter`] on the same
//! backend that implements [`DraDriver`], then starts it with
//! [`DraPlugin::with_resource_health`]. The reporting loop must return when
//! kubelet closes the sender; this example closes its local receiver after one
//! report to exercise that lifecycle without requiring a Kubernetes cluster.
//!
//! Run it with:
//!
//! ```bash
//! cargo run -p k8s-device-plugin-dra --example resource_health_reporter --features resource-health
//! ```

use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use async_trait::async_trait;
use k8s_device_plugin_core::DraDeviceHealth;
use k8s_device_plugin_core::ResourceHealthError;
use k8s_device_plugin_core::ResourceHealthReport;
use k8s_device_plugin_core::ResourceHealthReporter;
use k8s_device_plugin_core::ResourceHealthStatus;
use tokio::sync::mpsc;

#[derive(Debug)]
struct WidgetHealthReporter;

#[async_trait]
impl ResourceHealthReporter for WidgetHealthReporter {
    async fn watch_resource_health(
        &self,
        reports: mpsc::Sender<ResourceHealthReport>,
    ) -> Result<(), ResourceHealthError> {
        let last_updated_time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| ResourceHealthError::MonitorFailed(error.to_string()))?
            .as_secs();
        let last_updated_time = i64::try_from(last_updated_time)
            .map_err(|error| ResourceHealthError::MonitorFailed(error.to_string()))?;
        let report = ResourceHealthReport {
            devices: vec![DraDeviceHealth {
                pool_name: "widget-pool".to_string(),
                device_name: "widget-0".to_string(),
                health: ResourceHealthStatus::Healthy,
                last_updated_time,
                health_check_timeout_seconds: 30,
                message: "backend monitor reports normal operation".to_string(),
            }],
        };

        reports.send(report).await.map_err(|_| {
            ResourceHealthError::MonitorFailed("kubelet stopped the health watch".to_string())
        })?;
        reports.closed().await;
        Ok(())
    }
}

#[tokio::main]
async fn main() -> Result<(), ResourceHealthError> {
    let (sender, mut receiver) = mpsc::channel(1);
    let reporter = WidgetHealthReporter;
    let watch = reporter.watch_resource_health(sender);
    let consume_one_report = async {
        let report = receiver.recv().await.ok_or_else(|| {
            ResourceHealthError::MonitorFailed("reporter ended before sending a report".to_string())
        })?;
        println!("reported health for {} device(s)", report.devices.len());
        Ok(())
    };

    tokio::try_join!(watch, consume_one_report)?;
    Ok(())
}
