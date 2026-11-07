//! Dynamic Resource Allocation (DRA) driver runtime.
//!
//! Phase 1 scope: a pluginwatcher-based `Registration` server, the
//! `DRAPlugin` gRPC service (`NodePrepareResources`/
//! `NodeUnprepareResources`), a `ResourceClaim` resolver, and a
//! authoritative `ResourceSlice` publisher — wired together by a
//! `DraPlugin::run` lifecycle harness. See `docs/dra-design.md` for the
//! full design and phasing.

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::fmt;
use std::fs;
use std::io;
use std::sync::Arc;
use std::time::Duration;

use camino::Utf8Path;
use camino::Utf8PathBuf;
use serde_json as json;
use tokio::net;
use tokio::task;
use tonic::transport;

use k8s_openapi::api::core::v1 as corev1;
use k8s_openapi::api::resource::v1 as resourcev1;
use k8s_openapi::api::resource::v1alpha3 as resourcev1alpha3;
use k8s_openapi::apimachinery::pkg::api::resource as apiresource;
use k8s_openapi::apimachinery::pkg::apis::meta::v1 as metav1;
use kube::api;

use k8s_device_plugin_dra_core::AllocatedDevice;
use k8s_device_plugin_dra_core::AttributeValue;
use k8s_device_plugin_dra_core::ClaimPreparer;
use k8s_device_plugin_dra_core::ClaimRef;
use k8s_device_plugin_dra_core::DeviceName;
use k8s_device_plugin_dra_core::DraDriver;
use k8s_device_plugin_dra_core::DriverName;
use k8s_device_plugin_dra_core::InventoryDevice;
use k8s_device_plugin_dra_core::NodeName;
#[cfg(test)]
use k8s_device_plugin_dra_core::PoolDevice;
use k8s_device_plugin_dra_core::PoolInventory;
use k8s_device_plugin_dra_core::PoolName;
use k8s_device_plugin_dra_core::PrepareError;
use k8s_device_plugin_dra_core::PreparedDevice;
use k8s_device_plugin_dra_core::ResolvedClaim;
use k8s_device_plugin_dra_core::ResourceHealthReporter;
use k8s_device_plugin_dra_core::ResourcePool;

use k8s_device_plugin_proto::dra;

pub use claim::ClaimResolver;
pub use device_status::ClaimDeviceStatus;
pub use device_status::ClaimDeviceStatusError;
pub use device_status::ClaimDeviceStatusKey;
pub use device_status::ClaimDeviceStatusPublishOutcome;
pub use device_status::ClaimDeviceStatusPublisher;
pub use ext::KubeClientExt;
pub use health::DraPluginLivenessProbe;
pub use metadata::DeviceMetadataOptions;
pub use pool_status::PoolStatusClient;
pub use pool_status::PoolStatusError;
pub use pool_status::PoolStatusQuery;
pub use registration::DraRegistrationServer;
pub use resource_health::DraResourceHealthService;
pub use resourceslice::ResourceSlicePublisher;
pub use service::DraPluginService;

mod cdi;
mod claim;
mod device_status;
mod ext;
mod health;
mod metadata;
mod pool_status;
mod registration;
mod resource_health;
mod resourceslice;
mod service;

/// A mismatch between the configured and backend driver identities.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("runtime driver name {runtime:?} differs from backend driver name {backend:?}")]
pub struct DriverIdentityMismatch {
    pub runtime: DriverName,
    pub backend: DriverName,
}

fn component_error(
    component: &str,
    result: Result<Result<(), transport::Error>, task::JoinError>,
) -> io::Error {
    match result {
        Ok(Ok(())) => io::Error::other(format!("{component} exited unexpectedly")),
        Ok(Err(err)) => io::Error::other(format!("{component} failed: {err}")),
        Err(join_err) => io::Error::other(format!("{component} panicked: {join_err}")),
    }
}

