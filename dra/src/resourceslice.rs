//! Authoritative `ResourceSlice` publisher.

use kube::Resource as _;

use super::*;

/// Devices change far less often than a device's health status, so this
/// defaults meaningfully longer than `DevicePluginService`'s 5s health-poll
/// interval (`lib/src/lib.rs`).
const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(30);
const DEFAULT_RETRY_BASE_DELAY: Duration = Duration::from_secs(1);
const DEFAULT_RETRY_MAX_DELAY: Duration = Duration::from_secs(30);

/// Kubernetes limits a `ResourceSlice` to 128 ordinary devices.
const MAX_DEVICES_PER_SLICE: usize = 128;
const MAX_DEVICES_PER_TAINTED_SLICE: usize = 64;
const MAX_TAINTS_PER_DEVICE: usize = 16;

fn slice_name(driver_name: &str, node_name: &str, pool_name: &str, index: usize) -> String {
    let base = format!("{driver_name}-{node_name}-{pool_name}");
    if index == 0 {
        base
    } else {
        format!("{base}-{index}")
    }
}

fn attribute_value_to_wire(value: &AttributeValue) -> resourcev1::DeviceAttribute {
    match value {
        AttributeValue::String(s) => resourcev1::DeviceAttribute {
            string: Some(s.clone()),
            ..default()
        },
        AttributeValue::Int(i) => resourcev1::DeviceAttribute {
            int: Some(*i),
            ..default()
        },
        AttributeValue::Bool(b) => resourcev1::DeviceAttribute {
            bool: Some(*b),
            ..default()
        },
        AttributeValue::Version(v) => resourcev1::DeviceAttribute {
            version: Some(v.clone()),
            ..default()
        },
    }
}

fn pool_device_to_wire(device: &PoolDevice) -> resourcev1::Device {
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
                    resourcev1::DeviceCapacity {
                        request_policy: None,
                        value: K8sQuantity(quantity.0.to_string()),
                    },
                )
            })
            .collect()
    });
    resourcev1::Device {
        name: device.name.clone(),
        attributes,
        capacity,
        ..default()
    }
}

fn inventory_device_to_wire(
    inventory: &InventoryDevice,
    pool_name: &str,
) -> kube::Result<resourcev1::Device> {
    if inventory.taints.len() > MAX_TAINTS_PER_DEVICE {
        return Err(invalid_pool_error(
            pool_name,
            "has a device with more than 16 taints",
        ));
    }
    let mut identities = BTreeSet::new();
    let taints = inventory
        .taints
        .iter()
        .map(|taint| {
            if !identities.insert((&taint.key, taint.effect.as_str())) {
                return Err(invalid_pool_error(
                    pool_name,
                    "has duplicate device taint keys/effects",
                ));
            }
            let time_added = taint
                .time_added
                .map(|seconds| {
                    k8s_openapi::jiff::Timestamp::from_second(seconds)
                        .map(metav1::Time)
                        .map_err(|_| {
                            invalid_pool_error(pool_name, "has an invalid taint timestamp")
                        })
                })
                .transpose()?;
            Ok(resourcev1::DeviceTaint {
                key: taint.key.clone(),
                value: (!taint.value.is_empty()).then(|| taint.value.clone()),
                effect: taint.effect.as_str().to_string(),
                time_added,
            })
        })
        .collect::<kube::Result<Vec<_>>>()?;
    Ok(resourcev1::Device {
        taints: (!taints.is_empty()).then_some(taints),
        ..pool_device_to_wire(&inventory.device)
    })
}

/// Returns devices in the stable order used for comparison and publication.
fn canonicalize_devices(mut devices: Vec<resourcev1::Device>) -> Vec<resourcev1::Device> {
    for device in &mut devices {
        if let Some(taints) = &mut device.taints {
            for taint in taints.iter_mut() {
                if taint.value.as_deref() == Some("") {
                    taint.value = None;
                }
            }
            taints.sort_by(|left, right| {
                (&left.key, &left.effect, &left.value).cmp(&(
                    &right.key,
                    &right.effect,
                    &right.value,
                ))
            });
            if taints.is_empty() {
                device.taints = None;
            }
        }
    }
    devices.sort_by(|left, right| left.name.cmp(&right.name));
    devices
}

/// Keep the API server's defaulted timestamp for an unchanged taint. Replacing
/// it on each poll would reset toleration deadlines and continuously republish.
fn preserve_taint_times(
    desired: &mut [resourcev1::Device],
    existing: &[(&String, &resourcev1::ResourceSlice)],
) {
    let mut current_devices = BTreeMap::new();
    for (_, slice) in existing {
        for device in slice.spec.devices.iter().flatten() {
            let current = current_devices
                .entry(&device.name)
                .or_insert((slice.spec.pool.generation, device));
            if slice.spec.pool.generation > current.0 {
                *current = (slice.spec.pool.generation, device);
            }
        }
    }
    for device in desired {
        let Some((_, current)) = current_devices.get(&device.name) else {
            continue;
        };
        for taint in device.taints.iter_mut().flatten() {
            if taint.time_added.is_none()
                && let Some(previous) = current.taints.iter().flatten().find(|previous| {
                    previous.key == taint.key
                        && previous.effect == taint.effect
                        && previous.value.as_deref().unwrap_or_default()
                            == taint.value.as_deref().unwrap_or_default()
                })
            {
                taint.time_added = previous.time_added.clone();
            }
        }
    }
}

