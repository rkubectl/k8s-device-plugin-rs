//! Authoritative `ResourceSlice` publisher.

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use k8s_device_plugin_core::AttributeValue;
use k8s_device_plugin_core::PoolDevice;
use k8s_device_plugin_core::ResourcePool;
use k8s_openapi::api::core::v1::Node;
use k8s_openapi::api::resource::v1::Device as WireDevice;
use k8s_openapi::api::resource::v1::DeviceAttribute;
use k8s_openapi::api::resource::v1::DeviceCapacity;
use k8s_openapi::api::resource::v1::ResourcePool as WireResourcePool;
use k8s_openapi::api::resource::v1::ResourceSlice;
use k8s_openapi::api::resource::v1::ResourceSliceSpec;
use k8s_openapi::apimachinery::pkg::api::resource::Quantity as WireQuantity;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::OwnerReference;
use kube::Api;
use kube::Client;
use kube::api::DeleteParams;
use kube::api::ListParams;
use kube::api::PostParams;
use tokio::task::JoinHandle;

/// Devices change far less often than a device's health status, so this
/// defaults meaningfully longer than `DevicePluginService`'s 5s health-poll
/// interval (`lib/src/lib.rs`).
const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(30);
const DEFAULT_RETRY_BASE_DELAY: Duration = Duration::from_secs(1);
const DEFAULT_RETRY_MAX_DELAY: Duration = Duration::from_secs(30);

/// Kubernetes limits a `ResourceSlice` to 128 ordinary devices.
const MAX_DEVICES_PER_SLICE: usize = 128;

fn slice_name(driver_name: &str, node_name: &str, pool_name: &str, index: usize) -> String {
    let base = format!("{driver_name}-{node_name}-{pool_name}");
    if index == 0 {
        base
    } else {
        format!("{base}-{index}")
    }
}

fn attribute_value_to_wire(value: &AttributeValue) -> DeviceAttribute {
    match value {
        AttributeValue::String(s) => DeviceAttribute {
            string: Some(s.clone()),
            ..Default::default()
        },
        AttributeValue::Int(i) => DeviceAttribute {
            int: Some(*i),
            ..Default::default()
        },
        AttributeValue::Bool(b) => DeviceAttribute {
            bool: Some(*b),
            ..Default::default()
        },
        AttributeValue::Version(v) => DeviceAttribute {
            version: Some(v.clone()),
            ..Default::default()
        },
    }
}

fn pool_device_to_wire(device: &PoolDevice) -> WireDevice {
    // Omit empty maps entirely: the API server normalizes empty optional
    // maps to None, and retaining Some({}) would cause endless updates.
    let attributes = (!device.attributes.is_empty()).then(|| {
        device
            .attributes
            .iter()
            .map(|(name, value)| (name.clone(), attribute_value_to_wire(value)))
            .collect()
    });
    let capacity = (!device.capacity.is_empty()).then(|| {
        device
            .capacity
            .iter()
            .map(|(name, quantity)| {
                (
                    name.clone(),
                    DeviceCapacity {
                        request_policy: None,
                        value: WireQuantity(quantity.0.to_string()),
                    },
                )
            })
            .collect()
    });
    WireDevice {
        name: device.name.clone(),
        attributes,
        capacity,
        ..Default::default()
    }
}

/// Returns devices in the stable order used for comparison and publication.
fn canonicalize_devices(mut devices: Vec<WireDevice>) -> Vec<WireDevice> {
    devices.sort_by(|left, right| left.name.cmp(&right.name));
    devices
}

/// Reconciles a [`ResourcePool`]'s complete snapshot against the API server.
///
/// The publisher owns all slices for its driver and node. One publication
/// takes one full snapshot, publishes every desired slice before deleting
/// stale ones, and keeps generation/count coherent across split slices.
pub struct ResourceSlicePublisher {
    client: Client,
    driver_name: String,
    node_name: String,
    resource_pool: Arc<dyn ResourcePool>,
    poll_interval: Duration,
    retry_base_delay: Duration,
    retry_max_delay: Duration,
}

impl fmt::Debug for ResourceSlicePublisher {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ResourceSlicePublisher")
            .finish_non_exhaustive()
    }
}

