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

use k8s_device_plugin_dra::ClaimDeviceStatus;
use k8s_device_plugin_dra::ClaimDeviceStatusPublisher;
use k8s_device_plugin_dra::DeviceMetadataOptions;
use k8s_device_plugin_dra::DraPlugin;
use k8s_device_plugin_dra_core::ClaimPreparer;
use k8s_device_plugin_dra_core::ClaimRef;
use k8s_device_plugin_dra_core::CounterSet;
use k8s_device_plugin_dra_core::DeviceBinding;
use k8s_device_plugin_dra_core::DeviceCounterConsumption;
use k8s_device_plugin_dra_core::DeviceName;
use k8s_device_plugin_dra_core::DeviceTaint;
use k8s_device_plugin_dra_core::DeviceTaintEffect;
use k8s_device_plugin_dra_core::DraDriver;
use k8s_device_plugin_dra_core::DriverName;
use k8s_device_plugin_dra_core::InventoryDevice;
use k8s_device_plugin_dra_core::NodeName;
use k8s_device_plugin_dra_core::PoolDevice;
use k8s_device_plugin_dra_core::PoolInventory;
use k8s_device_plugin_dra_core::PoolName;
use k8s_device_plugin_dra_core::PrepareError;
use k8s_device_plugin_dra_core::PreparedDevice;
use k8s_device_plugin_dra_core::PreparedDeviceWithMetadata;
use k8s_device_plugin_dra_core::Quantity;
use k8s_device_plugin_dra_core::ResolvedClaim;
use k8s_device_plugin_dra_core::ResourcePool;

// Share the runtime's private CDI projection without publishing a partial API.
#[path = "../src/cdi.rs"]
mod cdi;

fn default<T: Default>() -> T {
    T::default()
}

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
    let spec = cdi::Spec {
        cdi_version: "0.8.0",
        kind: CDI_KIND.into(),
        devices: devices
            .iter()
            .map(|device| cdi::Device {
                name: device.name.to_string(),
                container_edits: cdi::ContainerEdits {
                    env: if metadata {
                        vec![
                            format!("DRA_E2E_DEVICE={}", device.name),
                            format!(
                                "DRA_METADATA_PHYSICAL_{}=present",
                                device.name.as_str().replace('-', "_")
                            ),
                        ]
                    } else {
                        vec![format!("DRA_E2E_DEVICE={}", device.name)]
                    },
                    ..default()
                },
            })
            .collect(),
    };

    tokio::fs::create_dir_all(CDI_SPEC_DIR).await?;
    let encoded = json::to_vec_pretty(&spec).map_err(io::Error::other)?;
    tokio::fs::write(CDI_SPEC_PATH, encoded).await
}

#[derive(Debug)]
struct StaticDraDriver {
    name: DriverName,
    pool: PoolName,
    devices: Vec<PoolDevice>,
    status_publisher: Option<ClaimDeviceStatusPublisher>,
    taint: Option<DeviceTaint>,
    binding: Option<DeviceBinding>,
    pool_inventory: Option<PoolInventory>,
}

impl StaticDraDriver {
    fn new(name: DriverName, pool: PoolName, devices: Vec<PoolDevice>) -> Self {
        Self {
            name,
            pool,
            devices,
            status_publisher: None,
            taint: None,
            binding: None,
            pool_inventory: None,
        }
    }

    fn with_status_publisher(mut self, status_publisher: ClaimDeviceStatusPublisher) -> Self {
        self.status_publisher = Some(status_publisher);
        self
    }

    fn has_device(&self, pool_name: &str, device_name: &str) -> bool {
        pool_name == self.pool.as_str()
            && self
                .devices
                .iter()
                .any(|device| device.name.as_str() == device_name)
    }

    fn with_taint(mut self, taint: Option<DeviceTaint>) -> Self {
        self.taint = taint;
        self
    }

    fn with_binding(mut self, binding: Option<DeviceBinding>) -> Self {
        self.binding = binding;
        self
    }

    fn with_inventory(mut self, inventory: PoolInventory) -> Self {
        self.devices = inventory
            .devices
            .iter()
            .map(|device| device.device.clone())
            .collect();
        self.pool_inventory = Some(inventory);
        self
    }
}

