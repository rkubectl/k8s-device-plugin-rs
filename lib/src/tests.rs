use std::collections::BTreeMap;
use std::path::PathBuf;

use tokio_stream::StreamExt;

use super::*;

#[test]
fn kubelet_socket_path() {
    let endpoint = DevicePlugin::kubelet_socket_path();
    assert_eq!(endpoint, "/var/lib/kubelet/device-plugins/kubelet.sock");
}

#[test]
fn register_uses_socket_filename_as_endpoint() {
    let plugin =
        DevicePlugin::new("example.com/device", make_service()).expect("valid resource name");

    let expected_prefix = "/var/lib/kubelet/device-plugins/example_com_device-";
    assert!(
        plugin.endpoint.starts_with(expected_prefix),
        "endpoint {} should start with {expected_prefix}",
        plugin.endpoint
    );
    assert!(
        plugin
            .registration_endpoint()
            .starts_with("example_com_device-")
    );
}

#[test]
fn new_rejects_malformed_resource_name() {
    let err = DevicePlugin::new("widget", make_service()).expect_err("no domain in name");
    assert_eq!(
        err,
        ValidationError::MalformedResourceName("widget".to_string())
    );
}

#[test]
fn sanitize_socket_name_is_deterministic() {
    assert_eq!(
        sanitize_socket_name("example.com/device"),
        sanitize_socket_name("example.com/device")
    );
}

#[test]
fn sanitize_socket_name_does_not_collide_across_distinct_names() {
    // These two names sanitize to the same "acme_com_gpu" prefix but must not
    // collide once the disambiguating hash suffix is applied.
    assert_ne!(
        sanitize_socket_name("acme.com/gpu"),
        sanitize_socket_name("acme_com/gpu")
    );
}

#[test]
fn sanitize_socket_name_keeps_full_endpoint_within_socket_path_limit() {
    let long_name = "example.com/a-very-long-custom-accelerator-resource-name-that-keeps-going";
    let socket_name = sanitize_socket_name(long_name);
    let endpoint_len = v1beta1::DEVICE_PLUGIN_PATH.len() + socket_name.len();

    assert!(
        endpoint_len <= k8s_device_plugin_core::MAX_SOCKET_PATH_LEN,
        "endpoint length {endpoint_len} exceeds the socket path limit of {}",
        k8s_device_plugin_core::MAX_SOCKET_PATH_LEN
    );
    // The disambiguating hash suffix (`-` + 16 hex digits) must survive
    // truncation intact.
    let suffix = &socket_name[socket_name.len() - 17..];
    assert!(suffix.starts_with('-'));
    assert!(suffix[1..].chars().all(|c| c.is_ascii_hexdigit()));
}

#[test]
fn devices_equal_ignoring_order_treats_reordered_devices_as_equal() {
    let a = vec![
        make_device("dev-0", Health::Healthy),
        make_device("dev-1", Health::Healthy),
    ];
    let b = vec![
        make_device("dev-1", Health::Healthy),
        make_device("dev-0", Health::Healthy),
    ];

    assert!(devices_equal_ignoring_order(&a, &b));
}

#[test]
fn devices_equal_ignoring_order_detects_real_changes() {
    let a = vec![make_device("dev-0", Health::Healthy)];
    let b = vec![make_device("dev-0", Health::Unhealthy)];

    assert!(!devices_equal_ignoring_order(&a, &b));
}

#[tokio::test]
async fn list_and_watch_does_not_repeat_reordered_but_unchanged_devices() {
    use v1beta1::DevicePlugin as _;

    let devices = Arc::new(std::sync::Mutex::new(vec![
        make_device("dev-0", Health::Healthy),
        make_device("dev-1", Health::Healthy),
    ]));
    let service = DevicePluginService::new(DynamicDevicePlugin(Arc::clone(&devices)))
        .with_poll_interval(Duration::from_millis(5));

    let mut stream = service
        .list_and_watch(tonic::Request::new(v1beta1::Empty {}))
        .await
        .unwrap()
        .into_inner();

    stream.next().await.unwrap().unwrap();

    // Same devices, different order: must not be treated as a change.
    *devices.lock().unwrap() = vec![
        make_device("dev-1", Health::Healthy),
        make_device("dev-0", Health::Healthy),
    ];

    let second = tokio::time::timeout(Duration::from_millis(50), stream.next()).await;
    assert!(
        second.is_err(),
        "no update should be pushed when devices are merely reordered"
    );
}

