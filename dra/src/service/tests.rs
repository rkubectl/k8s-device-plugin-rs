use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;

use k8s_device_plugin_core::ResolvedClaim;
use k8s_device_plugin_test::dra_plugin::MockDraPluginClient;
use k8s_device_plugin_test::kube_mock::MockKubeHandle;
use k8s_device_plugin_test::kube_mock::allocated_status;
use k8s_device_plugin_test::kube_mock::mock_kube_client;
use k8s_device_plugin_test::kube_mock::not_found_json;
use k8s_device_plugin_test::kube_mock::resource_claim_json;
use kube::client::Body;
use tempfile::TempDir;

use super::*;

fn mock_resolver() -> (ClaimResolver, MockKubeHandle) {
    let (client, handle) = mock_kube_client();
    (ClaimResolver::new(client), handle)
}

fn claim_json(name: &str, uid: &str, pool: &str, device: &str, request: &str) -> serde_json::Value {
    resource_claim_json(
        name,
        "default",
        uid,
        allocated_status(pool, device, request),
    )
}

/// Answers exactly `responses.len()` requests on `handle`, matching each by
/// the claim name in the request path (the last URI segment), then stops.
/// A missing entry answers 404, so a resolver failure can be scripted
/// alongside successes in the same batch.
fn spawn_kube_responder(
    mut handle: MockKubeHandle,
    responses: Vec<(&'static str, Option<serde_json::Value>)>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        for _ in 0..responses.len() {
            let (request, send) = handle.next_request().await.expect("request sent");
            let path = request.uri().path().to_string();
            let name = path.rsplit('/').next().unwrap_or_default().to_string();
            let body = responses
                .iter()
                .find(|(claim_name, _)| *claim_name == name)
                .and_then(|(_, body)| body.clone());
            let response = match body {
                Some(body) => {
                    let bytes = serde_json::to_vec(&body).expect("serialize response body");
                    http::Response::new(Body::from(bytes))
                }
                None => {
                    let bytes =
                        serde_json::to_vec(&not_found_json()).expect("serialize response body");
                    http::Response::builder()
                        .status(404)
                        .body(Body::from(bytes))
                        .expect("build 404 response")
                }
            };
            send.send_response(response);
        }
    })
}

#[derive(Default)]
struct FakeClaimPreparer {
    prepare_calls: AtomicUsize,
    unprepare_calls: AtomicUsize,
    fail_uid: Option<String>,
}

impl FakeClaimPreparer {
    fn failing(fail_uid: &str) -> Self {
        Self {
            fail_uid: Some(fail_uid.to_string()),
            ..Default::default()
        }
    }
}

#[tonic::async_trait]
impl ClaimPreparer for FakeClaimPreparer {
    async fn prepare(
        &self,
        claims: &[ResolvedClaim],
    ) -> BTreeMap<ClaimRef, Result<Vec<PreparedDevice>, PrepareError>> {
        self.prepare_calls.fetch_add(1, Ordering::SeqCst);
        claims
            .iter()
            .map(|resolved| {
                let result = if self.fail_uid.as_deref() == Some(resolved.claim.uid.as_str()) {
                    Err(PrepareError::HookFailed("boom".to_string()))
                } else {
                    Ok(resolved
                        .devices
                        .iter()
                        .map(|device| PreparedDevice {
                            request_names: device.request_name.clone().into_iter().collect(),
                            pool_name: device.pool_name.clone(),
                            device_name: device.device_name.clone(),
                            cdi_device_ids: vec![format!(
                                "example.com/{}={}",
                                device.pool_name, device.device_name
                            )],
                        })
                        .collect())
                };
                (resolved.claim.clone(), result)
            })
            .collect()
    }

    async fn unprepare(&self, claim: &ClaimRef) -> Result<(), PrepareError> {
        self.unprepare_calls.fetch_add(1, Ordering::SeqCst);
        if self.fail_uid.as_deref() == Some(claim.uid.as_str()) {
            Err(PrepareError::HookFailed("boom".to_string()))
        } else {
            Ok(())
        }
    }
}

fn wire_claim(name: &str, uid: &str) -> v1::Claim {
    v1::Claim {
        namespace: "default".to_string(),
        uid: uid.to_string(),
        name: name.to_string(),
    }
}

async fn start_service(
    service: DraPluginService,
) -> (
    MockDraPluginClient,
    TempDir,
    JoinHandle<Result<(), transport::Error>>,
) {
    let socket_dir = TempDir::new().expect("create temp dir for plugin socket");
    let socket_path = socket_dir.path().join("plugin.sock");
    let handle = service
        .spawn_at(&socket_path)
        .await
        .expect("spawn DraPluginService");
    let client = MockDraPluginClient::connect(&socket_path)
        .await
        .expect("connect to plugin socket");
    (client, socket_dir, handle)
}

