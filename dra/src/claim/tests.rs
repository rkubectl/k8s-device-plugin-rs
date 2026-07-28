use kube::client::Body;
use serde_json::json;

use super::*;

type MockHandle = tower_test::mock::Handle<http::Request<Body>, http::Response<Body>>;

/// Sets up a `ClaimResolver` backed by a mocked `kube::Client` -- see
/// `kube` 4.0.0's own `src/mock_tests.rs` for the canonical version of this
/// pattern, which this mirrors.
fn mock_resolver() -> (ClaimResolver, MockHandle) {
    let (mock_service, handle) =
        tower_test::mock::pair::<http::Request<Body>, http::Response<Body>>();
    let client = Client::new(mock_service, "default");
    (ClaimResolver::new(client), handle)
}

fn claim_json(name: &str, uid: &str, status: serde_json::Value) -> serde_json::Value {
    json!({
        "apiVersion": "resource.k8s.io/v1",
        "kind": "ResourceClaim",
        "metadata": {
            "name": name,
            "namespace": "default",
            "uid": uid,
        },
        "spec": {},
        "status": status,
    })
}

fn allocated_status(pool: &str, device: &str, request: &str) -> serde_json::Value {
    json!({
        "allocation": {
            "devices": {
                "results": [
                    {
                        "request": request,
                        "driver": "example.com",
                        "pool": pool,
                        "device": device,
                    }
                ]
            }
        }
    })
}

/// Waits for the next request on `handle` and responds with `body`, run as
/// its own task so it can proceed concurrently with the `resolve` call that
/// triggers the request -- `next_request` would otherwise deadlock waiting
/// for a request that only gets sent once `resolve` is polled.
fn respond_once(
    mut handle: MockHandle,
    body: serde_json::Value,
) -> tokio::task::JoinHandle<http::Request<Body>> {
    tokio::spawn(async move {
        let (request, send) = handle.next_request().await.expect("request sent");
        let bytes = serde_json::to_vec(&body).expect("serialize response body");
        send.send_response(http::Response::new(Body::from(bytes)));
        request
    })
}

#[tokio::test]
async fn resolve_returns_resolved_claim_for_allocated_claim() {
    let (resolver, handle) = mock_resolver();
    let claim_ref = ClaimRef {
        namespace: "default".to_string(),
        uid: "claim-uid-0".to_string(),
        name: "my-claim".to_string(),
    };
    let body = claim_json(
        "my-claim",
        "claim-uid-0",
        allocated_status("node-0", "widget-0", "req-0"),
    );
    let server = respond_once(handle, body);

    let resolved = resolver
        .resolve(&claim_ref)
        .await
        .expect("resolve succeeds");
    let request = server.await.expect("server task completes");

    assert_eq!(request.method(), http::Method::GET);
    assert_eq!(resolved.claim, claim_ref);
    assert_eq!(resolved.devices.len(), 1);
    assert_eq!(resolved.devices[0].pool_name, "node-0");
    assert_eq!(resolved.devices[0].device_name, "widget-0");
    assert_eq!(resolved.devices[0].request_name, Some("req-0".to_string()));
}

#[tokio::test]
async fn resolve_detects_uid_mismatch() {
    let (resolver, handle) = mock_resolver();
    let claim_ref = ClaimRef {
        namespace: "default".to_string(),
        uid: "expected-uid".to_string(),
        name: "my-claim".to_string(),
    };
    let body = claim_json(
        "my-claim",
        "different-uid",
        allocated_status("node-0", "widget-0", "req-0"),
    );
    let server = respond_once(handle, body);

    let err = resolver
        .resolve(&claim_ref)
        .await
        .expect_err("uid mismatch must be rejected");
    server.await.expect("server task completes");

    assert!(matches!(err, PrepareError::ResolutionFailed(_)));
    assert!(err.to_string().contains("uid mismatch"));
}

#[tokio::test]
async fn resolve_rejects_claim_without_allocation() {
    let (resolver, handle) = mock_resolver();
    let claim_ref = ClaimRef {
        namespace: "default".to_string(),
        uid: "claim-uid-0".to_string(),
        name: "my-claim".to_string(),
    };
    let body = claim_json("my-claim", "claim-uid-0", json!({}));
    let server = respond_once(handle, body);

    let err = resolver
        .resolve(&claim_ref)
        .await
        .expect_err("unallocated claim must be rejected");
    server.await.expect("server task completes");

    assert_eq!(err, PrepareError::ClaimNotAllocated(claim_ref));
}

#[tokio::test]
async fn resolve_all_resolves_a_batch_concurrently() {
    let (resolver, mut handle) = mock_resolver();
    let claim_refs = vec![
        ClaimRef {
            namespace: "default".to_string(),
            uid: "uid-a".to_string(),
            name: "claim-a".to_string(),
        },
        ClaimRef {
            namespace: "default".to_string(),
            uid: "uid-b".to_string(),
            name: "claim-b".to_string(),
        },
    ];

    let server = tokio::spawn(async move {
        for _ in 0..2 {
            let (request, send) = handle.next_request().await.expect("request sent");
            let path = request.uri().path().to_string();
            let (name, uid, pool) = if path.contains("claim-a") {
                ("claim-a", "uid-a", "node-0")
            } else {
                ("claim-b", "uid-b", "node-1")
            };
            let body = claim_json(name, uid, allocated_status(pool, "widget-0", "req-0"));
            let bytes = serde_json::to_vec(&body).expect("serialize response body");
            send.send_response(http::Response::new(Body::from(bytes)));
        }
    });

    let results = resolver.resolve_all(&claim_refs).await;
    server.await.expect("server task completes");

    assert_eq!(results.len(), 2);
    for (claim_ref, result) in &results {
        let resolved = result.as_ref().expect("resolve succeeds");
        let expected_pool = if claim_ref.name == "claim-a" {
            "node-0"
        } else {
            "node-1"
        };
        assert_eq!(resolved.devices[0].pool_name, expected_pool);
    }
}
