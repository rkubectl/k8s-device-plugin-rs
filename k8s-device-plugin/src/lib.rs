#![doc = include_str!("../README.md")]

pub use k8s_device_plugin_core as core;
pub use k8s_device_plugin_core::*;

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
pub use k8s_device_plugin_dra::DraPlugin;
#[cfg(feature = "dra")]
pub use k8s_device_plugin_dra::DraPluginLivenessProbe;
#[cfg(feature = "dra")]
pub use k8s_device_plugin_dra::DraPluginService;
#[cfg(feature = "dra")]
pub use k8s_device_plugin_dra::DraRegistrationServer;
#[cfg(feature = "resource-health")]
pub use k8s_device_plugin_dra::DraResourceHealthService;
#[cfg(feature = "dra")]
pub use k8s_device_plugin_dra::ResourceSlicePublisher;

#[cfg(feature = "proto")]
pub use k8s_device_plugin_proto as proto;
