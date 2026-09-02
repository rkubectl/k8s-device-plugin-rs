use std::fmt;
use std::io;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use tokio::net::UnixListener;
use tokio::task::JoinHandle;
use tokio_stream::wrappers::ReceiverStream;
use tokio_stream::wrappers::UnixListenerStream;
use tonic::transport;
use tonic::transport::Channel;

use k8s_device_plugin_proto as proto;
pub use proto::v1beta1;

pub use k8s_device_plugin_core::AllocationError;
pub use k8s_device_plugin_core::ContainerAllocation;
pub use k8s_device_plugin_core::Device;
pub use k8s_device_plugin_core::DeviceAllocator;
pub use k8s_device_plugin_core::DeviceDiscovery;
pub use k8s_device_plugin_core::DevicePath;
pub use k8s_device_plugin_core::DevicePermissions;
pub use k8s_device_plugin_core::Health;
pub use k8s_device_plugin_core::HostMount;
pub use k8s_device_plugin_core::K8sDevicePlugin;
pub use k8s_device_plugin_core::ValidationError;
pub use registration::RegistrationClient;
pub use static_plugin::StaticDevicePlugin;

mod registration;
mod static_plugin;

fn device_to_proto(device: &Device) -> v1beta1::Device {
    v1beta1::Device {
        id: device.id.clone(),
        health: device.health.to_string(),
        topology: None,
    }
}

/// Validates a single discovered device's id and paths.
fn validate_device(device: &Device) -> Result<(), ValidationError> {
    k8s_device_plugin_core::validate_device_id(&device.id)?;
    for path in &device.paths {
        k8s_device_plugin_core::validate_absolute_path(&path.host_path)?;
        k8s_device_plugin_core::validate_absolute_path(&path.container_path)?;
    }
    Ok(())
}

/// Drops (and logs) any device that fails [`validate_device`] instead of
/// forwarding it to kubelet -- a single misbehaving device shouldn't take
/// down health reporting for every other healthy device. Filtering happens
/// before the `ListAndWatch` change-detection comparison, so a device that
/// flaps in and out of validity doesn't cause spurious pushes of an
/// otherwise-unchanged filtered list.
fn valid_devices(devices: &[Device]) -> Vec<Device> {
    devices
        .iter()
        .filter(|device| match validate_device(device) {
            Ok(()) => true,
            Err(err) => {
                tracing::warn!(device_id = %device.id, %err, "dropping invalid device from ListAndWatch");
                false
            }
        })
        .cloned()
        .collect()
}

/// Validates every path a [`ContainerAllocation`] would have kubelet bind-mount
/// into a container.
fn validate_allocation(allocation: &ContainerAllocation) -> Result<(), ValidationError> {
    for path in &allocation.device_paths {
        k8s_device_plugin_core::validate_absolute_path(&path.host_path)?;
        k8s_device_plugin_core::validate_absolute_path(&path.container_path)?;
    }
    for mount in &allocation.mounts {
        k8s_device_plugin_core::validate_absolute_path(&mount.host_path)?;
        k8s_device_plugin_core::validate_absolute_path(&mount.container_path)?;
    }
    Ok(())
}

/// Compares two device snapshots by content, ignoring order, so a backend whose
/// `discover()` doesn't return devices in a stable order (e.g. backed by a
/// `HashMap`) doesn't trigger spurious `ListAndWatch` updates every poll.
fn devices_equal_ignoring_order(a: &[Device], b: &[Device]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut a_sorted = a.iter().collect::<Vec<_>>();
    let mut b_sorted = b.iter().collect::<Vec<_>>();
    a_sorted.sort_by(|x, y| x.id.cmp(&y.id));
    b_sorted.sort_by(|x, y| x.id.cmp(&y.id));
    a_sorted == b_sorted
}

fn device_path_to_spec(path: &DevicePath) -> v1beta1::DeviceSpec {
    v1beta1::DeviceSpec {
        host_path: path.host_path.to_string_lossy().into_owned(),
        container_path: path.container_path.to_string_lossy().into_owned(),
        permissions: path.permissions.to_string(),
    }
}

fn host_mount_to_proto(mount: &HostMount) -> v1beta1::Mount {
    v1beta1::Mount {
        container_path: mount.container_path.to_string_lossy().into_owned(),
        host_path: mount.host_path.to_string_lossy().into_owned(),
        read_only: mount.read_only,
    }
}

