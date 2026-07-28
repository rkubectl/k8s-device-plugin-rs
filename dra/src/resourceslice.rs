//! Single-slice `ResourceSlice` publisher — see beads issue 9uf.8.

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
use kube::api::PostParams;
use tokio::task::JoinHandle;

/// Devices change far less often than a device's health status, so this
/// defaults meaningfully longer than `DevicePluginService`'s 5s health-poll
/// interval (`lib/src/lib.rs`).
const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(30);

fn slice_name(driver_name: &str, node_name: &str, pool_name: &str) -> String {
    format!("{driver_name}-{node_name}-{pool_name}")
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
    // Omit empty attribute/capacity maps entirely (`None`) rather than an
    // empty `Some(BTreeMap::new())`: an object read back from the API
    // server never round-trips an empty optional map as `Some({})`, so
    // always setting `Some(...)` here would make every freshly-created
    // device compare unequal to itself on the next `publish_once`.
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

/// Diffs a [`ResourcePool`]'s device snapshot against the API server and
/// publishes one `ResourceSlice` per pool. Phase 1 scope: no splitting
/// across the ~128-device-per-slice limit, no workqueue, no mutation
/// cache -- deferred to Phase 2's port of upstream's
/// `resourceslice.Controller` (see `docs/dra-design.md`).
pub struct ResourceSlicePublisher {
    client: Client,
    driver_name: String,
    node_name: String,
    resource_pool: Arc<dyn ResourcePool>,
    poll_interval: Duration,
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
        }
    }

    /// Overrides the default interval at which the pool snapshot is
    /// re-published.
    #[must_use]
    pub fn with_poll_interval(mut self, poll_interval: Duration) -> Self {
        self.poll_interval = poll_interval;
        self
    }

    /// Publishes the current snapshot once: one `ResourceSlice` per pool
    /// key in [`ResourcePool::devices`], created if absent, updated in
    /// place if present and different, left untouched if unchanged.
    pub async fn publish_once(&self) -> kube::Result<()> {
        let owner = self.owner_reference().await?;
        for (pool_name, devices) in self.resource_pool.devices().await {
            self.reconcile_pool(&owner, &pool_name, &devices).await?;
        }
        Ok(())
    }

    async fn owner_reference(&self) -> kube::Result<OwnerReference> {
        let api: Api<Node> = Api::all(self.client.clone());
        let node = api.get(&self.node_name).await?;
        Ok(OwnerReference {
            api_version: "v1".to_string(),
            kind: "Node".to_string(),
            name: self.node_name.clone(),
            uid: node.metadata.uid.unwrap_or_default(),
            controller: Some(true),
            block_owner_deletion: Some(false),
        })
    }

    async fn reconcile_pool(
        &self,
        owner: &OwnerReference,
        pool_name: &str,
        devices: &[PoolDevice],
    ) -> kube::Result<()> {
        let api: Api<ResourceSlice> = Api::all(self.client.clone());
        let name = slice_name(&self.driver_name, &self.node_name, pool_name);
        let wire_devices: Vec<WireDevice> = devices.iter().map(pool_device_to_wire).collect();

        match api.get_opt(&name).await? {
            Some(mut existing) if existing.spec.devices.as_deref() != Some(&wire_devices) => {
                existing.spec.pool.generation += 1;
                existing.spec.devices = Some(wire_devices);
                api.replace(&name, &PostParams::default(), &existing)
                    .await?;
            }
            // Devices already match what's published -- no write needed.
            Some(_) => {}
            None => {
                let slice = ResourceSlice {
                    metadata: ObjectMeta {
                        name: Some(name),
                        owner_references: Some(vec![owner.clone()]),
                        ..Default::default()
                    },
                    spec: ResourceSliceSpec {
                        driver: self.driver_name.clone(),
                        node_name: Some(self.node_name.clone()),
                        pool: WireResourcePool {
                            name: pool_name.to_string(),
                            generation: 1,
                            resource_slice_count: 1,
                        },
                        devices: Some(wire_devices),
                        ..Default::default()
                    },
                };
                api.create(&PostParams::default(), &slice).await?;
            }
        }
        Ok(())
    }

    /// Publishes once immediately, then re-publishes every
    /// `poll_interval` for as long as the returned task runs. A single
    /// failed publish attempt is logged and does not stop the loop -- a
    /// transient API server hiccup shouldn't permanently stop advertising
    /// this node's devices.
    pub fn spawn(self) -> JoinHandle<()> {
        tokio::spawn(async move {
            loop {
                if let Err(err) = self.publish_once().await {
                    tracing::warn!(%err, "failed to publish ResourceSlice");
                }
                tokio::time::sleep(self.poll_interval).await;
            }
        })
    }
}

#[cfg(test)]
mod tests;
