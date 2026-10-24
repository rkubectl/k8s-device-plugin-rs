//! A validation-only DRA driver with one static pool of fake devices.
//!
//! Build a Linux container image for it with `dra/Dockerfile`, then deploy
//! `dra/k8s/`. It exposes a harmless CDI environment marker instead of a real
//! hardware device, so a consumer pod can prove the complete DRA path without
//! requiring a host device node.
//!
//! ## Extended-resource consumption
//!
//! This driver's `ResourcePool` and `ClaimPreparer` implementations are the
//! same whether a workload declares an explicit `ResourceClaim` or requests a
//! Kubernetes v1.37 DRA extended resource. In the latter case, a `DeviceClass`
//! maps this driver's devices to `dra.example.com/widget`, and Kubernetes
//! creates the backing claim for the Pod. The complete driver-author and
//! workload-consumer guide is in `docs/dra-extended-resources.md`.

use std::collections::BTreeMap;
use std::env;
use std::io;

use async_trait::async_trait;
use serde_json as json;

use k8s_device_plugin_core::ClaimPreparer;
use k8s_device_plugin_core::ClaimRef;
use k8s_device_plugin_core::DeviceBinding;
use k8s_device_plugin_core::DeviceTaint;
use k8s_device_plugin_core::DeviceTaintEffect;
use k8s_device_plugin_core::DraDriver;
use k8s_device_plugin_core::InventoryDevice;
use k8s_device_plugin_core::PoolDevice;
use k8s_device_plugin_core::PrepareError;
use k8s_device_plugin_core::PreparedDevice;
use k8s_device_plugin_core::PreparedDeviceWithMetadata;
use k8s_device_plugin_core::ResolvedClaim;
use k8s_device_plugin_core::ResourcePool;
use k8s_device_plugin_dra::ClaimDeviceStatus;
use k8s_device_plugin_dra::ClaimDeviceStatusPublisher;
use k8s_device_plugin_dra::DeviceMetadataOptions;
use k8s_device_plugin_dra::DraPlugin;

const DEFAULT_DRIVER_NAME: &str = "dra.example.com";
const DEFAULT_POOL_NAME: &str = "widget-pool";
const DEFAULT_DEVICE_NAMES: &str = "widget-0";
const CDI_SPEC_DIR: &str = "/var/run/cdi";
const CDI_SPEC_PATH: &str = "/var/run/cdi/dra-example.json";
const CDI_KIND: &str = "dra.example.com/widget";

fn cdi_device_id(device_name: &str) -> String {
    format!("{CDI_KIND}={device_name}")
}

async fn write_cdi_spec(devices: &[PoolDevice], metadata: bool) -> io::Result<()> {
    let spec = json::json!({
        "cdiVersion": "0.8.0",
        "kind": CDI_KIND,
        "devices": devices.iter().map(|device| json::json!({
            "name": device.name,
            "containerEdits": {
                "env": if metadata { vec![format!("DRA_E2E_DEVICE={}", device.name), format!("DRA_METADATA_PHYSICAL_{}=present",device.name.replace('-', "_"))] } else {vec![format!("DRA_E2E_DEVICE={}", device.name)]},
            },
        })).collect::<Vec<_>>(),
    });

    tokio::fs::create_dir_all(CDI_SPEC_DIR).await?;
    let encoded = json::to_vec_pretty(&spec).map_err(io::Error::other)?;
    tokio::fs::write(CDI_SPEC_PATH, encoded).await
}

#[derive(Debug)]
struct StaticDraDriver {
    name: String,
    pool: String,
    devices: Vec<PoolDevice>,
    status_publisher: Option<ClaimDeviceStatusPublisher>,
    taint: Option<DeviceTaint>,
    binding: Option<DeviceBinding>,
}

impl StaticDraDriver {
    fn new(name: String, pool: String, devices: Vec<PoolDevice>) -> Self {
        Self {
            name,
            pool,
            devices,
            status_publisher: None,
            taint: None,
            binding: None,
        }
    }