/// Maps an [`AllocationError`] to the `tonic::Status` code that best matches
/// its semantics, consistently across every RPC handler that can return one.
fn allocation_error_to_status(err: AllocationError) -> tonic::Status {
    let message = err.to_string();
    match err {
        AllocationError::DeviceNotFound(_) => tonic::Status::not_found(message),
        AllocationError::PreferredAllocationUnavailable => tonic::Status::unimplemented(message),
        AllocationError::HookFailed(_) => tonic::Status::failed_precondition(message),
        AllocationError::DeviceUnavailable(_) => tonic::Status::unavailable(message),
    }
}

fn container_allocation_to_response(
    allocation: ContainerAllocation,
) -> v1beta1::ContainerAllocateResponse {
    v1beta1::ContainerAllocateResponse {
        devices: allocation
            .device_paths
            .iter()
            .map(device_path_to_spec)
            .collect(),
        mounts: allocation.mounts.iter().map(host_mount_to_proto).collect(),
        envs: allocation.envs.into_iter().collect(),
        annotations: allocation.annotations.into_iter().collect(),
        cdi_devices: allocation
            .cdi_devices
            .into_iter()
            .map(|name| v1beta1::CdiDevice { name })
            .collect(),
    }
}

#[derive(Clone, Debug)]
pub struct DevicePlugin {
    endpoint: String,
    resource_name: String,
    service: Arc<DevicePluginService>,
    kubelet_socket: String,
}

impl DevicePlugin {
    pub fn new(resource_name: &str, service: DevicePluginService) -> Result<Self, ValidationError> {
        k8s_device_plugin_core::validate_resource_name(resource_name)?;
        let socket_name = sanitize_socket_name(resource_name);
        let resource_name = resource_name.to_string();
        let endpoint = String::from(v1beta1::DEVICE_PLUGIN_PATH) + &socket_name;
        let service = Arc::new(service);
        let kubelet_socket = Self::kubelet_socket_path();
        Ok(Self {
            endpoint,
            resource_name,
            service,
            kubelet_socket,
        })
    }

    #[cfg(test)]
    fn for_test(
        resource_name: &str,
        service: DevicePluginService,
        endpoint: String,
        kubelet_socket: String,
    ) -> Self {
        Self {
            endpoint,
            resource_name: resource_name.to_string(),
            service: Arc::new(service),
            kubelet_socket,
        }
    }

    #[tracing::instrument(skip(self), fields(resource_name = %self.resource_name))]
    pub async fn run(&self) -> io::Result<()> {
        let mut server_handle = self.spawn_server()?;
        loop {
            // Subscribe before registering so a fast kubelet disconnect is never missed.
            let kubelet_gone = self.service.kubelet_gone.notified();
            if let Err(err) = self.register_with_retry().await {
                tracing::error!(%err, "registration permanently failed; shutting down");
                // Registration is permanently exhausted: abort the spawned server task
                // so it doesn't keep serving RPCs against a listener nobody can reach.
                server_handle.abort();
                return Err(err);
            }
            tracing::info!("registered with kubelet");
            tokio::select! {
                result = &mut server_handle => {
                    return result
                        .map_err(io::Error::other)?
                        .map_err(io::Error::other);
                }
                _ = kubelet_gone => {
                    tracing::warn!("kubelet disconnected; re-registering");
                }
            }
        }
    }

    async fn register_with_retry(&self) -> io::Result<()> {
        self.try_register(self.kubelet_socket.clone(), 10, Duration::from_secs(1))
            .await
    }

    #[tracing::instrument(skip(self, kubelet_socket), fields(resource_name = %self.resource_name))]
    async fn try_register(
        &self,
        kubelet_socket: String,
        max_attempts: u32,
        initial_delay: Duration,
    ) -> io::Result<()> {
        if max_attempts == 0 {
            return Err(io::Error::other("max_attempts must be at least 1"));
        }
        let mut delay = initial_delay;
        for attempt in 1..max_attempts {
            match self.register_at(kubelet_socket.clone()).await {
                Ok(()) => return Ok(()),
                Err(err) => {
                    tracing::warn!(attempt, max_attempts, %err, "registration attempt failed; retrying");
                    tokio::time::sleep(delay).await;
                    delay = (delay * 2).min(Duration::from_secs(30));
                }
            }
        }
        self.register_at(kubelet_socket)
            .await
            .map_err(io::Error::other)
    }