fn publisher_error(result: Result<(), task::JoinError>) -> io::Error {
    match result {
        Ok(()) => io::Error::other("ResourceSlice publisher exited unexpectedly"),
        Err(join_err) => io::Error::other(format!("ResourceSlice publisher panicked: {join_err}")),
    }
}

/// Waits for the first component that exits, then stops and reaps the
/// remaining components before returning its error. Dropping a
/// [`task::JoinHandle`] detaches its task, so each losing branch must be aborted
/// explicitly instead of relying on normal scope cleanup.
async fn wait_for_component_exit(
    mut registration_handle: task::JoinHandle<Result<(), transport::Error>>,
    mut service_handle: task::JoinHandle<Result<(), transport::Error>>,
    mut publisher_handle: task::JoinHandle<()>,
) -> io::Result<()> {
    tokio::select! {
        result = &mut registration_handle => {
            service_handle.abort();
            publisher_handle.abort();
            let _ = tokio::join!(service_handle, publisher_handle);
            Err(component_error("registration server", result))
        }
        result = &mut service_handle => {
            registration_handle.abort();
            publisher_handle.abort();
            let _ = tokio::join!(registration_handle, publisher_handle);
            Err(component_error("DRAPlugin service", result))
        }
        result = &mut publisher_handle => {
            registration_handle.abort();
            service_handle.abort();
            let _ = tokio::join!(registration_handle, service_handle);
            Err(publisher_error(result))
        }
    }
}

/// Wires the registration server, `DRAPlugin` gRPC service, and
/// `ResourceSlice` publisher together into the same "implement one trait,
/// call `.run()`" experience `k8s_device_plugin_lib`'s `DevicePlugin`
/// gives classic device-plugin authors -- referenced here only for
/// orientation, not a dependency of this crate.
#[derive(Debug)]
pub struct DraPlugin {
    driver_name: DriverName,
    registration: DraRegistrationServer,
    service: DraPluginService,
    publisher: ResourceSlicePublisher,
}

