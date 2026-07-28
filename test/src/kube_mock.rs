//! Shared `kube::Client` mocking helpers for DRA components that talk to
//! the Kubernetes API server (`ClaimResolver`, `ResourceSlicePublisher`,
//! and anything built on top of them) -- the `kube`-API analog of
//! [`crate::registration`]/[`crate::dra_plugin`]'s Unix-socket gRPC mocks.
//!
//! Mirrors the canonical mocking pattern `kube` 4.0.0's own
//! `src/mock_tests.rs` uses: a [`tower_test::mock`] pair backing a real
//! [`kube::Client`], so callers never need a live cluster.

use kube::Client;
use kube::client::Body;
use serde_json::json;

pub type MockKubeHandle = tower_test::mock::Handle<http::Request<Body>, http::Response<Body>>;

/// Builds a [`kube::Client`] backed by a mock HTTP service, and the
/// [`MockKubeHandle`] used to script its responses.
pub fn mock_kube_client() -> (Client, MockKubeHandle) {
    let (mock_service, handle) =
        tower_test::mock::pair::<http::Request<Body>, http::Response<Body>>();
    let client = Client::new(mock_service, "default");
    (client, handle)
}

/// Answers the next request on `handle` with `body` and the given HTTP
/// status, returning the request that was answered (so callers can assert
/// on its method/path/body).
pub async fn respond(
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

/// A minimal `v1.Node` object, for `ClaimResolver`/`ResourceSlicePublisher`
/// owner-reference lookups.
pub fn node_json(name: &str, uid: &str) -> serde_json::Value {
    json!({
        "apiVersion": "v1",
        "kind": "Node",
        "metadata": { "name": name, "uid": uid },
    })
}

/// A `Status` object matching what the API server returns for a 404.
pub fn not_found_json() -> serde_json::Value {
    json!({
        "kind": "Status",
        "apiVersion": "v1",
        "status": "Failure",
        "reason": "NotFound",
        "code": 404,
    })
}

/// A minimal `resource.k8s.io/v1` `ResourceClaim` with the given `status`
/// (build with [`allocated_status`] for the common allocated case, or pass
/// `serde_json::json!({})` for an unallocated claim).
pub fn resource_claim_json(
    name: &str,
    namespace: &str,
    uid: &str,
    status: serde_json::Value,
) -> serde_json::Value {
    json!({
        "apiVersion": "resource.k8s.io/v1",
        "kind": "ResourceClaim",
        "metadata": { "name": name, "namespace": namespace, "uid": uid },
        "spec": {},
        "status": status,
    })
}

/// A `ResourceClaimStatus.allocation` with one allocated device, for use
/// with [`resource_claim_json`].
pub fn allocated_status(pool: &str, device: &str, request: &str) -> serde_json::Value {
    json!({
        "allocation": {
            "devices": {
                "results": [
                    { "request": request, "driver": "example.com", "pool": pool, "device": device }
                ]
            }
        }
    })
}

/// A minimal `resource.k8s.io/v1` `ResourceSlice`.
pub fn resource_slice_json(
    name: &str,
    driver: &str,
    node_name: &str,
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
            "nodeName": node_name,
            "pool": { "name": pool_name, "generation": generation, "resourceSliceCount": 1 },
            "devices": devices,
        },
    })
}

/// A minimal `resource.k8s.io/v1` `Device` entry, for use with
/// [`resource_slice_json`].
pub fn device_json(name: &str) -> serde_json::Value {
    json!({ "name": name })
}
