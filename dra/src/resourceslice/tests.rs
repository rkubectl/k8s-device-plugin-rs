use http_body_util::BodyExt;
use k8s_device_plugin_test::kube_mock::MockKubeHandle;
use k8s_device_plugin_test::kube_mock::device_json;
use k8s_device_plugin_test::kube_mock::mock_kube_client;
use k8s_device_plugin_test::kube_mock::node_json;
use k8s_device_plugin_test::kube_mock::resource_slice_json;
use k8s_device_plugin_test::kube_mock::respond;
use kube::client::Body;

use super::*;

struct StaticPool(BTreeMap<String, Vec<PoolDevice>>);

#[tonic::async_trait]
impl ResourcePool for StaticPool {
    async fn devices(&self) -> BTreeMap<String, Vec<PoolDevice>> {
        self.0.clone()
    }
}

fn one_pool(pool_name: &str, devices: Vec<PoolDevice>) -> StaticPool {
    StaticPool(BTreeMap::from([(pool_name.to_string(), devices)]))
}

fn mock_publisher(resource_pool: StaticPool) -> (ResourceSlicePublisher, MockKubeHandle) {
    let (client, handle) = mock_kube_client();
    let publisher = ResourceSlicePublisher::new(client, "example.com", "node-0", resource_pool);
    (publisher, handle)
}

fn resource_slice_list(items: Vec<serde_json::Value>) -> serde_json::Value {
    json::json!({
        "apiVersion": "resource.k8s.io/v1",
        "kind": "ResourceSliceList",
        "items": items,
    })
}

fn owned_slice(
    name: &str,
    generation: i64,
    pool_name: &str,
    slice_count: i64,
    devices: Vec<json::Value>,
) -> serde_json::Value {
    let mut slice = resource_slice_json(
        name,
        "example.com",
        "node-0",
        generation,
        pool_name,
        devices,
    );
    slice["spec"]["pool"]["resourceSliceCount"] = json::json!(slice_count);
    slice["metadata"]["ownerReferences"] = json::json!([{
        "apiVersion": "v1",
        "kind": "Node",
        "name": "node-0",
        "uid": "node-uid-0",
        "controller": true,
        "blockOwnerDeletion": false,
    }]);
    slice
}

fn widgets(count: usize) -> Vec<PoolDevice> {
    (0..count)
        .map(|index| PoolDevice::new(format!("widget-{index:03}")))
        .collect()
}

async fn request_body_json(request: http::Request<Body>) -> serde_json::Value {
    let bytes = request
        .into_body()
        .collect()
        .await
        .expect("collect request body")
        .to_bytes();
    serde_json::from_slice(&bytes).expect("request body is valid JSON")
}

#[tokio::test]
async fn publish_once_creates_slice_when_absent() {
    let (publisher, mut handle) =
        mock_publisher(one_pool("pool-0", vec![PoolDevice::new("widget-0")]));

    let script = async {
        respond(&mut handle, 200, &node_json("node-0", "node-uid-0")).await;
        let list = respond(&mut handle, 200, &resource_slice_list(vec![])).await;
        assert_eq!(list.method(), http::Method::GET);
        assert!(
            list.uri()
                .query()
                .is_some_and(|query| query.contains("fieldSelector="))
        );

        let (create_request, send) = handle.next_request().await.expect("create request sent");
        assert_eq!(create_request.method(), http::Method::POST);
        let body = request_body_json(create_request).await;
        assert_eq!(body["spec"]["driver"], "example.com");
        assert_eq!(body["spec"]["nodeName"], "node-0");
        assert_eq!(body["spec"]["pool"]["name"], "pool-0");
        assert_eq!(body["spec"]["pool"]["generation"], 1);
        assert_eq!(body["spec"]["pool"]["resourceSliceCount"], 1);
        assert_eq!(body["spec"]["devices"][0]["name"], "widget-0");
        assert_eq!(body["metadata"]["ownerReferences"][0]["uid"], "node-uid-0");
        send.send_response(http::Response::new(Body::from(
            serde_json::to_vec(&body).expect("serialize created slice"),
        )));
    };

    let (_, result) = tokio::join!(script, publisher.publish_once());
    result.expect("publish_once succeeds");
}

