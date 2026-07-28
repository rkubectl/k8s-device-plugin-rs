use std::collections::HashMap;

use http_body_util::BodyExt;
use kube::client::Body;
use serde_json::json;

use super::*;

type MockKubeHandle = tower_test::mock::Handle<http::Request<Body>, http::Response<Body>>;

struct StaticPool(HashMap<String, Vec<PoolDevice>>);

#[tonic::async_trait]
impl ResourcePool for StaticPool {
    async fn devices(&self) -> HashMap<String, Vec<PoolDevice>> {
        self.0.clone()
    }
}

fn one_pool(pool_name: &str, devices: Vec<PoolDevice>) -> StaticPool {
    StaticPool(HashMap::from([(pool_name.to_string(), devices)]))
}

fn mock_publisher(resource_pool: StaticPool) -> (ResourceSlicePublisher, MockKubeHandle) {
    let (mock_service, handle) =
        tower_test::mock::pair::<http::Request<Body>, http::Response<Body>>();
    let client = Client::new(mock_service, "default");
    let publisher = ResourceSlicePublisher::new(client, "example.com", "node-0", resource_pool);
    (publisher, handle)
}

fn node_json(uid: &str) -> serde_json::Value {
    json!({
        "apiVersion": "v1",
        "kind": "Node",
        "metadata": { "name": "node-0", "uid": uid },
    })
}

fn not_found_json() -> serde_json::Value {
    json!({
        "kind": "Status",
        "apiVersion": "v1",
        "status": "Failure",
        "reason": "NotFound",
        "code": 404,
    })
}

fn slice_json(
    name: &str,
    driver: &str,
    generation: i64,
    pool_name: &str,
    devices: Vec<serde_json::Value>,
) -> serde_json::Value {
    json!({
        "apiVersion": "resource.k8s.io/v1",
        "kind": "ResourceSlice",
        "metadata": { "name": name },
        "spec": {
            "driver": driver,
            "nodeName": "node-0",
            "pool": { "name": pool_name, "generation": generation, "resourceSliceCount": 1 },
            "devices": devices,
        },
    })
}

fn device_json(name: &str) -> serde_json::Value {
    json!({ "name": name })
}

async fn respond(
    handle: &mut MockKubeHandle,
    status: u16,
    body: &serde_json::Value,
) -> http::Request<Body> {
    let (request, send) = handle.next_request().await.expect("request sent");
    let bytes = serde_json::to_vec(body).expect("serialize response body");
    let response = http::Response::builder()
        .status(status)
        .body(Body::from(bytes))
        .expect("build response");
    send.send_response(response);
    request
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
        let node_request = respond(&mut handle, 200, &node_json("node-uid-0")).await;
        assert_eq!(node_request.method(), http::Method::GET);

        let get_request = respond(&mut handle, 404, &not_found_json()).await;
        assert_eq!(get_request.method(), http::Method::GET);
        assert!(
            get_request
                .uri()
                .path()
                .contains("example.com-node-0-pool-0")
        );

        let (create_request, send) = handle.next_request().await.expect("create request sent");
        assert_eq!(create_request.method(), http::Method::POST);
        let body = request_body_json(create_request).await;
        assert_eq!(body["spec"]["driver"], "example.com");
        assert_eq!(body["spec"]["nodeName"], "node-0");
        assert_eq!(body["spec"]["pool"]["name"], "pool-0");
        assert_eq!(body["spec"]["pool"]["generation"], 1);
        assert_eq!(body["spec"]["devices"][0]["name"], "widget-0");
        assert_eq!(body["metadata"]["ownerReferences"][0]["uid"], "node-uid-0");
        assert_eq!(body["metadata"]["ownerReferences"][0]["kind"], "Node");

        send.send_response(http::Response::new(Body::from(
            serde_json::to_vec(&body).unwrap(),
        )));
    };

    let (_, result) = tokio::join!(script, publisher.publish_once());
    result.expect("publish_once succeeds");
}

#[tokio::test]
async fn publish_once_updates_slice_in_place_when_devices_change() {
    let (publisher, mut handle) =
        mock_publisher(one_pool("pool-0", vec![PoolDevice::new("widget-1")]));

    let script = async {
        respond(&mut handle, 200, &node_json("node-uid-0")).await;

        let existing = slice_json(
            "example.com-node-0-pool-0",
            "example.com",
            1,
            "pool-0",
            vec![device_json("widget-0")],
        );
        respond(&mut handle, 200, &existing).await;

        let (replace_request, send) = handle.next_request().await.expect("replace request sent");
        assert_eq!(replace_request.method(), http::Method::PUT);
        assert!(
            replace_request
                .uri()
                .path()
                .contains("example.com-node-0-pool-0")
        );
        let body = request_body_json(replace_request).await;
        assert_eq!(body["spec"]["pool"]["generation"], 2);
        assert_eq!(body["spec"]["devices"][0]["name"], "widget-1");

        send.send_response(http::Response::new(Body::from(
            serde_json::to_vec(&body).unwrap(),
        )));
    };

    let (_, result) = tokio::join!(script, publisher.publish_once());
    result.expect("publish_once succeeds");
}

#[tokio::test]
async fn publish_once_is_a_noop_when_devices_unchanged() {
    let (publisher, mut handle) =
        mock_publisher(one_pool("pool-0", vec![PoolDevice::new("widget-0")]));

    let script = async {
        respond(&mut handle, 200, &node_json("node-uid-0")).await;

        let existing = slice_json(
            "example.com-node-0-pool-0",
            "example.com",
            1,
            "pool-0",
            vec![device_json("widget-0")],
        );
        respond(&mut handle, 200, &existing).await;

        // No third request should be sent -- give publish_once a chance to
        // (incorrectly) send one, then confirm the queue is still empty.
        let extra = tokio::time::timeout(Duration::from_millis(50), handle.next_request()).await;
        assert!(
            extra.is_err(),
            "no further request should be sent when devices are unchanged"
        );
    };

    let (_, result) = tokio::join!(script, publisher.publish_once());
    result.expect("publish_once succeeds");
}

#[tokio::test]
async fn spawn_republishes_on_poll_interval() {
    let (publisher, mut handle) =
        mock_publisher(one_pool("pool-0", vec![PoolDevice::new("widget-0")]));
    let publisher = publisher.with_poll_interval(Duration::from_millis(5));

    let script = async {
        for _ in 0..2 {
            respond(&mut handle, 200, &node_json("node-uid-0")).await;
            respond(&mut handle, 404, &not_found_json()).await;
            let (_, send) = handle.next_request().await.expect("create request sent");
            let created = slice_json(
                "example.com-node-0-pool-0",
                "example.com",
                1,
                "pool-0",
                vec![device_json("widget-0")],
            );
            send.send_response(http::Response::new(Body::from(
                serde_json::to_vec(&created).unwrap(),
            )));
        }
    };

    let handle_task = publisher.spawn();
    tokio::time::timeout(Duration::from_secs(1), script)
        .await
        .expect("observed two publish cycles within timeout");
    handle_task.abort();
}