#[async_trait]
impl ResourcePool for StaticDraDriver {
    async fn devices(&self) -> BTreeMap<PoolName, Vec<PoolDevice>> {
        BTreeMap::from([(self.pool.clone(), self.devices.clone())])
    }

    async fn inventory(&self) -> BTreeMap<PoolName, Vec<InventoryDevice>> {
        let inventory = self.pool_inventory.as_ref().map_or_else(
            || {
                self.devices
                    .iter()
                    .cloned()
                    .map(InventoryDevice::from)
                    .collect()
            },
            |inventory| inventory.devices.clone(),
        );
        let devices = inventory
            .into_iter()
            .map(|device| {
                let mut inventory = device;
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

    async fn snapshot(&self) -> BTreeMap<PoolName, PoolInventory> {
        self.inventory()
            .await
            .into_iter()
            .map(|(name, devices)| {
                let mut inventory = PoolInventory::new(devices);
                if let Some(pool) = &self.pool_inventory {
                    inventory.shared_counters = pool.shared_counters.clone();
                }
                (name, inventory)
            })
            .collect()
    }
}

/// Two pairs, each exposed both as individual cards and as a complete bridge.
fn partitionable_inventory() -> PoolInventory {
    let mut inventory = PoolInventory::default();
    for pair in ["a", "b"] {
        let set = format!("pair-{pair}");
        inventory.shared_counters.push(
            CounterSet::new(set.as_str().try_into().expect("valid set"))
                .counter("cards".parse().unwrap(), Quantity::from(1)),
        );
        for (suffix, kind, consumption) in [
            ("0", "card", "0.5"),
            ("1", "card", "0.5"),
            ("bridge", "pair", "1"),
        ] {
            let device =
                PoolDevice::new(format!("{pair}-{suffix}").try_into().expect("valid device"))
                    .attribute("type", kind)
                    .attribute("pair", pair)
                    .capacity("fractional", "0.5".parse().expect("valid fixture quantity"));
            inventory
                .devices
                .push(
                    InventoryDevice::new(device).with_consumes_counters(
                        DeviceCounterConsumption::new(set.as_str().try_into().expect("valid set"))
                            .counter(
                                "cards".parse().unwrap(),
                                consumption.parse().expect("valid fixture quantity"),
                            ),
                    ),
                );
        }
    }
    inventory
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
                    if !self.has_device(device.pool_name.as_str(), device.device_name.as_str()) {
                        return Err(PrepareError::DeviceUnavailable(format!(
                            "{}/{}",
                            device.pool_name, device.device_name
                        )));
                    }
                    Ok(PreparedDevice {
                        request_names: device.request_name.clone().into_iter().collect(),
                        pool_name: device.pool_name.clone(),
                        device_name: device.device_name.clone(),
                        cdi_device_ids: vec![cdi_device_id(device.device_name.as_str())],
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
                    let mut status = ClaimDeviceStatus::new(
                        device.pool_name.clone(),
                        device.device_name.clone(),
                    );
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
    fn driver_name(&self) -> &DriverName {
        &self.name
    }
}

#[tokio::main]
async fn main() -> io::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let driver_name: DriverName = env_or_default("DRIVER_NAME", DEFAULT_DRIVER_NAME)
        .parse()
        .map_err(io::Error::other)?;
    let pool_name: PoolName = env_or_default("POOL_NAME", DEFAULT_POOL_NAME)
        .parse()
        .map_err(io::Error::other)?;
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
    let mut devices = parse_device_names(&env_or_default("DEVICE_NAMES", DEFAULT_DEVICE_NAMES))?;
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
    let node_name: NodeName = env::var("NODE_NAME")
        .map_err(|error| io::Error::other(format!("NODE_NAME must be set: {error}")))?
        .parse()
        .map_err(io::Error::other)?;

    let mut driver = StaticDraDriver::new(driver_name.clone(), pool_name.clone(), devices)
        .with_taint(taint)
        .with_binding(binding);
    match env_or_default("DRA_EXAMPLE_SHARED_COUNTERS", "0").as_str() {
        "0" => {}
        "1" => driver = driver.with_inventory(partitionable_inventory()),
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "DRA_EXAMPLE_SHARED_COUNTERS must be 0 or 1",
            ));
        }
    }
    if driver.devices.is_empty() {
        tracing::warn!("no DEVICE_NAMES configured -- publishing an empty ResourceSlice");
    }
    write_cdi_spec(&driver.devices, metadata_enabled).await?;
    tracing::info!(
        %driver_name,
        %pool_name,
        %node_name,
        device_count = driver.devices.len(),
        "starting validation-only DRA driver"
    );

    let client = kube::Client::try_default()
        .await
        .map_err(io::Error::other)?;
    let status_publisher = ClaimDeviceStatusPublisher::new(client.clone(), driver_name.clone());
    let driver = driver.with_status_publisher(status_publisher);
    let plugin = match env_or_default("DRA_EXAMPLE_RESOURCE_HEALTH", "0").as_str() {
        "0" => DraPlugin::new(client, driver_name.clone(), node_name, driver),
        "1" => DraPlugin::with_resource_health(client, driver_name.clone(), node_name, driver),
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "DRA_EXAMPLE_RESOURCE_HEALTH must be 0 or 1",
            ));
        }
    }
    .map_err(io::Error::other)?;
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

