use std::collections::BTreeMap;

use http_body_util::BodyExt;
use k8s_device_plugin_test::kube_mock::MockKubeHandle;
use k8s_device_plugin_test::kube_mock::device_json;
use k8s_device_plugin_test::kube_mock::mock_kube_client;
use k8s_device_plugin_test::kube_mock::node_json;
use k8s_device_plugin_test::kube_mock::resource_slice_json;
use k8s_device_plugin_test::kube_mock::respond;
use kube::client::Body;
use serde_json::json;

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
    json!({
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
    devices: Vec<serde_json::Value>,
) -> serde_json::Value {
    let mut slice = resource_slice_json(
        name,
        "example.com",
        "node-0",
        generation,
        pool_name,
        devices,
    );
    slice["spec"]["pool"]["resourceSliceCount"] = json!(slice_count);
    slice["metadata"]["ownerReferences"] = json!([{
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
                    serde_json::to_vec(&json!({ "status": "Success" })).expect("serialize status"),
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
