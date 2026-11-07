#![doc = include_str!("../README.md")]

pub use k8s_device_plugin_core as core;
pub use k8s_device_plugin_core::*;

#[cfg(feature = "dra")]
pub use k8s_device_plugin_dra_core as dra_core;
#[cfg(feature = "dra")]
pub use k8s_device_plugin_dra_core::AllocatedDevice;
#[cfg(feature = "dra")]
pub use k8s_device_plugin_dra_core::AttributeValue;
#[cfg(feature = "dra")]
pub use k8s_device_plugin_dra_core::ClaimPreparer;
#[cfg(feature = "dra")]
pub use k8s_device_plugin_dra_core::ClaimRef;
#[cfg(feature = "dra")]
pub use k8s_device_plugin_dra_core::CounterSet;
#[cfg(feature = "dra")]
pub use k8s_device_plugin_dra_core::DeviceBinding;
#[cfg(feature = "dra")]
pub use k8s_device_plugin_dra_core::DeviceCounterConsumption;
#[cfg(feature = "dra")]
pub use k8s_device_plugin_dra_core::DeviceTaint;
#[cfg(feature = "dra")]
pub use k8s_device_plugin_dra_core::DeviceTaintEffect;
#[cfg(feature = "dra")]
pub use k8s_device_plugin_dra_core::DraDeviceHealth;
#[cfg(feature = "dra")]
pub use k8s_device_plugin_dra_core::DraDriver;
#[cfg(feature = "dra")]
pub use k8s_device_plugin_dra_core::InventoryDevice;
#[cfg(feature = "dra")]
pub use k8s_device_plugin_dra_core::PoolDevice;
#[cfg(feature = "dra")]
pub use k8s_device_plugin_dra_core::PoolInventory;
#[cfg(feature = "dra")]
pub use k8s_device_plugin_dra_core::PrepareError;
#[cfg(feature = "dra")]
pub use k8s_device_plugin_dra_core::PreparedDevice;
#[cfg(feature = "dra")]
pub use k8s_device_plugin_dra_core::PreparedDeviceWithMetadata;
#[cfg(feature = "dra")]
pub use k8s_device_plugin_dra_core::Quantity;
#[cfg(feature = "dra")]
pub use k8s_device_plugin_dra_core::QuantityParseError;
#[cfg(feature = "dra")]
pub use k8s_device_plugin_dra_core::ResolvedClaim;
#[cfg(feature = "dra")]
pub use k8s_device_plugin_dra_core::ResourceHealthError;
#[cfg(feature = "dra")]
pub use k8s_device_plugin_dra_core::ResourceHealthReport;
#[cfg(feature = "dra")]
pub use k8s_device_plugin_dra_core::ResourceHealthReporter;
#[cfg(feature = "dra")]
pub use k8s_device_plugin_dra_core::ResourceHealthStatus;
#[cfg(feature = "dra")]
pub use k8s_device_plugin_dra_core::ResourcePool;
#[cfg(feature = "dra")]
pub use k8s_device_plugin_dra_core::ValidationError as DraValidationError;
#[cfg(feature = "dra")]
pub use k8s_device_plugin_dra_core::is_valid_counter_name;
#[cfg(feature = "dra")]
pub use k8s_device_plugin_dra_core::validate_driver_name;
#[cfg(feature = "dra")]
pub use k8s_device_plugin_dra_core::validate_node_name;
#[cfg(feature = "dra")]
pub use k8s_device_plugin_dra_core::validate_pool_name;

#[cfg(feature = "device-plugin")]
pub use k8s_device_plugin_lib as device_plugin;
#[cfg(feature = "device-plugin")]
pub use k8s_device_plugin_lib::DevicePlugin;
#[cfg(feature = "device-plugin")]
pub use k8s_device_plugin_lib::DevicePluginService;
#[cfg(feature = "device-plugin")]
pub use k8s_device_plugin_lib::RegistrationClient;
#[cfg(feature = "device-plugin")]
pub use k8s_device_plugin_lib::StaticDevicePlugin;

#[cfg(feature = "dra")]
pub use k8s_device_plugin_dra as dra;
#[cfg(feature = "dra")]
pub use k8s_device_plugin_dra::ClaimDeviceStatus;
#[cfg(feature = "dra")]
pub use k8s_device_plugin_dra::ClaimDeviceStatusError;
#[cfg(feature = "dra")]
pub use k8s_device_plugin_dra::ClaimDeviceStatusKey;
#[cfg(feature = "dra")]
pub use k8s_device_plugin_dra::ClaimDeviceStatusPublishOutcome;
#[cfg(feature = "dra")]
pub use k8s_device_plugin_dra::ClaimDeviceStatusPublisher;
#[cfg(feature = "dra")]
pub use k8s_device_plugin_dra::ClaimResolver;
#[cfg(feature = "dra")]
pub use k8s_device_plugin_dra::DeviceMetadataOptions;
#[cfg(feature = "dra")]
pub use k8s_device_plugin_dra::DraPlugin;
#[cfg(feature = "dra")]
pub use k8s_device_plugin_dra::DraPluginLivenessProbe;
#[cfg(feature = "dra")]
pub use k8s_device_plugin_dra::DraPluginService;
#[cfg(feature = "dra")]
pub use k8s_device_plugin_dra::DraRegistrationServer;
#[cfg(feature = "dra")]
pub use k8s_device_plugin_dra::DraResourceHealthService;
#[cfg(feature = "dra")]
pub use k8s_device_plugin_dra::PoolStatusClient;
#[cfg(feature = "dra")]
pub use k8s_device_plugin_dra::PoolStatusError;
#[cfg(feature = "dra")]
pub use k8s_device_plugin_dra::PoolStatusQuery;
#[cfg(feature = "dra")]
pub use k8s_device_plugin_dra::ResourceSlicePublisher;

#[cfg(feature = "proto")]
pub use k8s_device_plugin_proto as proto;
