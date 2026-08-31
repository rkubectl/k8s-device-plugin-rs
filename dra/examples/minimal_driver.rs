//! A validation-only DRA driver with one static pool of fake devices.
//!
//! Build a Linux container image for it with `dra/Dockerfile`, then deploy
//! `dra/k8s/`. It deliberately returns no CDI device names: this example
//! validates registration, `ResourceSlice` publishing, and claim preparation
//! plumbing without pretending to provide a real hardware device.

use std::collections::HashMap;
use std::env;
use std::io;

use async_trait::async_trait;
use k8s_device_plugin_core::ClaimPreparer;
use k8s_device_plugin_core::ClaimRef;
use k8s_device_plugin_core::DraDriver;
use k8s_device_plugin_core::PoolDevice;
use k8s_device_plugin_core::PrepareError;
use k8s_device_plugin_core::PreparedDevice;
use k8s_device_plugin_core::ResolvedClaim;
use k8s_device_plugin_core::ResourcePool;
use k8s_device_plugin_dra::DraPlugin;

const DEFAULT_DRIVER_NAME: &str = "dra.example.com";
const DEFAULT_POOL_NAME: &str = "widget-pool";
const DEFAULT_DEVICE_NAMES: &str = "widget-0";

#[derive(Debug)]
struct StaticDraDriver {
    name: String,
    pool: String,
    devices: Vec<PoolDevice>,
}

impl StaticDraDriver {
    fn new(name: String, pool: String, devices: Vec<PoolDevice>) -> Self {
        Self {
            name,
            pool,
            devices,
        }
    }

    fn has_device(&self, pool_name: &str, device_name: &str) -> bool {
        pool_name == self.pool && self.devices.iter().any(|device| device.name == device_name)
    }
}

#[async_trait]
impl ResourcePool for StaticDraDriver {
    async fn devices(&self) -> HashMap<String, Vec<PoolDevice>> {
        HashMap::from([(self.pool.clone(), self.devices.clone())])
    }
}

#[async_trait]
impl ClaimPreparer for StaticDraDriver {
    async fn prepare(
        &self,
        claims: &[ResolvedClaim],
    ) -> HashMap<ClaimRef, Result<Vec<PreparedDevice>, PrepareError>> {
        claims
            .iter()
            .map(|resolved| {
                let prepared = resolved
                    .devices
                    .iter()
                    .map(|device| {
                        if !self.has_device(&device.pool_name, &device.device_name) {
                            return Err(PrepareError::DeviceUnavailable(format!(
                                "{}/{}",
                                device.pool_name, device.device_name
                            )));
                        }
                        Ok(PreparedDevice {
                            request_names: device.request_name.clone().into_iter().collect(),
                            pool_name: device.pool_name.clone(),
                            device_name: device.device_name.clone(),
                            cdi_device_ids: Vec::new(),
                        })
                    })
                    .collect();
                (resolved.claim.clone(), prepared)
            })
            .collect()
    }

    async fn unprepare(&self, _claim: &ClaimRef) -> Result<(), PrepareError> {
        Ok(())
    }
}

impl DraDriver for StaticDraDriver {
    fn driver_name(&self) -> &str {
        &self.name
    }
}

#[tokio::main]
async fn main() -> io::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let driver_name = env_or_default("DRIVER_NAME", DEFAULT_DRIVER_NAME);
    let pool_name = env_or_default("POOL_NAME", DEFAULT_POOL_NAME);
    let devices = parse_device_names(&env_or_default("DEVICE_NAMES", DEFAULT_DEVICE_NAMES));
    let node_name = env::var("NODE_NAME")
        .map_err(|error| io::Error::other(format!("NODE_NAME must be set: {error}")))?;

    if devices.is_empty() {
        tracing::warn!("no DEVICE_NAMES configured -- publishing an empty ResourceSlice");
    }
    tracing::info!(
        %driver_name,
        %pool_name,
        %node_name,
        device_count = devices.len(),
        "starting validation-only DRA driver"
    );

    let client = kube::Client::try_default()
        .await
        .map_err(io::Error::other)?;
    let plugin = DraPlugin::new(
        client,
        driver_name.clone(),
        node_name,
        StaticDraDriver::new(driver_name, pool_name, devices),
    );
    plugin.run().await
}

fn env_or_default(name: &str, default: &str) -> String {
    env::var(name).unwrap_or_else(|_| default.to_string())
}

fn parse_device_names(names: &str) -> Vec<PoolDevice> {
    names
        .split(',')
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(PoolDevice::new)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_device_names_splits_trims_and_drops_empty_values() {
        let devices = parse_device_names(" widget-0, ,widget-1 ,, ");

        let names: Vec<&str> = devices.iter().map(|device| device.name.as_str()).collect();
        assert_eq!(names, ["widget-0", "widget-1"]);
    }

    #[test]
    fn static_driver_accepts_devices_from_its_pool_only() {
        let driver = StaticDraDriver::new(
            DEFAULT_DRIVER_NAME.to_string(),
            DEFAULT_POOL_NAME.to_string(),
            parse_device_names(DEFAULT_DEVICE_NAMES),
        );

        assert!(driver.has_device(DEFAULT_POOL_NAME, "widget-0"));
        assert!(!driver.has_device("other-pool", "widget-0"));
        assert!(!driver.has_device(DEFAULT_POOL_NAME, "other-device"));
    }
}