impl DraPlugin {
    /// Constructs a runtime with an injected client, node and backend.
    /// `driver_name` must agree with [`DraDriver::driver_name`].
    ///
    /// # Errors
    ///
    /// Returns an error when the backend driver identity differs from the supplied name.
    pub fn new<D: DraDriver + 'static>(
        client: kube::Client,
        driver_name: DriverName,
        node_name: NodeName,
        driver: D,
    ) -> Result<Self, DriverIdentityMismatch> {
        Self::from_driver(client, driver_name, node_name, Arc::new(driver))
    }

    fn from_driver<D: DraDriver + 'static>(
        client: kube::Client,
        driver_name: DriverName,
        node_name: NodeName,
        driver: Arc<D>,
    ) -> Result<Self, DriverIdentityMismatch> {
        if driver.driver_name() != &driver_name {
            return Err(DriverIdentityMismatch {
                runtime: driver_name,
                backend: driver.driver_name().clone(),
            });
        }
        let plugin_endpoint = service::plugin_socket_path(driver_name.as_str());
        let registration = DraRegistrationServer::new(&driver_name, &plugin_endpoint);
        let resolver = ClaimResolver::new(client.clone(), driver_name.clone());
        let service = DraPluginService::new(resolver, Arc::clone(&driver));
        let publisher = ResourceSlicePublisher::new(client, driver_name.clone(), node_name, driver);
        Ok(Self {
            driver_name,
            registration,
            service,
            publisher,
        })
    }

    /// Enables read-only container metadata using an explicitly selected
    /// protocol. The default lifecycle creates no metadata artifacts.
    pub fn with_device_metadata(mut self, options: DeviceMetadataOptions) -> io::Result<Self> {
        self.service = self
            .service
            .with_device_metadata(self.driver_name.clone(), options)?;
        Ok(self)
    }

    /// Creates a DRA plugin that also serves the opt-in resource-health
    /// protocol. The backend health monitor is intentionally independent of
    /// the `ResourcePool` inventory snapshot.
    ///
    /// # Errors
    ///
    /// Returns the same identity validation errors as [`Self::new`].
    pub fn with_resource_health<D: DraDriver + ResourceHealthReporter + 'static>(
        client: kube::Client,
        driver_name: DriverName,
        node_name: NodeName,
        driver: D,
    ) -> Result<Self, DriverIdentityMismatch> {
        let driver = Arc::new(driver);
        let mut plugin = Self::from_driver(client, driver_name, node_name, Arc::clone(&driver))?;
        plugin.service = plugin.service.with_resource_health(driver);
        plugin.registration = plugin.registration.with_resource_health();
        Ok(plugin)
    }

    #[cfg(test)]
    fn for_test<D: DraDriver + 'static>(
        client: kube::Client,
        driver_name: DriverName,
        node_name: NodeName,
        driver: D,
        registration_socket_path: Utf8PathBuf,
    ) -> Self {
        let mut plugin =
            Self::new(client, driver_name, node_name, driver).expect("valid test driver");
        plugin.registration = DraRegistrationServer::for_test(
            &plugin.driver_name,
            service::plugin_socket_path(plugin.driver_name.as_str()),
            registration_socket_path,
        );
        plugin
    }

    /// Spawns the registration server, the `DRAPlugin` gRPC service, and
    /// the `ResourceSlice` publisher as concurrent tasks, creating
    /// `/var/lib/kubelet/plugins_registry/` and
    /// `/var/lib/kubelet/plugins/<driver_name>/` first if either is
    /// missing -- unlike `/var/lib/kubelet/device-plugins/`, which kubelet
    /// itself guarantees exists, these are the driver's own responsibility.
    /// Returns an error naming whichever component exited or panicked
    /// first, after aborting and reaping the remaining component tasks.
    /// There is no active re-registration retry loop to run here (unlike
    /// `DevicePlugin::run()`) since plugin-watcher registration is passive
    /// from the plugin's side -- see [`DraRegistrationServer`].
    ///
    /// No graceful-shutdown/socket-cleanup logic on process termination:
    /// the classic plugin (`DevicePlugin::run`, `example/src/main.rs`) has
    /// none either, relying on stale-socket removal at the next bind --
    /// matched here rather than introducing behavior the sibling
    /// implementation doesn't have. Likewise, the published
    /// `ResourceSlice` is deliberately left for the Node-owner-reference GC
    /// path rather than deleted here: a killed (not terminated) process
    /// would leave it either way, so explicit delete-on-graceful-shutdown
    /// only covers a subset of exits for no real benefit.
    /// This is a process-lifetime future: dropping it does not abort the
    /// component tasks. Terminate the process to shut down the plugin.
    pub async fn run(self) -> io::Result<()> {
        let plugin_dir = Utf8Path::new(dra::KUBELET_PLUGINS_PATH).join(self.driver_name.as_str());
        tokio::fs::create_dir_all(dra::KUBELET_PLUGINS_REGISTRY_PATH).await?;
        tokio::fs::create_dir_all(&plugin_dir).await?;
        self.run_at(&plugin_dir.join("plugin.sock")).await
    }

    /// Core of `run()`, parameterized on the plugin socket path so tests
    /// can point it at a temp directory instead of the real
    /// `/var/lib/kubelet/plugins/<driver_name>/` -- same split
    /// `DraPluginService::spawn`/`spawn_at` already uses.
    async fn run_at(self, plugin_socket_path: &Utf8Path) -> io::Result<()> {
        let service_handle = self.service.spawn_at(plugin_socket_path).await?;
        let registration_handle = match self.registration.spawn().await {
            Ok(handle) => handle,
            Err(err) => {
                service_handle.abort();
                let _ = service_handle.await;
                return Err(err);
            }
        };
        let publisher_handle = self.publisher.spawn();

        wait_for_component_exit(registration_handle, service_handle, publisher_handle).await
    }
}

fn default<T: Default>() -> T {
    T::default()
}

#[cfg(test)]
mod tests;