/// Reconciles a [`ResourcePool`]'s complete snapshot against the API server.
///
/// The publisher owns all slices for its driver and node. One publication
/// takes one full snapshot, publishes every desired slice before deleting
/// stale ones, and keeps generation/count coherent across split slices.
pub struct ResourceSlicePublisher {
    client: kube::Client,
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
        client: kube::Client,
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
    /// Pools absent from [`ResourcePool::inventory`] and pools with no devices
    /// have no desired slices, so their existing slices are deleted.
    pub async fn publish_once(&self) -> kube::Result<()> {
        let owner = self.owner_reference().await?;
        let resource_slices = self.client.resource_slices();
        let lp = api::ListParams::default().fields(&format!(
            "spec.driver={},spec.nodeName={}",
            self.driver_name, self.node_name
        ));
        let existing = resource_slices.list(&lp).await?;
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

        for (pool_name, devices) in self.resource_pool.inventory().await {
            self.reconcile_pool(
                &resource_slices,
                &owner,
                &mut existing,
                &pool_name,
                &devices,
            )
            .await?;
        }

        // Desired slices were created or updated first. Deleting the
        // leftovers last avoids a deliberate incomplete pool state.
        let dp = api::DeleteParams::default();
        for (name, _) in existing {
            resource_slices.delete(&name, &dp).await?;
        }
        Ok(())
    }

    async fn owner_reference(&self) -> kube::Result<metav1::OwnerReference> {
        let api = self.client.nodes();
        let node = api.get(&self.node_name).await?;
        node.owner_ref(&())
            .map(|owner_ref| metav1::OwnerReference {
                controller: Some(true),
                block_owner_deletion: Some(false),
                ..owner_ref
            })
            .ok_or_else(|| {
                kube::Error::Api(
                    kube::core::Status::failure("node object is missing a UID", "Invalid")
                        .with_code(422)
                        .boxed(),
                )
            })
    }

    async fn reconcile_pool(
        &self,
        api: &api::Api<resourcev1::ResourceSlice>,
        owner: &metav1::OwnerReference,
        existing: &mut BTreeMap<String, resourcev1::ResourceSlice>,
        pool_name: &str,
        devices: &[InventoryDevice],
    ) -> kube::Result<()> {
        let wire_devices = devices
            .iter()
            .map(|device| inventory_device_to_wire(device, pool_name))
            .collect::<kube::Result<Vec<_>>>()?;
        let mut wire_devices = canonicalize_devices(wire_devices);
        if wire_devices
            .windows(2)
            .any(|pair| pair[0].name == pair[1].name)
        {
            return Err(invalid_pool_error(
                pool_name,
                "contains duplicate device names",
            ));
        }

        let pool_existing = existing
            .iter()
            .filter(|(_, slice)| slice.spec.pool.name == pool_name)
            .collect::<Vec<_>>();
        preserve_taint_times(&mut wire_devices, &pool_existing);
        let chunk_size = if wire_devices.iter().any(|device| device.taints.is_some()) {
            MAX_DEVICES_PER_TAINTED_SLICE
        } else {
            MAX_DEVICES_PER_SLICE
        };
        let chunks = wire_devices
            .chunks(chunk_size)
            .map(<[resourcev1::Device]>::to_vec)
            .collect::<Vec<_>>();
        if chunks.is_empty() {
            return Ok(());
        }
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
            let desired = resourcev1::ResourceSlice {
                metadata: metav1::ObjectMeta {
                    name: Some(name.clone()),
                    owner_references: Some(vec![owner.clone()]),
                    ..default()
                },
                spec: resourcev1::ResourceSliceSpec {
                    driver: self.driver_name.clone(),
                    node_name: Some(self.node_name.clone()),
                    pool: resourcev1::ResourcePool {
                        name: pool_name.to_string(),
                        generation,
                        resource_slice_count: slice_count,
                    },
                    devices: Some(devices),
                    ..default()
                },
            };

            let pp = api::PostParams::default();
            match existing.remove(&name) {
                Some(current) if resource_slice_matches(&current, &desired) => {}
                Some(mut current) => {
                    current.metadata.owner_references = desired.metadata.owner_references;
                    current.spec = desired.spec;
                    api.replace(&name, &pp, &current).await?;
                }
                None => {
                    api.create(&pp, &desired).await?;
                }
            }
        }
        Ok(())
    }

    /// Publishes once immediately, then re-publishes every poll interval.
    /// Failures use a bounded exponential delay and success resets it.
    pub fn spawn(self) -> task::JoinHandle<()> {
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

fn resource_slice_matches(
    current: &resourcev1::ResourceSlice,
    desired: &resourcev1::ResourceSlice,
) -> bool {
    let mut current_spec = current.spec.clone();
    current_spec.devices = current_spec.devices.map(canonicalize_devices);
    current_spec == desired.spec
        && current.metadata.owner_references == desired.metadata.owner_references
}

fn pool_generation(
    existing: &[(&String, &resourcev1::ResourceSlice)],
    driver_name: &str,
    node_name: &str,
    pool_name: &str,
    desired_chunks: &[Vec<resourcev1::Device>],
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