#[test]
fn converts_device_to_proto() {
    let device = Device::new("dev-0");
    let proto = device_to_proto(&device);
    assert_eq!(proto.id, "dev-0");
    assert_eq!(proto.health, v1beta1::HEALTHY);
}

#[test]
fn converts_device_path_to_spec() {
    let path = DevicePath::rdwr("/dev/mydev0");
    let spec = device_path_to_spec(&path);
    assert_eq!(spec.host_path, "/dev/mydev0");
    assert_eq!(spec.container_path, "/dev/mydev0");
    assert_eq!(spec.permissions, "rw");
}

fn make_service() -> DevicePluginService {
    let device = Device::rdwr("dev-0", "/dev/null");
    DevicePluginService::new(StaticDevicePlugin::new(vec![device]))
}

#[tokio::test]
async fn list_and_watch_sends_initial_device_list() {
    use v1beta1::DevicePlugin as _;

    let service = make_service();
    let mut stream = service
        .list_and_watch(tonic::Request::new(v1beta1::Empty {}))
        .await
        .unwrap()
        .into_inner();

    let response = stream.next().await.unwrap().unwrap();
    assert_eq!(response.devices.len(), 1);
    assert_eq!(response.devices[0].id, "dev-0");
    assert_eq!(response.devices[0].health, v1beta1::HEALTHY);
}

struct DynamicDevicePlugin(Arc<std::sync::Mutex<Vec<Device>>>);

impl K8sDevicePlugin for DynamicDevicePlugin {}

#[tonic::async_trait]
impl DeviceDiscovery for DynamicDevicePlugin {
    async fn discover(&self) -> Vec<Device> {
        self.0.lock().unwrap().clone()
    }
}

#[tonic::async_trait]
impl DeviceAllocator for DynamicDevicePlugin {
    async fn allocate(
        &self,
        _device_ids: &[String],
    ) -> Result<ContainerAllocation, AllocationError> {
        Ok(ContainerAllocation::default())
    }
}

fn make_device(id: &str, health: Health) -> Device {
    Device::new(id).health(health)
}

#[tokio::test]
async fn list_and_watch_pushes_update_when_devices_change() {
    use v1beta1::DevicePlugin as _;

    let devices = Arc::new(std::sync::Mutex::new(vec![make_device(
        "dev-0",
        Health::Healthy,
    )]));
    let service = DevicePluginService::new(DynamicDevicePlugin(Arc::clone(&devices)))
        .with_poll_interval(Duration::from_millis(5));

    let mut stream = service
        .list_and_watch(tonic::Request::new(v1beta1::Empty {}))
        .await
        .unwrap()
        .into_inner();

    let first = stream.next().await.unwrap().unwrap();
    assert_eq!(first.devices[0].health, v1beta1::HEALTHY);

    *devices.lock().unwrap() = vec![make_device("dev-0", Health::Unhealthy)];

    let second = stream.next().await.unwrap().unwrap();
    assert_eq!(second.devices[0].health, v1beta1::UNHEALTHY);
}

#[tokio::test]
async fn list_and_watch_does_not_repeat_unchanged_devices() {
    use v1beta1::DevicePlugin as _;

    let devices = Arc::new(std::sync::Mutex::new(vec![make_device(
        "dev-0",
        Health::Healthy,
    )]));
    let service = DevicePluginService::new(DynamicDevicePlugin(Arc::clone(&devices)))
        .with_poll_interval(Duration::from_millis(5));

    let mut stream = service
        .list_and_watch(tonic::Request::new(v1beta1::Empty {}))
        .await
        .unwrap()
        .into_inner();

    stream.next().await.unwrap().unwrap();

    let second = tokio::time::timeout(Duration::from_millis(50), stream.next()).await;
    assert!(
        second.is_err(),
        "no update should be pushed while devices are unchanged"
    );
}