#[tokio::test]
async fn publish_once_is_a_noop_when_existing_pool_is_complete() {
    let (publisher, mut handle) =
        mock_publisher(one_pool("pool-0", vec![PoolDevice::new("widget-0")]));

    let script = async {
        respond(&mut handle, 200, &node_json("node-0", "node-uid-0")).await;
        let existing = owned_slice(
            "example.com-node-0-pool-0",
            4,
            "pool-0",
            1,
            vec![device_json("widget-0")],
        );
        respond(&mut handle, 200, &resource_slice_list(vec![existing])).await;

        let extra = tokio::time::timeout(Duration::from_millis(50), handle.next_request()).await;
        assert!(
            extra.is_err(),
            "complete desired state must not be rewritten"
        );
    };

    let (_, result) = tokio::join!(script, publisher.publish_once());
    result.expect("publish_once succeeds");
}

#[tokio::test]
async fn publish_once_is_a_noop_when_existing_devices_are_reordered() {
    let (publisher, mut handle) = mock_publisher(one_pool(
        "pool-0",
        vec![PoolDevice::new("widget-0"), PoolDevice::new("widget-1")],
    ));

    let script = async {
        respond(&mut handle, 200, &node_json("node-0", "node-uid-0")).await;
        let existing = owned_slice(
            "example.com-node-0-pool-0",
            4,
            "pool-0",
            1,
            vec![device_json("widget-1"), device_json("widget-0")],
        );
        respond(&mut handle, 200, &resource_slice_list(vec![existing])).await;

        let extra = tokio::time::timeout(Duration::from_millis(50), handle.next_request()).await;
        assert!(
            extra.is_err(),
            "device ordering alone must not advance the generation"
        );
    };

    let (_, result) = tokio::join!(script, publisher.publish_once());
    result.expect("publish_once succeeds");
}

#[tokio::test]
async fn publish_once_updates_changed_pool_with_next_generation() {
    let (publisher, mut handle) =
        mock_publisher(one_pool("pool-0", vec![PoolDevice::new("widget-1")]));

    let script = async {
        respond(&mut handle, 200, &node_json("node-0", "node-uid-0")).await;
        let existing = owned_slice(
            "example.com-node-0-pool-0",
            4,
            "pool-0",
            1,
            vec![device_json("widget-0")],
        );
        respond(&mut handle, 200, &resource_slice_list(vec![existing])).await;

        let (replace_request, send) = handle.next_request().await.expect("replace request sent");
        assert_eq!(replace_request.method(), http::Method::PUT);
        let body = request_body_json(replace_request).await;
        assert_eq!(body["spec"]["pool"]["generation"], 5);
        assert_eq!(body["spec"]["devices"][0]["name"], "widget-1");
        send.send_response(http::Response::new(Body::from(
            serde_json::to_vec(&body).expect("serialize replaced slice"),
        )));
    };

    let (_, result) = tokio::join!(script, publisher.publish_once());
    result.expect("publish_once succeeds");
}

#[tokio::test]
async fn publish_once_splits_large_pool_with_one_generation_and_count() {
    let (publisher, mut handle) = mock_publisher(one_pool("pool-0", widgets(129)));

    let script = async {
        respond(&mut handle, 200, &node_json("node-0", "node-uid-0")).await;
        respond(&mut handle, 200, &resource_slice_list(vec![])).await;

        for expected_devices in [128, 1] {
            let (request, send) = handle.next_request().await.expect("create request sent");
            assert_eq!(request.method(), http::Method::POST);
            let body = request_body_json(request).await;
            assert_eq!(body["spec"]["pool"]["generation"], 1);
            assert_eq!(body["spec"]["pool"]["resourceSliceCount"], 2);
            assert_eq!(
                body["spec"]["devices"].as_array().map(Vec::len),
                Some(expected_devices)
            );
            send.send_response(http::Response::new(Body::from(
                serde_json::to_vec(&body).expect("serialize created slice"),
            )));
        }
    };

    let (_, result) = tokio::join!(script, publisher.publish_once());
    result.expect("publish_once succeeds");
}

