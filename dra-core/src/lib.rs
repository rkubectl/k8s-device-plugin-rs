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
pub use validation::ValidationError;
pub use validation::is_valid_counter_name;
pub use validation::validate_driver_name;
pub use validation::validate_node_name;
pub use validation::validate_pool_name;

mod dra;
mod validation;

fn default<T: Default>() -> T {
    T::default()
}
