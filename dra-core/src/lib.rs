use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use k8s_device_plugin_core::Health;

pub use dra::AllocatedDevice;
pub use dra::AttributeValue;
pub use dra::ClaimPreparer;
pub use dra::ClaimRef;
pub use dra::CounterSet;
pub use dra::DeviceBinding;
pub use dra::DeviceCounterConsumption;
pub use dra::DeviceTaint;
pub use dra::DeviceTaintEffect;
pub use dra::DraDeviceHealth;
pub use dra::DraDriver;
pub use dra::InventoryDevice;
pub use dra::PoolDevice;
pub use dra::PoolInventory;
pub use dra::PrepareError;
pub use dra::PreparedDevice;
pub use dra::PreparedDeviceWithMetadata;
pub use dra::Quantity;
pub use dra::QuantityParseError;
pub use dra::ResolvedClaim;
pub use dra::ResourceHealthError;
pub use dra::ResourceHealthReport;
pub use dra::ResourceHealthReporter;
pub use dra::ResourceHealthStatus;
pub use dra::ResourcePool;
pub use identity::CounterName;
pub use identity::CounterNameError;
pub use identity::CounterSetName;
pub use identity::CounterSetNameError;
pub use identity::DeviceName;
pub use identity::DeviceNameError;
pub use identity::DriverName;
pub use identity::DriverNameError;
pub use identity::NodeName;
pub use identity::NodeNameError;
pub use identity::PoolName;
pub use identity::PoolNameError;

mod dra;
mod identity;

fn default<T: Default>() -> T {
    T::default()
}