#[tokio::test]
async fn publish_once_completes_new_generation_before_deleting_stale_slices() {
    let (publisher, mut handle) =
        mock_publisher(one_pool("pool-0", vec![PoolDevice::new("widget-0")]));

    let script = async {
        respond(&mut handle, 200, &node_json("node-0", "node-uid-0")).await;
        let current = owned_slice(
            "example.com-node-0-pool-0",
            4,
            "pool-0",
            1,
            vec![device_json("widget-old")],
        );
        let stale = owned_slice(
            "example.com-node-0-removed-pool",
            3,
            "removed-pool",
            1,
            vec![device_json("widget-old")],
        );
        respond(&mut handle, 200, &resource_slice_list(vec![current, stale])).await;

        let (replace_request, send) = handle.next_request().await.expect("replace request sent");
        assert_eq!(replace_request.method(), http::Method::PUT);
        let body = request_body_json(replace_request).await;
        assert_eq!(body["spec"]["pool"]["generation"], 5);
        send.send_response(http::Response::new(Body::from(
            serde_json::to_vec(&body).expect("serialize replaced slice"),
        )));

        let (delete_request, send) = handle.next_request().await.expect("delete request sent");
        assert_eq!(delete_request.method(), http::Method::DELETE);
        assert!(
            delete_request
                .uri()
                .path()
                .ends_with("example.com-node-0-removed-pool")
        );
        send.send_response(
            http::Response::builder()
                .status(200)
                .body(Body::from(
                    serde_json::to_vec(&json::json!({ "status": "Success" }))
                        .expect("serialize status"),
                ))
                .expect("build delete response"),
        );
    };

    let (_, result) = tokio::join!(script, publisher.publish_once());
    result.expect("publish_once succeeds");
}

#[tokio::test]
async fn publish_once_rejects_duplicate_devices_before_mutation() {
    let (publisher, mut handle) = mock_publisher(one_pool(
        "pool-0",
        vec![PoolDevice::new("widget-0"), PoolDevice::new("widget-0")],
    ));

    let script = async {
        respond(&mut handle, 200, &node_json("node-0", "node-uid-0")).await;
        respond(&mut handle, 200, &resource_slice_list(vec![])).await;
        let extra = tokio::time::timeout(Duration::from_millis(50), handle.next_request()).await;
        assert!(extra.is_err(), "invalid input must not mutate slices");
    };

    let (_, result) = tokio::join!(script, publisher.publish_once());
    let error = result.expect_err("duplicate device names must fail");
    assert!(error.to_string().contains("duplicate device names"));
}

#[test]
fn retry_delay_is_bounded() {
    assert_eq!(
        retry_delay(Duration::from_millis(10), Duration::from_millis(50), 1),
        Duration::from_millis(10)
    );
    assert_eq!(
        retry_delay(Duration::from_millis(10), Duration::from_millis(50), 4),
        Duration::from_millis(50)
    );
}

struct TaintedInventory(Vec<InventoryDevice>);

#[tonic::async_trait]
impl ResourcePool for TaintedInventory {
    async fn devices(&self) -> BTreeMap<String, Vec<PoolDevice>> {
        panic!("publication must use the complete inventory override")
    }

    async fn inventory(&self) -> BTreeMap<String, Vec<InventoryDevice>> {
        BTreeMap::from([("pool-0".to_string(), self.0.clone())])
    }
}