impl ResourceSlicePublisher {
    pub fn new<P: ResourcePool + 'static>(
        client: Client,
        driver_name: impl Into<String>,
        node_name: impl Into<String>,
        resource_pool: P,
    ) -> Self {
        Self {
            client,
            driver_name: driver_name.into(),
            node_name: node_name.into(),
            resource_pool: Arc::new(resource_pool),
            poll_interval: DEFAULT_POLL_INTERVAL,
            retry_base_delay: DEFAULT_RETRY_BASE_DELAY,
            retry_max_delay: DEFAULT_RETRY_MAX_DELAY,
        }
    }

    /// Overrides the normal interval between successful reconciliations.
    #[must_use]
    pub fn with_poll_interval(mut self, poll_interval: Duration) -> Self {
        self.poll_interval = poll_interval;
        self
    }

    /// Configures the bounded exponential delay used after failed attempts.
    #[must_use]
    pub fn with_retry_backoff(mut self, base_delay: Duration, max_delay: Duration) -> Self {
        self.retry_base_delay = base_delay;
        self.retry_max_delay = max_delay.max(base_delay);
        self
    }

    /// Reconciles the complete snapshot once.
    ///
    /// Pools absent from [`ResourcePool::devices`] and pools with no devices
    /// have no desired slices, so their existing slices are deleted.
    pub async fn publish_once(&self) -> kube::Result<()> {
        let owner = self.owner_reference().await?;
        let api: Api<ResourceSlice> = Api::all(self.client.clone());
        let existing = api
            .list(&ListParams::default().fields(&format!(
                "spec.driver={},spec.nodeName={}",
                self.driver_name, self.node_name
            )))
            .await?;
        let mut existing = existing
            .items
            // A proxy or test server may ignore field selectors. Never let
            // that turn an authoritative reconciliation into cross-node GC.
            .into_iter()
            .filter(|slice| {
                slice.spec.driver == self.driver_name
                    && slice.spec.node_name.as_deref() == Some(&self.node_name)
            })
            .filter_map(|slice| slice.metadata.name.clone().map(|name| (name, slice)))
            .collect::<BTreeMap<_, _>>();

        for (pool_name, devices) in self.resource_pool.devices().await {
            self.reconcile_pool(&api, &owner, &mut existing, &pool_name, &devices)
                .await?;
        }

        // Desired slices were created or updated first. Deleting the
        // leftovers last avoids a deliberate incomplete pool state.
        for (name, _) in existing {
            api.delete(&name, &DeleteParams::default()).await?;
        }
        Ok(())
    }

    async fn owner_reference(&self) -> kube::Result<OwnerReference> {
        let api: Api<Node> = Api::all(self.client.clone());
        let node = api.get(&self.node_name).await?;
        let uid = node.metadata.uid.ok_or_else(|| {
            kube::Error::Api(
                kube::core::Status::failure("node object is missing a UID", "Invalid")
                    .with_code(422)
                    .boxed(),
            )
        })?;
        Ok(OwnerReference {
            api_version: "v1".to_string(),
            kind: "Node".to_string(),
            name: self.node_name.clone(),
            uid,
            controller: Some(true),
            block_owner_deletion: Some(false),
        })
    }

    async fn reconcile_pool(
        &self,
        api: &Api<ResourceSlice>,
        owner: &OwnerReference,
        existing: &mut BTreeMap<String, ResourceSlice>,
        pool_name: &str,
        devices: &[PoolDevice],
    ) -> kube::Result<()> {
        let wire_devices = canonicalize_devices(devices.iter().map(pool_device_to_wire).collect());
        if wire_devices
            .windows(2)
            .any(|pair| pair[0].name == pair[1].name)
        {
            return Err(invalid_pool_error(
                pool_name,
                "contains duplicate device names",
            ));
        }

        let chunks = wire_devices
            .chunks(MAX_DEVICES_PER_SLICE)
            .map(<[WireDevice]>::to_vec)
            .collect::<Vec<_>>();
        if chunks.is_empty() {
            return Ok(());
        }
        let pool_existing = existing
            .iter()
            .filter(|(_, slice)| slice.spec.pool.name == pool_name)
            .collect::<Vec<_>>();
        let generation = pool_generation(
            &pool_existing,
            &self.driver_name,
            &self.node_name,
            pool_name,
            &chunks,
        )?;
        let slice_count = i64::try_from(chunks.len())
            .map_err(|_| invalid_pool_error(pool_name, "requires too many ResourceSlices"))?;

        for (index, devices) in chunks.into_iter().enumerate() {
            let name = slice_name(&self.driver_name, &self.node_name, pool_name, index);
            let desired = ResourceSlice {
                metadata: ObjectMeta {
                    name: Some(name.clone()),
                    owner_references: Some(vec![owner.clone()]),
                    ..Default::default()
                },
                spec: ResourceSliceSpec {
                    driver: self.driver_name.clone(),
                    node_name: Some(self.node_name.clone()),
                    pool: WireResourcePool {
                        name: pool_name.to_string(),
                        generation,
                        resource_slice_count: slice_count,
                    },
                    devices: Some(devices),
                    ..Default::default()
                },
            };

            match existing.remove(&name) {
                Some(current) if resource_slice_matches(&current, &desired) => {}
                Some(mut current) => {
                    current.metadata.owner_references = desired.metadata.owner_references;
                    current.spec = desired.spec;
                    api.replace(&name, &PostParams::default(), &current).await?;
                }
                None => {
                    api.create(&PostParams::default(), &desired).await?;
                }
            }
        }
        Ok(())
    }

    /// Publishes once immediately, then re-publishes every poll interval.
    /// Failures use a bounded exponential delay and success resets it.
    pub fn spawn(self) -> JoinHandle<()> {
        tokio::spawn(async move {
            let mut failed_attempts = 0_u32;
            loop {
                let delay = match self.publish_once().await {
                    Ok(()) => {
                        failed_attempts = 0;
                        self.poll_interval
                    }
                    Err(err) => {
                        failed_attempts = failed_attempts.saturating_add(1);
                        let delay = retry_delay(
                            self.retry_base_delay,
                            self.retry_max_delay,
                            failed_attempts,
                        );
                        tracing::warn!(%err, ?delay, "failed to reconcile ResourceSlices");
                        delay
                    }
                };
                tokio::time::sleep(delay).await;
            }
        })
    }
}