#[tokio::test]
async fn list_and_watch_drops_invalid_device_but_keeps_valid_sibling() {
    use v1beta1::DevicePlugin as _;

    let devices = Arc::new(std::sync::Mutex::new(vec![
        make_device("dev-0", Health::Healthy),
        make_device("", Health::Healthy),
    ]));
    let service = DevicePluginService::new(DynamicDevicePlugin(Arc::clone(&devices)));

    let mut stream = service
        .list_and_watch(tonic::Request::new(v1beta1::Empty {}))
        .await
        .unwrap()
        .into_inner();

    let response = stream.next().await.unwrap().unwrap();
    assert_eq!(response.devices.len(), 1);
    assert_eq!(response.devices[0].id, "dev-0");
}

#[tokio::test]
async fn allocate_known_device() {
    use v1beta1::DevicePlugin as _;

    let service = make_service();
    let request = tonic::Request::new(v1beta1::AllocateRequest {
        container_requests: vec![v1beta1::ContainerAllocateRequest {
            devices_ids: vec!["dev-0".to_string()],
        }],
    });

    let response = service.allocate(request).await.unwrap().into_inner();
    assert_eq!(response.container_responses.len(), 1);
    assert_eq!(response.container_responses[0].devices.len(), 1);
    assert_eq!(
        response.container_responses[0].devices[0].host_path,
        "/dev/null"
    );
}

#[tokio::test]
async fn allocate_maps_mounts_envs_annotations_and_cdi_devices() {
    use v1beta1::DevicePlugin as _;

    let service = DevicePluginService::new(FullFeaturedPlugin);
    let request = tonic::Request::new(v1beta1::AllocateRequest {
        container_requests: vec![v1beta1::ContainerAllocateRequest {
            devices_ids: vec!["widget-0".to_string()],
        }],
    });

    let response = service.allocate(request).await.unwrap().into_inner();
    let container_response = &response.container_responses[0];

    assert_eq!(container_response.mounts.len(), 1);
    assert_eq!(container_response.mounts[0].host_path, "/opt/widget/lib");
    assert!(container_response.mounts[0].read_only);

    assert_eq!(
        container_response.envs.get("WIDGET_VISIBLE_DEVICES"),
        Some(&"0".to_string())
    );
    assert_eq!(
        container_response
            .annotations
            .get("widget.example.com/pool"),
        Some(&"a".to_string())
    );
    assert_eq!(
        container_response.cdi_devices[0].name,
        "example.com/widget=widget-0"
    );
}

struct RelativePathPlugin;

#[tonic::async_trait]
impl DeviceDiscovery for RelativePathPlugin {
    async fn discover(&self) -> Vec<Device> {
        vec![]
    }
}

#[tonic::async_trait]
impl DeviceAllocator for RelativePathPlugin {
    async fn allocate(
        &self,
        _device_ids: &[String],
    ) -> Result<ContainerAllocation, AllocationError> {
        Ok(ContainerAllocation {
            device_paths: vec![DevicePath::rdwr("relative/widget0")],
            ..Default::default()
        })
    }
}

impl K8sDevicePlugin for RelativePathPlugin {}

#[tokio::test]
async fn allocate_rejects_relative_device_path() {
    use v1beta1::DevicePlugin as _;

    let service = DevicePluginService::new(RelativePathPlugin);
    let request = tonic::Request::new(v1beta1::AllocateRequest {
        container_requests: vec![v1beta1::ContainerAllocateRequest {
            devices_ids: vec!["widget-0".to_string()],
        }],
    });

    let status = service.allocate(request).await.unwrap_err();
    assert_eq!(status.code(), tonic::Code::Internal);
}

struct SlowPlugin;

#[tonic::async_trait]
impl DeviceDiscovery for SlowPlugin {
    async fn discover(&self) -> Vec<Device> {
        vec![]
    }
}