fn tainted_widget() -> InventoryDevice {
    use k8s_device_plugin_core::{DeviceTaint, DeviceTaintEffect};
    InventoryDevice::new(PoolDevice::new("widget-0")).with_taint(DeviceTaint::new(
        "example.com/maintenance",
        DeviceTaintEffect::NoSchedule,
    ))
}

fn inventory_publisher(devices: Vec<InventoryDevice>) -> (ResourceSlicePublisher, MockKubeHandle) {
    let (client, handle) = mock_kube_client();
    let pool: Arc<dyn ResourcePool> = Arc::new(TaintedInventory(devices));
    (
        ResourceSlicePublisher::new(client, "example.com", "node-0", pool),
        handle,
    )
}

fn tainted_slice(effect: &str) -> json::Value {
    let mut device = device_json("widget-0");
    device["taints"] = json::json!([{
        "key": "example.com/maintenance", "effect": effect,
        "timeAdded": "2026-10-23T10:00:00Z",
    }]);
    owned_slice("example.com-node-0-pool-0", 4, "pool-0", 1, vec![device])
}

#[test]
fn taint_conversion_preserves_effects_values_and_explicit_times() {
    use k8s_device_plugin_core::{DeviceTaint, DeviceTaintEffect};
    for effect in [
        DeviceTaintEffect::None,
        DeviceTaintEffect::NoSchedule,
        DeviceTaintEffect::NoExecute,
    ] {
        let device = InventoryDevice::new(PoolDevice::new("widget-0")).with_taint(
            DeviceTaint::new("example.com/maintenance", effect)
                .with_value("yes")
                .with_time_added(0),
        );
        let wire = inventory_device_to_wire(&device, "pool-0").unwrap();
        let taint = &wire.taints.unwrap()[0];
        assert_eq!(taint.effect, effect.as_str());
        assert_eq!(taint.value.as_deref(), Some("yes"));
        assert_eq!(taint.time_added.as_ref().unwrap().0.as_second(), 0);
    }
}

#[test]
fn invalid_taints_are_rejected_before_publication() {
    use k8s_device_plugin_core::{DeviceTaint, DeviceTaintEffect};
    let duplicate = tainted_widget().with_taint(DeviceTaint::new(
        "example.com/maintenance",
        DeviceTaintEffect::NoSchedule,
    ));
    let invalid_time = InventoryDevice::new(PoolDevice::new("widget-0")).with_taint(
        DeviceTaint::new("example.com/maintenance", DeviceTaintEffect::NoExecute)
            .with_time_added(i64::MAX),
    );
    let mut too_many = InventoryDevice::new(PoolDevice::new("widget-0"));
    for index in 0..17 {
        too_many = too_many.with_taint(DeviceTaint::new(
            format!("example.com/taint-{index}"),
            DeviceTaintEffect::None,
        ));
    }
    for inventory in [duplicate, invalid_time, too_many] {
        let error = inventory_device_to_wire(&inventory, "pool-0").unwrap_err();
        assert!(matches!(error, kube::Error::Api(status) if status.code == 422));
    }
}

#[tokio::test]
async fn tainted_pool_splits_at_64_devices_with_coherent_generation_and_count() {
    let devices = (0..129)
        .map(|index| {
            let mut inventory = tainted_widget();
            inventory.device.name = format!("widget-{index:03}");
            inventory
        })
        .collect();
    let (publisher, mut handle) = inventory_publisher(devices);
    let script = async {
        respond(&mut handle, 200, &node_json("node-0", "node-uid-0")).await;
        respond(&mut handle, 200, &resource_slice_list(vec![])).await;
        for size in [64, 64, 1] {
            let (request, send) = handle.next_request().await.unwrap();
            let body = request_body_json(request).await;
            assert_eq!(body["spec"]["devices"].as_array().unwrap().len(), size);
            assert_eq!(body["spec"]["pool"]["resourceSliceCount"], 3);
            assert_eq!(body["spec"]["pool"]["generation"], 1);
            assert_eq!(
                body["spec"]["devices"][0]["taints"][0]["effect"],
                "NoSchedule"
            );
            assert!(
                body["spec"]["devices"][0]["taints"][0]
                    .get("timeAdded")
                    .is_none()
            );
            send.send_response(http::Response::new(Body::from(
                serde_json::to_vec(&body).unwrap(),
            )));
        }
    };
    let (_, result) = tokio::join!(script, publisher.publish_once());
    result.unwrap();
}

