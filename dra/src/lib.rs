//! Dynamic Resource Allocation (DRA) driver runtime.
//!
//! Phase 1 scope: a pluginwatcher-based `Registration` server, the
//! `DRAPlugin` gRPC service (`NodePrepareResources`/
//! `NodeUnprepareResources`), a `ResourceClaim` resolver, and a
//! authoritative `ResourceSlice` publisher — wired together by a
//! `DraPlugin::run` lifecycle harness. See `docs/dra-design.md` for the
//! full design and phasing.

use std::io;
use std::path::Path;
use std::sync::Arc;

use k8s_device_plugin_core::DraDriver;
use k8s_device_plugin_proto::dra::KUBELET_PLUGINS_PATH;
use k8s_device_plugin_proto::dra::KUBELET_PLUGINS_REGISTRY_PATH;
use kube::Client;
use tokio::task::JoinHandle;
use tonic::transport;

pub use claim::ClaimResolver;
pub use health::DraPluginLivenessProbe;
pub use registration::DraRegistrationServer;
pub use resourceslice::ResourceSlicePublisher;
pub use service::DraPluginService;

mod claim;
mod health;
mod registration;
mod resourceslice;
mod service;

fn component_error(
    component: &str,
    result: Result<Result<(), transport::Error>, tokio::task::JoinError>,
) -> io::Error {
    match result {
        Ok(Ok(())) => io::Error::other(format!("{component} exited unexpectedly")),
        Ok(Err(err)) => io::Error::other(format!("{component} failed: {err}")),
        Err(join_err) => io::Error::other(format!("{component} panicked: {join_err}")),
    }
}

fn publisher_error(result: Result<(), tokio::task::JoinError>) -> io::Error {
    match result {
        Ok(()) => io::Error::other("ResourceSlice publisher exited unexpectedly"),
        Err(join_err) => io::Error::other(format!("ResourceSlice publisher panicked: {join_err}")),
    }
}

/// Waits for the first component that exits, then stops and reaps the
/// remaining components before returning its error. Dropping a
/// [`JoinHandle`] detaches its task, so each losing branch must be aborted
/// explicitly instead of relying on normal scope cleanup.
async fn wait_for_component_exit(
    mut registration_handle: JoinHandle<Result<(), transport::Error>>,
    mut service_handle: JoinHandle<Result<(), transport::Error>>,
    mut publisher_handle: JoinHandle<()>,
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
    driver_name: String,
    registration: DraRegistrationServer,
    service: DraPluginService,
    publisher: ResourceSlicePublisher,
}

impl DraPlugin {
    /// `client` and `node_name` are injected rather than built/read
    /// internally (e.g. via `kube::Client::try_default()` and a `NODE_NAME`
    /// env var) so this stays testable against a mocked `kube::Client`,
    /// matching every other DRA component this crate builds
    /// (`ClaimResolver::new`, `ResourceSlicePublisher::new`). Real-world
    /// wiring of both belongs to a future deployable `dra-example` binary
    /// -- explicitly deferred past Phase 1 in `docs/dra-design.md`.
    pub fn new<D: DraDriver + 'static>(
        client: Client,
        driver_name: impl Into<String>,
        node_name: impl Into<String>,
        driver: D,
    ) -> Self {
        let driver_name = driver_name.into();
        let driver = Arc::new(driver);
        let plugin_endpoint = service::plugin_socket_path(&driver_name);
        let registration =
            DraRegistrationServer::new(&driver_name, &plugin_endpoint.to_string_lossy());
        let service =
            DraPluginService::new(ClaimResolver::new(client.clone()), Arc::clone(&driver));
        let publisher = ResourceSlicePublisher::new(client, driver_name.clone(), node_name, driver);
        Self {
            driver_name,
            registration,
            service,
            publisher,
        }
    }

    #[cfg(test)]
    fn for_test<D: DraDriver + 'static>(
        client: Client,
        driver_name: impl Into<String>,
        node_name: impl Into<String>,
        driver: D,
        registration_socket_path: std::path::PathBuf,
    ) -> Self {
        let driver_name = driver_name.into();
        let driver = Arc::new(driver);
        let plugin_endpoint = service::plugin_socket_path(&driver_name);
        let registration = DraRegistrationServer::for_test(
            &driver_name,
            &plugin_endpoint.to_string_lossy(),
            registration_socket_path,
        );
        let service =
            DraPluginService::new(ClaimResolver::new(client.clone()), Arc::clone(&driver));
        let publisher = ResourceSlicePublisher::new(client, driver_name.clone(), node_name, driver);
        Self {
            driver_name,
            registration,
            service,
            publisher,
        }
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
    pub async fn run(self) -> io::Result<()> {
        let plugin_dir = Path::new(KUBELET_PLUGINS_PATH).join(&self.driver_name);
        tokio::fs::create_dir_all(KUBELET_PLUGINS_REGISTRY_PATH).await?;
        tokio::fs::create_dir_all(&plugin_dir).await?;
        self.run_at(&plugin_dir.join("plugin.sock")).await
    }

    /// Core of `run()`, parameterized on the plugin socket path so tests
    /// can point it at a temp directory instead of the real
    /// `/var/lib/kubelet/plugins/<driver_name>/` -- same split
    /// `DraPluginService::spawn`/`spawn_at` already uses.
    async fn run_at(self, plugin_socket_path: &Path) -> io::Result<()> {
        let registration_handle = self.registration.spawn().await?;
        let service_handle = match self.service.spawn_at(plugin_socket_path).await {
            Ok(handle) => handle,
            Err(err) => {
                registration_handle.abort();
                let _ = registration_handle.await;
                return Err(err);
            }
        };
        let publisher_handle = self.publisher.spawn();

        wait_for_component_exit(registration_handle, service_handle, publisher_handle).await
    }
}

#[cfg(test)]
mod tests;