#[tonic::async_trait]
impl DeviceAllocator for SlowPlugin {
    async fn allocate(
        &self,
        device_ids: &[String],
    ) -> Result<ContainerAllocation, AllocationError> {
        tokio::time::sleep(Duration::from_millis(20)).await;
        Ok(ContainerAllocation {
            envs: BTreeMap::from([("DEVICE_IDS".to_string(), device_ids.join(","))]),
            ..Default::default()
        })
    }
}

impl K8sDevicePlugin for SlowPlugin {}

#[tokio::test]
async fn allocate_runs_containers_concurrently_and_preserves_order() {
    use v1beta1::DevicePlugin as _;

    let service = DevicePluginService::new(SlowPlugin);
    let container_requests = (0..5)
        .map(|i| v1beta1::ContainerAllocateRequest {
            devices_ids: vec![format!("dev-{i}")],
        })
        .collect::<Vec<_>>();
    let request = tonic::Request::new(v1beta1::AllocateRequest { container_requests });

    let start = std::time::Instant::now();
    let response = service.allocate(request).await.unwrap().into_inner();
    let elapsed = start.elapsed();

    // 5 containers x 20ms sleep each; concurrent execution should take
    // much less than the 100ms a sequential loop would need.
    assert!(
        elapsed < Duration::from_millis(80),
        "allocate took {elapsed:?}, looks sequential"
    );

    for (i, container_response) in response.container_responses.iter().enumerate() {
        assert_eq!(
            container_response.envs.get("DEVICE_IDS"),
            Some(&format!("dev-{i}"))
        );
    }
}

struct AbortAwarePlugin {
    fail_on: String,
    completed: Arc<std::sync::atomic::AtomicUsize>,
}

#[tonic::async_trait]
impl DeviceDiscovery for AbortAwarePlugin {
    async fn discover(&self) -> Vec<Device> {
        vec![]
    }
}

#[tonic::async_trait]
impl DeviceAllocator for AbortAwarePlugin {
    async fn allocate(
        &self,
        device_ids: &[String],
    ) -> Result<ContainerAllocation, AllocationError> {
        if device_ids.first().map(String::as_str) == Some(self.fail_on.as_str()) {
            return Err(AllocationError::DeviceNotFound(self.fail_on.clone()));
        }
        // Simulate slow work; if the task is aborted (as it should be once
        // a sibling container fails), this sleep is cut short and
        // `completed` is never incremented.
        tokio::time::sleep(Duration::from_millis(200)).await;
        self.completed
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(ContainerAllocation::default())
    }
}

impl K8sDevicePlugin for AbortAwarePlugin {}

#[tokio::test]
async fn allocate_aborts_in_flight_tasks_when_one_container_fails() {
    use v1beta1::DevicePlugin as _;

    let completed = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let service = DevicePluginService::new(AbortAwarePlugin {
        fail_on: "bad".to_string(),
        completed: Arc::clone(&completed),
    });

    let request = tonic::Request::new(v1beta1::AllocateRequest {
        container_requests: vec![
            v1beta1::ContainerAllocateRequest {
                devices_ids: vec!["bad".to_string()],
            },
            v1beta1::ContainerAllocateRequest {
                devices_ids: vec!["good".to_string()],
            },
        ],
    });

    let status = service.allocate(request).await.unwrap_err();
    assert_eq!(status.code(), tonic::Code::NotFound);

    // Give the sibling task time to have finished if it weren't aborted.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        completed.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "the slow sibling task should have been aborted, not left running"
    );
}

#[tokio::test]
async fn allocate_unknown_device_returns_not_found() {
    use v1beta1::DevicePlugin as _;

    let service = make_service();
    let request = tonic::Request::new(v1beta1::AllocateRequest {
        container_requests: vec![v1beta1::ContainerAllocateRequest {
            devices_ids: vec!["does-not-exist".to_string()],
        }],
    });

    let status = service.allocate(request).await.unwrap_err();
    assert_eq!(status.code(), tonic::Code::NotFound);
}

struct FullFeaturedPlugin;

#[tonic::async_trait]
impl DeviceDiscovery for FullFeaturedPlugin {
    async fn discover(&self) -> Vec<Device> {
        vec![]
    }
}