#[tokio::test]
async fn defaulted_taint_time_and_empty_value_do_not_republish() {
    let (publisher, mut handle) = inventory_publisher(vec![tainted_widget()]);
    let script = async {
        respond(&mut handle, 200, &node_json("node-0", "node-uid-0")).await;
        let mut existing = tainted_slice("NoSchedule");
        existing["spec"]["devices"][0]["taints"][0]["value"] = json::json!("");
        respond(&mut handle, 200, &resource_slice_list(vec![existing])).await;
        assert!(
            tokio::time::timeout(Duration::from_millis(50), handle.next_request())
                .await
                .is_err()
        );
    };
    let (_, result) = tokio::join!(script, publisher.publish_once());
    result.unwrap();
}

#[tokio::test]
async fn changing_a_taint_effect_gets_a_new_time_and_generation() {
    use k8s_device_plugin_core::{DeviceTaint, DeviceTaintEffect};
    let desired = InventoryDevice::new(PoolDevice::new("widget-0")).with_taint(DeviceTaint::new(
        "example.com/maintenance",
        DeviceTaintEffect::NoExecute,
    ));
    let (publisher, mut handle) = inventory_publisher(vec![desired]);
    let script = async {
        respond(&mut handle, 200, &node_json("node-0", "node-uid-0")).await;
        respond(
            &mut handle,
            200,
            &resource_slice_list(vec![tainted_slice("NoSchedule")]),
        )
        .await;
        let (request, send) = handle.next_request().await.unwrap();
        assert_eq!(request.method(), http::Method::PUT);
        let body = request_body_json(request).await;
        assert_eq!(body["spec"]["pool"]["generation"], 5);
        assert_eq!(
            body["spec"]["devices"][0]["taints"][0]["effect"],
            "NoExecute"
        );
        assert!(
            body["spec"]["devices"][0]["taints"][0]
                .get("timeAdded")
                .is_none()
        );
        send.send_response(http::Response::new(Body::from(
            serde_json::to_vec(&body).unwrap(),
        )));
    };
    let (_, result) = tokio::join!(script, publisher.publish_once());
    result.unwrap();
}

#[tokio::test]
async fn removing_a_taint_is_published_with_next_generation() {
    let (publisher, mut handle) = inventory_publisher(vec![PoolDevice::new("widget-0").into()]);
    let script = async {
        respond(&mut handle, 200, &node_json("node-0", "node-uid-0")).await;
        respond(
            &mut handle,
            200,
            &resource_slice_list(vec![tainted_slice("NoSchedule")]),
        )
        .await;
        let (request, send) = handle.next_request().await.unwrap();
        let body = request_body_json(request).await;
        assert_eq!(body["spec"]["pool"]["generation"], 5);
        assert!(body["spec"]["devices"][0].get("taints").is_none());
        send.send_response(http::Response::new(Body::from(
            serde_json::to_vec(&body).unwrap(),
        )));
    };
    let (_, result) = tokio::join!(script, publisher.publish_once());
    result.unwrap();
}

