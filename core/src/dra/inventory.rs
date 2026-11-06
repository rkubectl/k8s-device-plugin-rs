use super::*;

/// Shared capacity available to overlapping devices in one pool.
/// Names are DNS labels; each set contains 1–32 integer counters.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct CounterSet {
    pub name: String,
    pub counters: BTreeMap<String, Quantity>,
}

impl CounterSet {
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            counters: BTreeMap::new(),
        }
    }

    #[must_use]
    pub fn counter(mut self, name: impl Into<String>, value: Quantity) -> Self {
        self.counters.insert(name.into(), value);
        self
    }
}

/// Counters reserved when Kubernetes allocates this device.
/// Names reference a set and 1–32 counters defined in the same pool.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct DeviceCounterConsumption {
    pub counter_set: String,
    pub counters: BTreeMap<String, Quantity>,
}

impl DeviceCounterConsumption {
    #[must_use]
    pub fn new(counter_set: impl Into<String>) -> Self {
        Self {
            counter_set: counter_set.into(),
            counters: BTreeMap::new(),
        }
    }

    #[must_use]
    pub fn counter(mut self, name: impl Into<String>, value: Quantity) -> Self {
        self.counters.insert(name.into(), value);
        self
    }
}

/// One coherent pool snapshot, including devices and their shared counters.
#[derive(Clone, Debug, Default, PartialEq)]
#[non_exhaustive]
pub struct PoolInventory {
    pub devices: Vec<InventoryDevice>,
    pub shared_counters: Vec<CounterSet>,
}

impl PoolInventory {
    #[must_use]
    pub fn new(devices: Vec<InventoryDevice>) -> Self {
        Self {
            devices,
            ..default()
        }
    }

    #[must_use]
    pub fn with_counter_set(mut self, counters: CounterSet) -> Self {
        self.shared_counters.push(counters);
        self
    }
}

impl From<Vec<InventoryDevice>> for PoolInventory {
    fn from(devices: Vec<InventoryDevice>) -> Self {
        Self::new(devices)
    }
}

/// The action Kubernetes takes for claims which do not tolerate a device taint.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[non_exhaustive]
pub enum DeviceTaintEffect {
    /// Publish diagnostic information without restricting use.
    None,
    /// Prevent new allocations and scheduling onto the device.
    NoSchedule,
    /// Also evict existing consumers, subject to their claim tolerations.
    NoExecute,
}

impl DeviceTaintEffect {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "None",
            Self::NoSchedule => "NoSchedule",
            Self::NoExecute => "NoExecute",
        }
    }
}

/// An explicit backend policy for a device, independent of health reporting.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct DeviceTaint {
    pub key: String,
    pub value: String,
    pub effect: DeviceTaintEffect,
    /// Unix seconds. `None` lets Kubernetes assign the time when this taint
    /// is first published; the publisher preserves it on later snapshots.
    pub time_added: Option<i64>,
}

impl DeviceTaint {
    #[must_use]
    pub fn new(key: impl Into<String>, effect: DeviceTaintEffect) -> Self {
        Self {
            key: key.into(),
            value: String::new(),
            effect,
            time_added: None,
        }
    }

    #[must_use]
    pub fn with_value(mut self, value: impl Into<String>) -> Self {
        self.value = value.into();
        self
    }

    #[must_use]
    pub fn with_time_added(mut self, unix_seconds: i64) -> Self {
        self.time_added = Some(unix_seconds);
        self
    }
}

/// Conditions an external controller must report before Pod binding.
///
/// Readiness belongs in per-device ResourceClaim status before kubelet calls
/// prepare. Reporting it only from prepare would prevent the Pod from binding.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct DeviceBinding {
    pub conditions: Vec<String>,
    pub failure_conditions: Vec<String>,
    /// Pin the allocation to the scheduler's selected Kubernetes node.
    pub binds_to_node: bool,
}

impl DeviceBinding {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn with_condition(mut self, condition: impl Into<String>) -> Self {
        self.conditions.push(condition.into());
        self
    }

    #[must_use]
    pub fn with_failure_condition(mut self, condition: impl Into<String>) -> Self {
        self.failure_conditions.push(condition.into());
        self
    }

    #[must_use]
    pub fn with_node_binding(mut self) -> Self {
        self.binds_to_node = true;
        self
    }
}

/// One device in a complete inventory snapshot, with optional DRA policies.
///
/// This wrapper leaves the v0.1 [`PoolDevice`] layout intact. Existing
/// backends need only implement [`crate::ResourcePool::devices`]; backends using
/// taints override [`crate::ResourcePool::inventory`] to supply one coherent
/// snapshot. Backends using shared counters override
/// [`crate::ResourcePool::snapshot`] to include pool counter definitions.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct InventoryDevice {
    pub device: PoolDevice,
    pub taints: Vec<DeviceTaint>,
    pub binding: Option<DeviceBinding>,
    /// At most two entries, each referencing a different counter set.
    pub consumes_counters: Vec<DeviceCounterConsumption>,
}