#[tonic::async_trait]
impl DeviceAllocator for FullFeaturedPlugin {
    async fn allocate(
        &self,
        _device_ids: &[String],
    ) -> Result<ContainerAllocation, AllocationError> {
        Ok(ContainerAllocation {
            mounts: vec![HostMount {
                host_path: PathBuf::from("/opt/widget/lib"),
                container_path: PathBuf::from("/opt/widget/lib"),
                read_only: true,
            }],
            envs: BTreeMap::from([("WIDGET_VISIBLE_DEVICES".to_string(), "0".to_string())]),
            annotations: BTreeMap::from([("widget.example.com/pool".to_string(), "a".to_string())]),
            cdi_devices: vec!["example.com/widget=widget-0".to_string()],
            ..Default::default()
        })
    }
}

#[tonic::async_trait]
impl K8sDevicePlugin for FullFeaturedPlugin {
    fn pre_start_required(&self) -> bool {
        true
    }

    async fn pre_start_container(&self, device_ids: &[String]) -> Result<(), AllocationError> {
        if device_ids.iter().any(|id| id == "broken") {
            return Err(AllocationError::HookFailed("device reset failed".into()));
        }
        Ok(())
    }

    fn preferred_allocation_available(&self) -> bool {
        true
    }

    async fn preferred_allocation(
        &self,
        available_device_ids: &[String],
        _must_include_device_ids: &[String],
        size: usize,
    ) -> Result<Vec<String>, AllocationError> {
        Ok(available_device_ids.iter().take(size).cloned().collect())
    }
}

#[tokio::test]
async fn get_device_plugin_options_reports_defaults_when_hooks_unimplemented() {
    use v1beta1::DevicePlugin as _;

    let service = make_service();
    let options = service
        .get_device_plugin_options(tonic::Request::new(v1beta1::Empty {}))
        .await
        .unwrap()
        .into_inner();

    assert!(!options.pre_start_required);
    assert!(!options.get_preferred_allocation_available);
}

#[tokio::test]
async fn get_device_plugin_options_reports_enabled_hooks() {
    use v1beta1::DevicePlugin as _;

    let service = DevicePluginService::new(FullFeaturedPlugin);
    let options = service
        .get_device_plugin_options(tonic::Request::new(v1beta1::Empty {}))
        .await
        .unwrap()
        .into_inner();

    assert!(options.pre_start_required);
    assert!(options.get_preferred_allocation_available);
}

#[tokio::test]
async fn pre_start_container_default_is_a_no_op() {
    use v1beta1::DevicePlugin as _;

    let service = make_service();
    let request = tonic::Request::new(v1beta1::PreStartContainerRequest {
        devices_ids: vec!["dev-0".to_string()],
    });

    service.pre_start_container(request).await.unwrap();
}

#[tokio::test]
async fn pre_start_container_surfaces_hook_failure() {
    use v1beta1::DevicePlugin as _;

    let service = DevicePluginService::new(FullFeaturedPlugin);
    let request = tonic::Request::new(v1beta1::PreStartContainerRequest {
        devices_ids: vec!["broken".to_string()],
    });

    let status = service.pre_start_container(request).await.unwrap_err();
    assert_eq!(status.code(), tonic::Code::FailedPrecondition);
}

#[tokio::test]
async fn get_preferred_allocation_default_is_unavailable() {
    use v1beta1::DevicePlugin as _;

    let service = make_service();
    let request = tonic::Request::new(v1beta1::PreferredAllocationRequest {
        container_requests: vec![v1beta1::ContainerPreferredAllocationRequest {
            available_device_i_ds: vec!["dev-0".to_string()],
            must_include_device_i_ds: vec![],
            allocation_size: 1,
        }],
    });

    let status = service.get_preferred_allocation(request).await.unwrap_err();
    assert_eq!(status.code(), tonic::Code::Unimplemented);
}