#[tokio::test]
async fn taint_reordering_does_not_advance_generation() {
    use k8s_device_plugin_core::{DeviceTaint, DeviceTaintEffect};
    let desired = tainted_widget().with_taint(DeviceTaint::new(
        "example.com/diagnostic",
        DeviceTaintEffect::None,
    ));
    let (publisher, mut handle) = inventory_publisher(vec![desired]);
    let script = async {
        respond(&mut handle, 200, &node_json("node-0", "node-uid-0")).await;
        let mut existing = tainted_slice("NoSchedule");
        existing["spec"]["devices"][0]["taints"].as_array_mut().unwrap().push(json::json!({
            "key": "example.com/diagnostic", "effect": "None", "timeAdded": "2026-10-23T09:00:00Z",
        }));
        respond(&mut handle, 200, &resource_slice_list(vec![existing])).await;
        assert!(
            tokio::time::timeout(Duration::from_millis(50), handle.next_request())
                .await
                .is_err()
        );
    };
    let (_, result) = tokio::join!(script, publisher.publish_once());
    result.unwrap();
}

#[tokio::test]
async fn newly_added_taint_converges_after_server_defaults_its_time() {
    let (publisher, mut handle) = inventory_publisher(vec![tainted_widget()]);
    let script = async {
        respond(&mut handle, 200, &node_json("node-0", "node-uid-0")).await;
        let existing = owned_slice(
            "example.com-node-0-pool-0",
            4,
            "pool-0",
            1,
            vec![device_json("widget-0")],
        );
        respond(&mut handle, 200, &resource_slice_list(vec![existing])).await;
        let (request, send) = handle.next_request().await.unwrap();
        let mut body = request_body_json(request).await;
        assert_eq!(body["spec"]["pool"]["generation"], 5);
        body["spec"]["devices"][0]["taints"][0]["timeAdded"] = json::json!("2026-10-23T11:00:00Z");
        send.send_response(http::Response::new(Body::from(
            serde_json::to_vec(&body).unwrap(),
        )));
        respond(&mut handle, 200, &node_json("node-0", "node-uid-0")).await;
        respond(&mut handle, 200, &resource_slice_list(vec![body])).await;
        assert!(
            tokio::time::timeout(Duration::from_millis(50), handle.next_request())
                .await
                .is_err()
        );
    };
    let publications = async {
        publisher.publish_once().await?;
        publisher.publish_once().await
    };
    let (_, result) = tokio::join!(script, publications);
    result.unwrap();
}

#[tokio::test]
async fn backend_can_explicitly_change_a_taint_timestamp() {
    let mut desired = tainted_widget();
    desired.taints[0].time_added = Some(0);
    let (publisher, mut handle) = inventory_publisher(vec![desired]);
    let script = async {
        respond(&mut handle, 200, &node_json("node-0", "node-uid-0")).await;
        respond(
            &mut handle,
            200,
            &resource_slice_list(vec![tainted_slice("NoSchedule")]),
        )
        .await;
        let (request, send) = handle.next_request().await.unwrap();
        let body = request_body_json(request).await;
        assert_eq!(body["spec"]["pool"]["generation"], 5);
        assert_eq!(
            body["spec"]["devices"][0]["taints"][0]["timeAdded"],
            "1970-01-01T00:00:00Z"
        );
        send.send_response(http::Response::new(Body::from(
            serde_json::to_vec(&body).unwrap(),
        )));
    };
    let (_, result) = tokio::join!(script, publisher.publish_once());
    result.unwrap();
}

#[test]
fn binding_metadata_is_optional_and_maps_to_the_device() {
    use k8s_device_plugin_core::DeviceBinding;
    let inventory = InventoryDevice::new(PoolDevice::new("widget-0")).with_binding(
        DeviceBinding::new()
            .with_condition("example.com/Attached")
            .with_failure_condition("example.com/AttachFailed")
            .with_node_binding(),
    );
    let wire = inventory_device_to_wire(&inventory, "pool-0").unwrap();
    assert_eq!(
        wire.binding_conditions,
        Some(vec!["example.com/Attached".to_string()])
    );
    assert_eq!(
        wire.binding_failure_conditions,
        Some(vec!["example.com/AttachFailed".to_string()])
    );
    assert_eq!(wire.binds_to_node, Some(true));
    let plain = inventory_device_to_wire(&PoolDevice::new("widget-0").into(), "pool-0").unwrap();
    assert!(plain.binding_conditions.is_none());
    assert!(plain.binding_failure_conditions.is_none());
    assert!(plain.binds_to_node.is_none());
}