impl InventoryDevice {
    #[must_use]
    pub fn new(device: PoolDevice) -> Self {
        Self {
            device,
            taints: Vec::new(),
            binding: None,
            consumes_counters: Vec::new(),
        }
    }

    #[must_use]
    pub fn with_taint(mut self, taint: DeviceTaint) -> Self {
        self.taints.push(taint);
        self
    }

    #[must_use]
    pub fn with_binding(mut self, binding: DeviceBinding) -> Self {
        self.binding = Some(binding);
        self
    }

    #[must_use]
    pub fn with_consumes_counters(mut self, consumption: DeviceCounterConsumption) -> Self {
        self.consumes_counters.push(consumption);
        self
    }
}

impl From<PoolDevice> for InventoryDevice {
    fn from(device: PoolDevice) -> Self {
        Self::new(device)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct LegacyPool;

    #[async_trait]
    impl ResourcePool for LegacyPool {
        async fn devices(&self) -> BTreeMap<String, Vec<PoolDevice>> {
            // Keep source compatibility for v0.1 consumers using literals.
            let device = PoolDevice {
                name: "widget-0".to_string(),
                attributes: BTreeMap::new(),
                capacity: BTreeMap::new(),
                health: Health::Healthy,
            };
            BTreeMap::from([("pool-0".to_string(), vec![device])])
        }
    }

    struct TaintedPool;

    #[async_trait]
    impl ResourcePool for TaintedPool {
        async fn devices(&self) -> BTreeMap<String, Vec<PoolDevice>> {
            LegacyPool.devices().await
        }

        async fn inventory(&self) -> BTreeMap<String, Vec<InventoryDevice>> {
            BTreeMap::from([(
                "pool-0".to_string(),
                vec![
                    InventoryDevice::new(PoolDevice::new("widget-0")).with_taint(DeviceTaint::new(
                        "example.com/maintenance",
                        DeviceTaintEffect::NoSchedule,
                    )),
                ],
            )])
        }
    }

    #[tokio::test]
    async fn legacy_backend_has_an_untainted_inventory_without_changes() {
        let inventory = LegacyPool.inventory().await;
        assert_eq!(inventory["pool-0"][0].device.name, "widget-0");
        assert!(inventory["pool-0"][0].taints.is_empty());
        let snapshot = LegacyPool.snapshot().await;
        assert_eq!(snapshot["pool-0"].devices, inventory["pool-0"]);
        assert!(snapshot["pool-0"].shared_counters.is_empty());
    }

    #[tokio::test]
    async fn arc_preserves_overridden_inventory_on_a_trait_object() {
        let pool: Arc<dyn ResourcePool> = Arc::new(TaintedPool);
        let inventory = pool.inventory().await;
        assert_eq!(inventory["pool-0"][0].taints.len(), 1);
        assert_eq!(
            inventory["pool-0"][0].taints[0].effect,
            DeviceTaintEffect::NoSchedule
        );
        assert_eq!(pool.snapshot().await["pool-0"].devices, inventory["pool-0"]);
    }

    struct PartitionablePool;

    #[async_trait]
    impl ResourcePool for PartitionablePool {
        async fn devices(&self) -> BTreeMap<String, Vec<PoolDevice>> {
            panic!("snapshot override must be delegated")
        }

        async fn snapshot(&self) -> BTreeMap<String, PoolInventory> {
            BTreeMap::from([(
                "pool-0".into(),
                PoolInventory::new(vec![
                    InventoryDevice::new(PoolDevice::new("card-0")).with_consumes_counters(
                        DeviceCounterConsumption::new("pair-a").counter("cards", Quantity(1)),
                    ),
                ])
                .with_counter_set(CounterSet::new("pair-a").counter("cards", Quantity(2))),
            )])
        }
    }

    #[tokio::test]
    async fn arc_delegates_a_coherent_partitionable_snapshot() {
        let pool: Arc<dyn ResourcePool> = Arc::new(PartitionablePool);
        let snapshot = pool.snapshot().await;
        let pool = &snapshot["pool-0"];
        assert_eq!(pool.shared_counters[0].counters["cards"], Quantity(2));
        assert_eq!(pool.devices[0].consumes_counters[0].counter_set, "pair-a");
        assert_eq!(
            pool.devices[0].consumes_counters[0].counters["cards"],
            Quantity(1)
        );
    }
}