    fn with_status_publisher(mut self, status_publisher: ClaimDeviceStatusPublisher) -> Self {
        self.status_publisher = Some(status_publisher);
        self
    }

    fn has_device(&self, pool_name: &str, device_name: &str) -> bool {
        pool_name == self.pool && self.devices.iter().any(|device| device.name == device_name)
    }

    fn with_taint(mut self, taint: Option<DeviceTaint>) -> Self {
        self.taint = taint;
        self
    }

    fn with_binding(mut self, binding: Option<DeviceBinding>) -> Self {
        self.binding = binding;
        self
    }
}

#[async_trait]
impl ResourcePool for StaticDraDriver {
    async fn devices(&self) -> BTreeMap<String, Vec<PoolDevice>> {
        BTreeMap::from([(self.pool.clone(), self.devices.clone())])
    }

    async fn inventory(&self) -> BTreeMap<String, Vec<InventoryDevice>> {
        let devices = self
            .devices
            .iter()
            .cloned()
            .map(|device| {
                let mut inventory = InventoryDevice::new(device);
                if let Some(taint) = &self.taint {
                    inventory = inventory.with_taint(taint.clone());
                }
                if let Some(binding) = &self.binding {
                    inventory = inventory.with_binding(binding.clone());
                }
                inventory
            })
            .collect();
        BTreeMap::from([(self.pool.clone(), devices)])
    }
}