#[test]
fn binding_condition_limits_and_duplicates_are_rejected() {
    use k8s_device_plugin_core::DeviceBinding;
    let duplicate = DeviceBinding::new()
        .with_condition("example.com/Ready")
        .with_condition("example.com/Ready");
    let duplicate_failure = DeviceBinding::new()
        .with_failure_condition("example.com/Failed")
        .with_failure_condition("example.com/Failed");
    let mut too_many = DeviceBinding::new();
    let mut too_many_failures = DeviceBinding::new();
    for index in 0..5 {
        too_many = too_many.with_condition(format!("example.com/Ready{index}"));
        too_many_failures =
            too_many_failures.with_failure_condition(format!("example.com/Failed{index}"));
    }
    for binding in [duplicate, duplicate_failure, too_many, too_many_failures] {
        let inventory = InventoryDevice::new(PoolDevice::new("widget-0")).with_binding(binding);
        assert!(
            matches!(inventory_device_to_wire(&inventory, "pool-0"), Err(kube::Error::Api(status)) if status.code == 422)
        );
    }
}

#[tokio::test]
async fn binding_condition_order_and_default_false_do_not_advance_generation() {
    use k8s_device_plugin_core::DeviceBinding;
    let desired = InventoryDevice::new(PoolDevice::new("widget-0")).with_binding(
        DeviceBinding::new()
            .with_condition("example.com/ZReady")
            .with_condition("example.com/AReady"),
    );
    let (publisher, mut handle) = inventory_publisher(vec![desired]);
    let script = async {
        respond(&mut handle, 200, &node_json("node-0", "node-uid-0")).await;
        let mut device = device_json("widget-0");
        device["bindingConditions"] = json::json!(["example.com/AReady", "example.com/ZReady"]);
        device["bindingFailureConditions"] = json::json!([]);
        device["bindsToNode"] = json::json!(false);
        let existing = owned_slice("example.com-node-0-pool-0", 4, "pool-0", 1, vec![device]);
        respond(&mut handle, 200, &resource_slice_list(vec![existing])).await;
        assert!(
            tokio::time::timeout(Duration::from_millis(50), handle.next_request())
                .await
                .is_err()
        );
    };
    let (_, result) = tokio::join!(script, publisher.publish_once());
    result.unwrap();
}

#[tokio::test]
async fn adding_binding_conditions_advances_generation_and_preserves_taints() {
    use k8s_device_plugin_core::DeviceBinding;
    let desired = tainted_widget().with_binding(
        DeviceBinding::new()
            .with_condition("example.com/Attached")
            .with_node_binding(),
    );
    let (publisher, mut handle) = inventory_publisher(vec![desired]);
    let script = async {
        respond(&mut handle, 200, &node_json("node-0", "node-uid-0")).await;
        respond(
            &mut handle,
            200,
            &resource_slice_list(vec![tainted_slice("NoSchedule")]),
        )
        .await;
        let (request, send) = handle.next_request().await.unwrap();
        let body = request_body_json(request).await;
        assert_eq!(body["spec"]["pool"]["generation"], 5);
        assert_eq!(
            body["spec"]["devices"][0]["bindingConditions"],
            json::json!(["example.com/Attached"])
        );
        assert_eq!(
            body["spec"]["devices"][0]["taints"][0]["timeAdded"],
            "2026-10-23T10:00:00Z"
        );
        assert_eq!(body["spec"]["devices"][0]["bindsToNode"], true);
        send.send_response(http::Response::new(Body::from(
            serde_json::to_vec(&body).unwrap(),
        )));
    };
    let (_, result) = tokio::join!(script, publisher.publish_once());
    result.unwrap();
}