#[tokio::test]
async fn node_prepare_resources_reports_success() {
    let (resolver, kube_handle) = mock_resolver();
    let kube_server = spawn_kube_responder(
        kube_handle,
        vec![(
            "my-claim",
            Some(claim_json(
                "my-claim", "uid-0", "node-0", "widget-0", "req-0",
            )),
        )],
    );
    let service = DraPluginService::new(resolver, FakeClaimPreparer::default());
    let (mut client, _dir, server) = start_service(service).await;

    let response = client
        .prepare_resources(vec![wire_claim("my-claim", "uid-0")])
        .await
        .expect("prepare_resources call succeeds");

    let entry = response.claims.get("uid-0").expect("entry for uid-0");
    assert!(entry.error.is_empty());
    assert_eq!(entry.devices.len(), 1);
    assert_eq!(entry.devices[0].pool_name, "node-0");
    assert_eq!(entry.devices[0].device_name, "widget-0");
    assert_eq!(
        entry.devices[0].cdi_device_ids,
        vec!["example.com/node-0=widget-0"]
    );

    kube_server.await.expect("kube responder completes");
    server.abort();
}

#[tokio::test]
async fn node_prepare_resources_reports_preparer_failure_in_response_not_as_rpc_error() {
    let (resolver, kube_handle) = mock_resolver();
    let kube_server = spawn_kube_responder(
        kube_handle,
        vec![(
            "my-claim",
            Some(claim_json(
                "my-claim", "uid-0", "node-0", "widget-0", "req-0",
            )),
        )],
    );
    let service = DraPluginService::new(resolver, FakeClaimPreparer::failing("uid-0"));
    let (mut client, _dir, server) = start_service(service).await;

    let response = client
        .prepare_resources(vec![wire_claim("my-claim", "uid-0")])
        .await
        .expect("RPC call itself must succeed even when the claim fails to prepare");

    let entry = response.claims.get("uid-0").expect("entry for uid-0");
    assert!(!entry.error.is_empty());
    assert!(entry.devices.is_empty());

    kube_server.await.expect("kube responder completes");
    server.abort();
}

#[tokio::test]
async fn node_prepare_resources_keeps_every_claim_when_one_fails_to_resolve() {
    let (resolver, kube_handle) = mock_resolver();
    let kube_server = spawn_kube_responder(
        kube_handle,
        vec![
            (
                "good-claim",
                Some(claim_json(
                    "good-claim",
                    "uid-good",
                    "node-0",
                    "widget-0",
                    "req-0",
                )),
            ),
            ("missing-claim", None),
        ],
    );
    let service = DraPluginService::new(resolver, FakeClaimPreparer::default());
    let (mut client, _dir, server) = start_service(service).await;

    let response = client
        .prepare_resources(vec![
            wire_claim("good-claim", "uid-good"),
            wire_claim("missing-claim", "uid-missing"),
        ])
        .await
        .expect("prepare_resources call succeeds");

    assert_eq!(
        response.claims.len(),
        2,
        "every requested claim must have an entry"
    );

    let good = response.claims.get("uid-good").expect("entry for uid-good");
    assert!(good.error.is_empty());
    assert_eq!(good.devices.len(), 1);

    let missing = response
        .claims
        .get("uid-missing")
        .expect("entry for uid-missing");
    assert!(!missing.error.is_empty());

    kube_server.await.expect("kube responder completes");
    server.abort();
}

#[tokio::test]
async fn node_prepare_resources_is_idempotent_for_repeated_calls() {
    let (resolver, kube_handle) = mock_resolver();
    let kube_server = spawn_kube_responder(
        kube_handle,
        vec![
            (
                "my-claim",
                Some(claim_json(
                    "my-claim", "uid-0", "node-0", "widget-0", "req-0",
                )),
            ),
            (
                "my-claim",
                Some(claim_json(
                    "my-claim", "uid-0", "node-0", "widget-0", "req-0",
                )),
            ),
        ],
    );
    let service = DraPluginService::new(resolver, FakeClaimPreparer::default());
    let (mut client, _dir, server) = start_service(service).await;

    let first = client
        .prepare_resources(vec![wire_claim("my-claim", "uid-0")])
        .await
        .expect("first prepare_resources call");
    let second = client
        .prepare_resources(vec![wire_claim("my-claim", "uid-0")])
        .await
        .expect("second prepare_resources call");

    assert_eq!(first.claims["uid-0"].error, second.claims["uid-0"].error);
    assert_eq!(
        first.claims["uid-0"].devices,
        second.claims["uid-0"].devices
    );

    kube_server.await.expect("kube responder completes");
    server.abort();
}

#[tokio::test]
async fn node_unprepare_resources_reports_success() {
    let (resolver, _kube_handle) = mock_resolver();
    let service = DraPluginService::new(resolver, FakeClaimPreparer::default());
    let (mut client, _dir, server) = start_service(service).await;

    let response = client
        .unprepare_resources(vec![wire_claim("my-claim", "uid-0")])
        .await
        .expect("unprepare_resources call");

    let entry = response.claims.get("uid-0").expect("entry for uid-0");
    assert!(entry.error.is_empty());

    server.abort();
}

#[tokio::test]
async fn node_unprepare_resources_reports_failure_in_response_not_as_rpc_error() {
    let (resolver, _kube_handle) = mock_resolver();
    let service = DraPluginService::new(resolver, FakeClaimPreparer::failing("uid-0"));
    let (mut client, _dir, server) = start_service(service).await;

    let response = client
        .unprepare_resources(vec![wire_claim("my-claim", "uid-0")])
        .await
        .expect("RPC call itself must succeed even when unprepare fails");

    let entry = response.claims.get("uid-0").expect("entry for uid-0");
    assert!(!entry.error.is_empty());

    server.abort();
}