#[async_trait]
impl ClaimPreparer for StaticDraDriver {
    async fn prepare(
        &self,
        claims: &[ResolvedClaim],
    ) -> BTreeMap<ClaimRef, Result<Vec<PreparedDevice>, PrepareError>> {
        let mut results = BTreeMap::new();
        for resolved in claims {
            tracing::info!(
                claim_namespace = resolved.claim.namespace,
                claim_name = resolved.claim.name,
                claim_uid = resolved.claim.uid,
                device_count = resolved.devices.len(),
                "preparing DRA claim"
            );
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
                        cdi_device_ids: vec![cdi_device_id(&device.device_name)],
                    })
                })
                .collect::<Result<Vec<_>, _>>();

            let prepared = match prepared {
                Ok(prepared) => prepared,
                Err(error) => {
                    results.insert(resolved.claim.clone(), Err(error));
                    continue;
                }
            };
            if let Some(status_publisher) = &self.status_publisher {
                let statuses = prepared.iter().map(|device| {
                    let mut status = ClaimDeviceStatus::new(&device.pool_name, &device.device_name);
                    status.data = Some(json::json!({"phase": "prepared"}));
                    status
                });
                if let Err(error) = status_publisher
                    .publish_fields(&resolved.claim, statuses)
                    .await
                {
                    results.insert(
                        resolved.claim.clone(),
                        Err(PrepareError::HookFailed(format!(
                            "failed to publish ResourceClaim device status: {error}"
                        ))),
                    );
                    continue;
                }
            }
            results.insert(resolved.claim.clone(), Ok(prepared));
        }
        results
    }

    async fn prepare_with_metadata(
        &self,
        claims: &[ResolvedClaim],
    ) -> BTreeMap<ClaimRef, Result<Vec<PreparedDeviceWithMetadata>, PrepareError>> {
        self.prepare(claims)
            .await
            .into_iter()
            .map(|(claim, result)| {
                (
                    claim,
                    result.map(|devices| {
                        devices
                            .into_iter()
                            .map(|device| {
                                let attributes = self
                                    .devices
                                    .iter()
                                    .find(|source| source.name == device.device_name)
                                    .map_or_else(BTreeMap::new, |source| source.attributes.clone());
                                let mut metadata = PreparedDeviceWithMetadata::new(device);
                                metadata.attributes = attributes;
                                metadata
                            })
                            .collect()
                    }),
                )
            })
            .collect()
    }

    async fn unprepare(&self, claim: &ClaimRef) -> Result<(), PrepareError> {
        tracing::info!(
            claim_namespace = claim.namespace,
            claim_name = claim.name,
            claim_uid = claim.uid,
            "unpreparing DRA claim"
        );
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
    let metadata_enabled = match env_or_default("DRA_EXAMPLE_DEVICE_METADATA", "0").as_str() {
        "0" => false,
        "1" => true,
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "DRA_EXAMPLE_DEVICE_METADATA must be 0 or 1",
            ));
        }
    };
    let mut devices = parse_device_names(&env_or_default("DEVICE_NAMES", DEFAULT_DEVICE_NAMES));
    if metadata_enabled {
        for device in &mut devices {
            device
                .attributes
                .insert("model".into(), "validation-widget".into());
        }
    }
    let taint = env::var("DRA_EXAMPLE_TAINT_EFFECT")
        .ok()
        .map(|value| parse_taint_effect(&value))
        .transpose()?
        .map(|effect| DeviceTaint::new("dra.example.com/maintenance", effect));
    let binding = match env_or_default("DRA_EXAMPLE_BINDING_CONDITIONS", "0").as_str() {
        "0" => None,
        "1" => Some(
            DeviceBinding::new()
                .with_condition("dra.example.com/Attached")
                .with_failure_condition("dra.example.com/AttachFailed")
                .with_node_binding(),
        ),
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "DRA_EXAMPLE_BINDING_CONDITIONS must be 0 or 1",
            ));
        }
    };
    let node_name = env::var("NODE_NAME")
        .map_err(|error| io::Error::other(format!("NODE_NAME must be set: {error}")))?;

    if devices.is_empty() {
        tracing::warn!("no DEVICE_NAMES configured -- publishing an empty ResourceSlice");
    }
    write_cdi_spec(&devices, metadata_enabled).await?;
    tracing::info!(
        driver_name,
        pool_name,
        node_name,
        device_count = devices.len(),
        "starting validation-only DRA driver"
    );

    let client = kube::Client::try_default()
        .await
        .map_err(io::Error::other)?;
    let status_publisher = ClaimDeviceStatusPublisher::new(client.clone(), driver_name.clone());
    let driver = StaticDraDriver::new(driver_name.clone(), pool_name, devices)
        .with_status_publisher(status_publisher)
        .with_taint(taint)
        .with_binding(binding);
    let plugin = DraPlugin::new(client, driver_name.clone(), node_name, driver);
    let plugin = if metadata_enabled {
        plugin.with_device_metadata(DeviceMetadataOptions::v1beta1())?
    } else {
        plugin
    };
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

fn parse_taint_effect(value: &str) -> io::Result<DeviceTaintEffect> {
    match value {
        "None" => Ok(DeviceTaintEffect::None),
        "NoSchedule" => Ok(DeviceTaintEffect::NoSchedule),
        "NoExecute" => Ok(DeviceTaintEffect::NoExecute),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "DRA_EXAMPLE_TAINT_EFFECT must be None, NoSchedule, or NoExecute",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_taint_configuration_does_not_silently_disable_policy() {
        assert!(parse_taint_effect("PreferNoSchedule").is_err());
        assert!(parse_taint_effect("").is_err());
        assert_eq!(
            parse_taint_effect("NoExecute").unwrap(),
            DeviceTaintEffect::NoExecute
        );
    }

    #[test]
    fn parse_device_names_splits_trims_and_drops_empty_values() {
        let devices = parse_device_names(" widget-0, ,widget-1 ,, ");

        let names = devices
            .iter()
            .map(|device| device.name.as_str())
            .collect::<Vec<&str>>();
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

    #[test]
    fn cdi_device_id_uses_the_validation_spec_kind() {
        assert_eq!(cdi_device_id("widget-0"), "dra.example.com/widget=widget-0");
    }
}