fn invalid_pool_error(pool_name: &str, reason: &str) -> kube::Error {
    let message = format!("ResourcePool {pool_name} {reason}");
    kube::Error::Api(
        kube::core::Status::failure(&message, "Invalid")
            .with_code(422)
            .boxed(),
    )
}

fn resource_slice_matches(current: &ResourceSlice, desired: &ResourceSlice) -> bool {
    let mut current_spec = current.spec.clone();
    current_spec.devices = current_spec.devices.map(canonicalize_devices);
    current_spec == desired.spec
        && current.metadata.owner_references == desired.metadata.owner_references
}

fn pool_generation(
    existing: &[(&String, &ResourceSlice)],
    driver_name: &str,
    node_name: &str,
    pool_name: &str,
    desired_chunks: &[Vec<WireDevice>],
) -> kube::Result<i64> {
    let expected = desired_chunks
        .iter()
        .enumerate()
        .map(|(index, devices)| {
            (
                slice_name(driver_name, node_name, pool_name, index),
                devices,
            )
        })
        .collect::<BTreeMap<_, _>>();
    let generations = existing
        .iter()
        .map(|(_, slice)| slice.spec.pool.generation)
        .collect::<BTreeSet<_>>();
    let expected_count = i64::try_from(desired_chunks.len())
        .map_err(|_| invalid_pool_error(pool_name, "requires too many ResourceSlices"))?;
    let unchanged = !expected.is_empty()
        && existing.len() == expected.len()
        && generations.len() == 1
        && existing.iter().all(|(name, slice)| {
            expected.get(*name).is_some_and(|devices| {
                slice.spec.pool.resource_slice_count == expected_count
                    && slice.spec.devices.clone().map(canonicalize_devices)
                        == Some((*devices).clone())
            })
        });
    if unchanged && let Some(generation) = generations.first() {
        return Ok(*generation);
    }

    let current = generations.into_iter().max().unwrap_or(0);
    current
        .checked_add(1)
        .ok_or_else(|| invalid_pool_error(pool_name, "generation cannot be incremented"))
}

fn retry_delay(base: Duration, max: Duration, failed_attempts: u32) -> Duration {
    let shift = failed_attempts.saturating_sub(1).min(31);
    let multiplier = 1_u32.checked_shl(shift).unwrap_or(u32::MAX);
    base.saturating_mul(multiplier).min(max)
}

#[cfg(test)]
mod tests;
