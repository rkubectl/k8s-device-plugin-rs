use super::PoolDevice;

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

/// One device in a complete inventory snapshot, with optional DRA policies.
///
/// This wrapper leaves the v0.1 [`PoolDevice`] layout intact. Existing
/// backends need only implement [`crate::ResourcePool::devices`]; backends using
/// taints override [`crate::ResourcePool::inventory`] to supply one coherent snapshot.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct InventoryDevice {
    pub device: PoolDevice,
    pub taints: Vec<DeviceTaint>,
}

impl InventoryDevice {
    #[must_use]
    pub fn new(device: PoolDevice) -> Self {
        Self {
            device,
            taints: Vec::new(),
        }
    }

    #[must_use]
    pub fn with_taint(mut self, taint: DeviceTaint) -> Self {
        self.taints.push(taint);
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
    use std::collections::BTreeMap;
    use std::sync::Arc;

    use async_trait::async_trait;

    use super::*;
    use crate::Health;
    use crate::ResourcePool;

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
    }
}