    fn spawn_server(&self) -> io::Result<JoinHandle<Result<(), transport::Error>>> {
        let incoming: UnixListenerStream = self.setup_listener()?;
        let svc = self.service();
        let router = transport::Server::builder().add_service(svc);
        let handle = tokio::spawn(router.serve_with_incoming(incoming));
        Ok(handle)
    }

    pub async fn register(&self) -> tonic::Result<()> {
        self.register_at(self.kubelet_socket.clone()).await
    }

    async fn register_at(&self, kubelet_socket: String) -> tonic::Result<()> {
        RegistrationClient::new(kubelet_socket)
            .await?
            .register(self.registration_endpoint(), &self.resource_name)
            .await
    }

    fn registration_endpoint(&self) -> &str {
        Path::new(&self.endpoint)
            .file_name()
            .and_then(|file_name| file_name.to_str())
            .unwrap_or(&self.endpoint)
    }

    fn kubelet_socket_path() -> String {
        String::from(v1beta1::DEVICE_PLUGIN_PATH) + v1beta1::KUBELET_SOCKET
    }

    fn setup_listener(&self) -> io::Result<UnixListenerStream> {
        let listener = k8s_device_plugin_core::bind_unix_listener(Path::new(&self.endpoint))?;
        UnixListener::from_std(listener).map(UnixListenerStream::new)
    }

    fn service(&self) -> v1beta1::DevicePluginServer<DevicePluginService> {
        let inner = Arc::clone(&self.service);
        v1beta1::DevicePluginServer::from_arc(inner)
    }
}

/// Default interval at which [`DevicePluginService`] re-polls [`DeviceDiscovery::discover`]
/// to detect health/state changes while a `ListAndWatch` stream is open.
const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(5);

pub struct DevicePluginService {
    plugin: Arc<dyn K8sDevicePlugin>,
    kubelet_gone: Arc<tokio::sync::Notify>,
    poll_interval: Duration,
}

impl DevicePluginService {
    pub fn new<P: K8sDevicePlugin + 'static>(plugin: P) -> Self {
        Self {
            plugin: Arc::new(plugin),
            kubelet_gone: Arc::new(tokio::sync::Notify::new()),
            poll_interval: DEFAULT_POLL_INTERVAL,
        }
    }

    /// Overrides the default interval at which device state is re-polled for
    /// `ListAndWatch` updates.
    #[must_use]
    pub fn with_poll_interval(mut self, poll_interval: Duration) -> Self {
        self.poll_interval = poll_interval;
        self
    }
}

impl fmt::Debug for DevicePluginService {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DevicePluginService")
            .finish_non_exhaustive()
    }
}

#[tonic::async_trait]
impl v1beta1::DevicePlugin for DevicePluginService {
    type ListAndWatchStream = ReceiverStream<tonic::Result<v1beta1::ListAndWatchResponse>>;

    async fn get_device_plugin_options(
        &self,
        _request: tonic::Request<v1beta1::Empty>,
    ) -> tonic::Result<tonic::Response<v1beta1::DevicePluginOptions>> {
        Ok(tonic::Response::new(v1beta1::DevicePluginOptions {
            pre_start_required: self.plugin.pre_start_required(),
            get_preferred_allocation_available: self.plugin.preferred_allocation_available(),
        }))
    }

