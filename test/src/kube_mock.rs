//! Shared `kube::Client` mocking helpers for DRA components that talk to
//! the Kubernetes API server (`ClaimResolver`, `ResourceSlicePublisher`,
//! and anything built on top of them) -- the `kube`-API analog of
//! [`crate::registration`]/[`crate::dra_plugin`]'s Unix-socket gRPC mocks.
//!
//! Mirrors the canonical mocking pattern `kube` 4.0.0's own
//! `src/mock_tests.rs` uses: a [`tower_test::mock`] pair backing a real
//! [`kube::Client`], so callers never need a live cluster.

use k8s_openapi::api::core::v1 as corev1;
use k8s_openapi::api::resource::v1 as resourcev1;
use k8s_openapi::apimachinery::pkg::apis::meta::v1 as metav1;
use kube::client;
use serde_json as json;

use super::default;

pub type MockKubeHandle =
    tower_test::mock::Handle<http::Request<client::Body>, http::Response<client::Body>>;

/// Builds a [`kube::Client`] backed by a mock HTTP service, and the
/// [`MockKubeHandle`] used to script its responses.
pub fn mock_kube_client() -> (kube::Client, MockKubeHandle) {
    let (mock_service, handle) =
        tower_test::mock::pair::<http::Request<client::Body>, http::Response<client::Body>>();
    let client = kube::Client::new(mock_service, "default");
    (client, handle)
}

/// Answers the next request on `handle` with `body` and the given HTTP
/// status, returning the request that was answered (so callers can assert
/// on its method/path/body).
pub async fn respond<T: serde::Serialize + ?Sized>(
    handle: &mut MockKubeHandle,
    status: u16,
    body: &T,
) -> http::Request<client::Body> {
    let (request, send) = handle.next_request().await.expect("request sent");
    let bytes = json::to_vec(body).expect("serialize response body");
    let response = http::Response::builder()
        .status(status)
        .body(client::Body::from(bytes))
        .expect("build response");
    send.send_response(response);
    request
}

/// A typed Node for owner-reference lookups.
pub fn node(name: &str, uid: &str) -> corev1::Node {
    corev1::Node {
        metadata: metav1::ObjectMeta {
            name: Some(name.into()),
            uid: Some(uid.into()),
            ..default()
        },
        ..default()
    }
}

/// A typed API-server 404 response.
pub fn not_found() -> metav1::Status {
    metav1::Status {
        status: Some("Failure".into()),
        reason: Some("NotFound".into()),
        code: Some(404),
        ..default()
    }
}

/// A typed ResourceClaim; use None for an absent status.
pub fn resource_claim(
    name: &str,
    namespace: &str,
    uid: &str,
    status: Option<resourcev1::ResourceClaimStatus>,
) -> resourcev1::ResourceClaim {
    resourcev1::ResourceClaim {
        metadata: metav1::ObjectMeta {
            name: Some(name.into()),
            namespace: Some(namespace.into()),
            uid: Some(uid.into()),
            ..default()
        },
        status,
        ..default()
    }
}

/// A typed status with one device allocated by the given driver.
pub fn allocated_claim_status(
    driver: &str,
    pool: &str,
    device: &str,
    request: &str,
) -> resourcev1::ResourceClaimStatus {
    resourcev1::ResourceClaimStatus {
        allocation: Some(resourcev1::AllocationResult {
            devices: Some(resourcev1::DeviceAllocationResult {
                results: Some(vec![resourcev1::DeviceRequestAllocationResult {
                    request: request.into(),
                    driver: driver.into(),
                    pool: pool.into(),
                    device: device.into(),
                    ..default()
                }]),
                ..default()
            }),
            ..default()
        }),
        ..default()
    }
}

/// A typed ResourceSlice for a complete one-slice pool.
pub fn resource_slice(
    name: &str,
    driver: &str,
    node_name: &str,
    generation: i64,
    pool_name: &str,
    devices: Vec<resourcev1::Device>,
) -> resourcev1::ResourceSlice {
    resourcev1::ResourceSlice {
        metadata: metav1::ObjectMeta {
            name: Some(name.into()),
            ..default()
        },
        spec: resourcev1::ResourceSliceSpec {
            driver: driver.into(),
            node_name: Some(node_name.into()),
            pool: resourcev1::ResourcePool {
                name: pool_name.into(),
                generation,
                resource_slice_count: 1,
            },
            devices: Some(devices),
            ..default()
        },
    }
}

/// A typed device with no optional attributes or capacity.
pub fn device(name: &str) -> resourcev1::Device {
    resourcev1::Device {
        name: name.into(),
        ..default()
    }
}

/// A minimal `v1.Node` object, for `ClaimResolver`/`ResourceSlicePublisher`
/// owner-reference lookups.
pub fn node_json(name: &str, uid: &str) -> json::Value {
    json::to_value(node(name, uid)).expect("serialize Node fixture")
}

/// A `Status` object matching what the API server returns for a 404.
pub fn not_found_json() -> json::Value {
    json::to_value(not_found()).expect("serialize Status fixture")
}

/// Raw compatibility fixture for wire-shape tests with arbitrary status.
/// Prefer [`resource_claim`] and [`allocated_claim_status`] for typed fixtures.
pub fn resource_claim_json(
    name: &str,
    namespace: &str,
    uid: &str,
    status: json::Value,
) -> json::Value {
    // Keep this raw compatibility helper for wire-shape and malformed-status
    // tests. Ordinary fixtures should use resource_claim and typed status.
    let mut claim = json::to_value(resource_claim(name, namespace, uid, None))
        .expect("serialize ResourceClaim fixture");
    claim["status"] = status;
    claim
}

/// A `ResourceClaimStatus.allocation` with one allocated device, for use
/// with [`resource_claim_json`].
pub fn allocated_status(pool: &str, device: &str, request: &str) -> json::Value {
    json::to_value(allocated_claim_status("example.com", pool, device, request))
        .expect("serialize allocation fixture")
}

/// Raw compatibility fixture for tests that inject arbitrary device fields.
/// Prefer [`resource_slice`] for ordinary typed fixtures.
pub fn resource_slice_json(
    name: &str,
    driver: &str,
    node_name: &str,
    generation: i64,
    pool_name: &str,
    devices: Vec<json::Value>,
) -> json::Value {
    // Preserve caller-supplied raw device fields for API boundary tests.
    let mut slice = json::to_value(resource_slice(
        name,
        driver,
        node_name,
        generation,
        pool_name,
        vec![],
    ))
    .expect("serialize ResourceSlice fixture");
    slice["spec"]["devices"] = json::Value::Array(devices);
    slice
}

/// A minimal `resource.k8s.io/v1` `Device` entry, for use with
/// [`resource_slice_json`].
pub fn device_json(name: &str) -> json::Value {
    json::to_value(device(name)).expect("serialize Device fixture")
}