#[tokio::test]
async fn get_preferred_allocation_returns_chosen_devices() {
    use v1beta1::DevicePlugin as _;

    let service = DevicePluginService::new(FullFeaturedPlugin);
    let request = tonic::Request::new(v1beta1::PreferredAllocationRequest {
        container_requests: vec![v1beta1::ContainerPreferredAllocationRequest {
            available_device_i_ds: vec!["dev-0".to_string(), "dev-1".to_string()],
            must_include_device_i_ds: vec![],
            allocation_size: 1,
        }],
    });

    let response = service
        .get_preferred_allocation(request)
        .await
        .unwrap()
        .into_inner();

    assert_eq!(response.container_responses.len(), 1);
    assert_eq!(response.container_responses[0].device_i_ds, vec!["dev-0"]);
}

#[tokio::test]
async fn try_register_succeeds_on_first_attempt() {
    use k8s_device_plugin_test::registration::start_mock_registration_server;

    let server = start_mock_registration_server(None);
    let plugin =
        DevicePlugin::new("example.com/device", make_service()).expect("valid resource name");

    plugin
        .try_register(server.socket_path(), 3, Duration::from_millis(1))
        .await
        .expect("registration should succeed");

    let requests = server.collected_requests().await;
    assert_eq!(requests.len(), 1);
    server.shutdown();
}

#[tokio::test]
async fn try_register_gives_up_after_max_attempts() {
    use k8s_device_plugin_test::registration::start_mock_registration_server;

    let server = start_mock_registration_server(Some((tonic::Code::Unavailable, "kubelet down")));
    let plugin =
        DevicePlugin::new("example.com/device", make_service()).expect("valid resource name");

    let err = plugin
        .try_register(server.socket_path(), 3, Duration::from_millis(1))
        .await
        .expect_err("should give up after max attempts");

    assert!(err.to_string().contains("kubelet down"));
    server.shutdown();
}

#[tokio::test]
async fn try_register_retries_until_success() {
    use k8s_device_plugin_test::registration::start_mock_registration_server_with_failures;

    let server = start_mock_registration_server_with_failures(2);
    let plugin =
        DevicePlugin::new("example.com/device", make_service()).expect("valid resource name");

    plugin
        .try_register(server.socket_path(), 3, Duration::from_millis(1))
        .await
        .expect("should succeed after 2 failures");

    // Only the successful attempt is recorded (failures don't push to requests).
    let requests = server.collected_requests().await;
    assert_eq!(requests.len(), 1);
    server.shutdown();
}

#[tokio::test]
async fn run_reregisters_after_kubelet_disconnects() {
    use k8s_device_plugin_test::device_plugin::MockDevicePluginClient;
    use k8s_device_plugin_test::registration::start_mock_registration_server;
    use tempfile::TempDir;

    let registration_server = start_mock_registration_server(None);
    let plugin_dir = TempDir::new().expect("create temp dir for plugin socket");
    let endpoint = plugin_dir
        .path()
        .join("plugin.sock")
        .to_string_lossy()
        .into_owned();

    let plugin = DevicePlugin::for_test(
        "example.com/device",
        make_service(),
        endpoint.clone(),
        registration_server.socket_path(),
    );

    let run_handle = tokio::spawn(async move { plugin.run().await });

    wait_for_request_count(&registration_server, 1).await;

    // Connect as the kubelet and read the initial device list. The
    // client-side stream is dropped when this call returns, which the
    // plugin detects as the kubelet going away.
    let mut client = MockDevicePluginClient::connect(&endpoint)
        .await
        .expect("connect to plugin socket");
    client
        .list_and_watch_once()
        .await
        .expect("initial device list");

    wait_for_request_count(&registration_server, 2).await;

    let requests = registration_server.collected_requests().await;
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1].resource_name, "example.com/device");
    assert_eq!(requests[1].endpoint, "plugin.sock");

    run_handle.abort();
    registration_server.shutdown();
}

async fn wait_for_request_count(
    server: &k8s_device_plugin_test::registration::MockRegistrationServer,
    count: usize,
) {
    for _ in 0..200 {
        if server.collected_requests().await.len() >= count {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("timed out waiting for {count} registration request(s)");
}
