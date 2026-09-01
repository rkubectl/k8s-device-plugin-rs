use k8s_device_plugin_test::kube_mock::allocated_status;
use k8s_device_plugin_test::kube_mock::mock_kube_client;
use k8s_device_plugin_test::kube_mock::resource_claim_json;
use kube::client::Body;
use serde_json::json;
use std::time::Duration;

use super::*;

fn mock_resolver() -> (
    ClaimResolver,
    k8s_device_plugin_test::kube_mock::MockKubeHandle,
) {
    let (client, handle) = mock_kube_client();
    (ClaimResolver::new(client), handle)
}

/// Waits for the next request on `handle` and responds with `body`, run as
/// its own task so it can proceed concurrently with the `resolve` call that
/// triggers the request -- `next_request` would otherwise deadlock waiting
/// for a request that only gets sent once `resolve` is polled.
fn respond_once(
    mut handle: k8s_device_plugin_test::kube_mock::MockKubeHandle,
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
    let body = resource_claim_json(
        "my-claim",
        "default",
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
    let body = resource_claim_json(
        "my-claim",
        "default",
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
    let body = resource_claim_json("my-claim", "default", "claim-uid-0", json!({}));
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
            let body = resource_claim_json(
                name,
                "default",
                uid,
                allocated_status(pool, "widget-0", "req-0"),
            );
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

#[tokio::test]
async fn resolve_retries_a_transient_api_failure() {
    let (resolver, mut handle) = mock_resolver();
    let resolver = resolver.with_retry_policy(2, Duration::ZERO);
    let claim_ref = ClaimRef {
        namespace: "default".to_string(),
        uid: "claim-uid-0".to_string(),
        name: "my-claim".to_string(),
    };
    let expected = resource_claim_json(
        "my-claim",
        "default",
        "claim-uid-0",
        allocated_status("pool-0", "widget-0", "req-0"),
    );

    let server = tokio::spawn(async move {
        let (first, send) = handle.next_request().await.expect("first request sent");
        assert_eq!(first.method(), http::Method::GET);
        send.send_response(
            http::Response::builder()
                .status(500)
                .body(Body::from(
                    serde_json::to_vec(&json!({
                        "status": "Failure",
                        "reason": "InternalError",
                        "code": 500,
                    }))
                    .expect("serialize error status"),
                ))
                .expect("build error response"),
        );

        let (second, send) = handle.next_request().await.expect("retry request sent");
        assert_eq!(second.method(), http::Method::GET);
        send.send_response(http::Response::new(Body::from(
            serde_json::to_vec(&expected).expect("serialize resolved claim"),
        )));
    });

    let resolved = resolver.resolve(&claim_ref).await.expect("retry succeeds");
    server.await.expect("server task completes");
    assert_eq!(resolved.devices[0].pool_name, "pool-0");
}

#[tokio::test]
async fn resolve_all_honors_the_concurrency_bound_and_request_order() {
    let (resolver, mut handle) = mock_resolver();
    let resolver = resolver.with_max_concurrent_resolves(1);
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
    let resolve = tokio::spawn(async move { resolver.resolve_all(&claim_refs).await });

    let (first, send) = handle.next_request().await.expect("first request sent");
    assert!(first.uri().path().contains("claim-a"));
    let second_before_first_response =
        tokio::time::timeout(Duration::from_millis(20), handle.next_request()).await;
    assert!(
        second_before_first_response.is_err(),
        "the configured bound must prevent a second in-flight request"
    );
    send.send_response(http::Response::new(Body::from(
        serde_json::to_vec(&resource_claim_json(
            "claim-a",
            "default",
            "uid-a",
            allocated_status("pool-a", "widget-0", "req-0"),
        ))
        .expect("serialize first claim"),
    )));

    let (second, send) = handle.next_request().await.expect("second request sent");
    assert!(second.uri().path().contains("claim-b"));
    send.send_response(http::Response::new(Body::from(
        serde_json::to_vec(&resource_claim_json(
            "claim-b",
            "default",
            "uid-b",
            allocated_status("pool-b", "widget-0", "req-0"),
        ))
        .expect("serialize second claim"),
    )));

    let results = resolve.await.expect("resolution task completes");
    assert_eq!(results[0].0.name, "claim-a");
    assert_eq!(results[1].0.name, "claim-b");
}