fn parse_device_names(names: &str) -> io::Result<Vec<PoolDevice>> {
    names
        .split(',')
        .filter(|name| !name.is_empty())
        .map(|name| {
            DeviceName::try_from(name)
                .map(PoolDevice::new)
                .map_err(io::Error::other)
        })
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
    fn parse_device_names_rejects_whitespace_and_drops_empty_values() {
        let devices = parse_device_names("widget-0,,widget-1,").unwrap();
        assert!(parse_device_names(" widget-0").is_err());

        let names = devices
            .iter()
            .map(|device| device.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(names, ["widget-0", "widget-1"]);
    }

    #[test]
    fn static_driver_accepts_devices_from_its_pool_only() {
        let driver = StaticDraDriver::new(
            DEFAULT_DRIVER_NAME.parse().unwrap(),
            DEFAULT_POOL_NAME.parse().unwrap(),
            parse_device_names(DEFAULT_DEVICE_NAMES).unwrap(),
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

mod resource_health {
    use std::time::Duration;
    use std::time::SystemTime;
    use std::time::UNIX_EPOCH;

    use tokio::sync::mpsc;

    use k8s_device_plugin_dra_core::DraDeviceHealth;
    use k8s_device_plugin_dra_core::ResourceHealthError;
    use k8s_device_plugin_dra_core::ResourceHealthReport;
    use k8s_device_plugin_dra_core::ResourceHealthReporter;
    use k8s_device_plugin_dra_core::ResourceHealthStatus;

    use super::*;

    #[async_trait]
    impl ResourceHealthReporter for StaticDraDriver {
        async fn watch_resource_health(
            &self,
            reports: mpsc::Sender<ResourceHealthReport>,
        ) -> Result<(), ResourceHealthError> {
            tracing::info!("kubelet opened resource health watch");
            // This validation backend has static fake devices. A real backend
            // must use its hardware monitor, independently of inventory health.
            loop {
                let seconds = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map_err(|error| ResourceHealthError::MonitorFailed(error.to_string()))?
                    .as_secs();
                let last_updated_time = i64::try_from(seconds)
                    .map_err(|error| ResourceHealthError::MonitorFailed(error.to_string()))?;
                let report = ResourceHealthReport {
                    devices: self
                        .devices
                        .iter()
                        .map(|device| DraDeviceHealth {
                            pool_name: self.pool.clone(),
                            device_name: device.name.clone(),
                            health: ResourceHealthStatus::Healthy,
                            last_updated_time,
                            health_check_timeout_seconds: 30,
                            message: "static validation device is healthy".into(),
                        })
                        .collect(),
                };
                if reports.send(report).await.is_err() {
                    return Ok(());
                }
                tokio::select! {
                    _ = reports.closed() => return Ok(()),
                    _ = tokio::time::sleep(Duration::from_secs(5)) => {},
                }
            }
        }
    }
}