    #[tracing::instrument(skip(self, _request))]
    async fn list_and_watch(
        &self,
        _request: tonic::Request<v1beta1::Empty>,
    ) -> tonic::Result<tonic::Response<Self::ListAndWatchStream>> {
        let (tx, rx) = tokio::sync::mpsc::channel(1);

        let mut last_devices = valid_devices(&self.plugin.discover().await);
        let response = v1beta1::ListAndWatchResponse {
            devices: last_devices.iter().map(device_to_proto).collect(),
        };
        let _ = tx.send(Ok(response)).await;

        let plugin = Arc::clone(&self.plugin);
        let kubelet_gone = Arc::clone(&self.kubelet_gone);
        let poll_interval = self.poll_interval;
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    () = tokio::time::sleep(poll_interval) => {
                        let devices = valid_devices(&plugin.discover().await);
                        if !devices_equal_ignoring_order(&devices, &last_devices) {
                            tracing::debug!(device_count = devices.len(), "device state changed; pushing update");
                            let response = v1beta1::ListAndWatchResponse {
                                devices: devices.iter().map(device_to_proto).collect(),
                            };
                            last_devices = devices;
                            if tx.send(Ok(response)).await.is_err() {
                                break;
                            }
                        }
                    }
                    () = tx.closed() => break,
                }
            }
            kubelet_gone.notify_one();
        });

        Ok(tonic::Response::new(Self::ListAndWatchStream::new(rx)))
    }

    #[tracing::instrument(skip(self, request))]
    async fn get_preferred_allocation(
        &self,
        request: tonic::Request<v1beta1::PreferredAllocationRequest>,
    ) -> tonic::Result<tonic::Response<v1beta1::PreferredAllocationResponse>> {
        let mut container_responses = Vec::new();
        for container_request in request.into_inner().container_requests {
            let size = usize::try_from(container_request.allocation_size).map_err(|_| {
                tonic::Status::invalid_argument("allocation_size must be non-negative")
            })?;
            let device_ids = self
                .plugin
                .preferred_allocation(
                    &container_request.available_device_i_ds,
                    &container_request.must_include_device_i_ds,
                    size,
                )
                .await
                .inspect_err(|err| tracing::warn!(%err, "preferred_allocation hook failed"))
                .map_err(allocation_error_to_status)?;
            container_responses.push(v1beta1::ContainerPreferredAllocationResponse {
                device_i_ds: device_ids,
            });
        }

        Ok(tonic::Response::new(v1beta1::PreferredAllocationResponse {
            container_responses,
        }))
    }

    #[tracing::instrument(skip(self, request))]
    async fn allocate(
        &self,
        request: tonic::Request<v1beta1::AllocateRequest>,
    ) -> tonic::Result<tonic::Response<v1beta1::AllocateResponse>> {
        // Each container's allocation is independent, so run them concurrently
        // instead of one-at-a-time -- spawn all tasks up front, then await in
        // order so container_responses lines up with the request. If any task
        // fails, abort the rest instead of letting them keep running after
        // we've already reported failure to kubelet.
        let tasks = request
            .into_inner()
            .container_requests
            .into_iter()
            .map(|container_request| {
                let plugin = Arc::clone(&self.plugin);
                tokio::spawn(async move { plugin.allocate(&container_request.devices_ids).await })
            })
            .collect::<Vec<_>>();

        let mut container_responses = Vec::with_capacity(tasks.len());
        let mut tasks = tasks.into_iter();
        while let Some(task) = tasks.next() {
            let allocation = match task.await {
                Ok(Ok(allocation)) => match validate_allocation(&allocation) {
                    Ok(()) => allocation,
                    Err(err) => {
                        tracing::warn!(%err, "backend returned invalid allocation");
                        tasks.for_each(|task| task.abort());
                        return Err(tonic::Status::internal(err.to_string()));
                    }
                },
                Ok(Err(err)) => {
                    tracing::warn!(%err, "allocate failed");
                    tasks.for_each(|task| task.abort());
                    return Err(allocation_error_to_status(err));
                }
                Err(join_err) => {
                    tasks.for_each(|task| task.abort());
                    return Err(tonic::Status::internal(format!(
                        "allocate task panicked: {join_err}"
                    )));
                }
            };
            container_responses.push(container_allocation_to_response(allocation));
        }

        Ok(tonic::Response::new(v1beta1::AllocateResponse {
            container_responses,
        }))
    }

    #[tracing::instrument(skip(self, request))]
    async fn pre_start_container(
        &self,
        request: tonic::Request<v1beta1::PreStartContainerRequest>,
    ) -> tonic::Result<tonic::Response<v1beta1::PreStartContainerResponse>> {
        let device_ids = request.into_inner().devices_ids;
        self.plugin
            .pre_start_container(&device_ids)
            .await
            .inspect_err(|err| tracing::warn!(%err, "pre_start_container hook failed"))
            .map_err(allocation_error_to_status)?;
        Ok(tonic::Response::new(v1beta1::PreStartContainerResponse {}))
    }
}

/// Derives a filesystem-safe, collision-resistant socket name from a resource
/// name, sized to fit alongside [`v1beta1::DEVICE_PLUGIN_PATH`] within the
/// platform's Unix socket path limit. See
/// [`k8s_device_plugin_core::sanitize_socket_name`] for how collisions and
/// truncation are handled.
fn sanitize_socket_name(name: &str) -> String {
    let budget = k8s_device_plugin_core::MAX_SOCKET_PATH_LEN
        .saturating_sub(v1beta1::DEVICE_PLUGIN_PATH.len());
    k8s_device_plugin_core::sanitize_socket_name(name, budget)
}

#[cfg(test)]
mod tests;
