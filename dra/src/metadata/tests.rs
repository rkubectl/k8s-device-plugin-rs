use std::os::unix::fs::MetadataExt;
use std::os::unix::fs::symlink;

use super::*;

fn new_runtime(root: &Path) -> Arc<MetadataRuntime> {
    Arc::new(
        MetadataRuntime::new(
            "dra.example.com".into(),
            DeviceMetadataOptions::v1beta1()
                .with_plugin_directories(root.join("plugin"), root.join("plugin"))
                .with_cdi_directory(root.join("cdi")),
        )
        .unwrap(),
    )
}
fn claim() -> ResourceClaim {
    ResourceClaim {
        metadata: metav1::ObjectMeta {
            namespace: Some("default".into()),
            name: Some("test-claim".into()),
            uid: Some("claim-uid".into()),
            ..default()
        },
        spec: resourcev1::ResourceClaimSpec {
            devices: Some(resourcev1::DeviceClaim {
                requests: Some(
                    ["a", "b"]
                        .map(|name| resourcev1::DeviceRequest {
                            name: name.into(),
                            exactly: Some(resourcev1::ExactDeviceRequest {
                                device_class_name: "widgets".into(),
                                ..default()
                            }),
                            ..default()
                        })
                        .into(),
                ),
                ..default()
            }),
        },
        status: Some(resourcev1::ResourceClaimStatus {
            allocation: Some(resourcev1::AllocationResult {
                devices: Some(resourcev1::DeviceAllocationResult {
                    results: Some(
                        [("a", "widget-0"), ("b", "widget-1")]
                            .map(
                                |(request, device)| resourcev1::DeviceRequestAllocationResult {
                                    request: request.into(),
                                    driver: "dra.example.com".into(),
                                    pool: "pool-a".into(),
                                    device: device.into(),
                                    ..default()
                                },
                            )
                            .into(),
                    ),
                    ..default()
                }),
                ..default()
            }),
            ..default()
        }),
    }
}
fn prepared(request: &str, device: &str) -> PreparedDeviceWithMetadata {
    PreparedDeviceWithMetadata::new(PreparedDevice {
        request_names: vec![request.into()],
        pool_name: "pool-a".into(),
        device_name: device.into(),
        cdi_device_ids: vec![format!("dra.example.com/widget={device}")],
    })
    .with_attribute("model", "widget")
}
fn plan(runtime: &MetadataRuntime) -> Plan {
    Plan::build(
        runtime,
        &claim(),
        vec![prepared("a", "widget-0"), prepared("b", "widget-1")],
    )
    .unwrap()
}
fn data(root: &Path, request: &str) -> PathBuf {
    root.join("plugin/dra-device-metadata/claim-uid")
        .join(request)
        .join("metadata.json")
}

#[test]
fn beta_schema_paths_and_request_cdi_edits_are_isolated() {
    let tmp = tempfile::tempdir().unwrap();
    let runtime = new_runtime(tmp.path());
    let plan = plan(&runtime);
    assert_eq!(plan.devices.len(), 4);
    assert_eq!(
        plan.devices[0].cdi_device_ids,
        vec!["dra.example.com/widget=widget-0"]
    );
    for (request, device) in [("a", "widget-0"), ("b", "widget-1")] {
        let json = json::from_str::<json::Value>(&plan.documents[request]).unwrap();
        assert_eq!(
            json,
            json::json!({
                "apiVersion": "metadata.resource.k8s.io/v1beta1",
                "kind": "DeviceMetadata",
                "metadata": {
                    "name": "test-claim",
                    "namespace": "default",
                    "uid": "claim-uid",
                    "generation": 1
                },
                "requests": [{
                    "name": request,
                    "devices": [{
                        "driver": "dra.example.com",
                        "pool": "pool-a",
                        "name": device,
                        "attributes": {"model": {"string": "widget"}}
                    }]
                }]
            })
        );
        assert!(plan.documents[request].ends_with('\n'));
        let spec = json::from_str::<json::Value>(&plan.specs[request]).unwrap();
        assert_eq!(
            spec["devices"][0]["containerEdits"]["mounts"][0]["options"],
            json::json!(["ro", "bind"])
        );
        assert_eq!(
            spec["devices"][0]["containerEdits"]["mounts"][0]["containerPath"],
            format!(
                "{CONTAINER_ROOT}/resourceclaims/test-claim/{request}/dra.example.com-metadata.json"
            )
        );
    }
    let selected = plan
        .devices
        .iter()
        .filter(|d| d.request_names.is_empty() || d.request_names.iter().any(|r| r == "a"))
        .flat_map(|d| d.cdi_device_ids.iter())
        .collect::<Vec<_>>();
    assert_eq!(selected.len(), 2);
    assert!(selected.iter().all(|id| !id.ends_with("_b")));
}
#[test]
fn template_subrequest_paths_and_full_json_reference() {
    let tmp = tempfile::tempdir().unwrap();
    let runtime = new_runtime(tmp.path());
    let mut claim = claim();
    claim
        .spec
        .devices
        .as_mut()
        .unwrap()
        .requests
        .as_mut()
        .unwrap()[0] = resourcev1::DeviceRequest {
        name: "a".into(),
        first_available: Some(vec![resourcev1::DeviceSubRequest {
            name: "fast".into(),
            device_class_name: "widgets".into(),
            ..default()
        }]),
        ..default()
    };
    claim.metadata.annotations = Some(BTreeMap::from([(
        POD_CLAIM_NAME.into(),
        "pod-local".into(),
    )]));
    claim
        .status
        .as_mut()
        .unwrap()
        .allocation
        .as_mut()
        .unwrap()
        .devices
        .as_mut()
        .unwrap()
        .results
        .as_mut()
        .unwrap()[0]
        .request = "a/fast".into();
    let plan = Plan::build(&runtime, &claim, vec![prepared("a/fast", "widget-0")]).unwrap();
    assert_eq!(plan.devices[0].request_names, vec!["a"]);
    let json = json::from_str::<json::Value>(&plan.documents["a"]).unwrap();
    assert_eq!(json["podClaimName"], "pod-local");
    assert_eq!(json["requests"][0]["name"], "a/fast");
    let spec = json::from_str::<json::Value>(&plan.specs["a"]).unwrap();
    assert_eq!(
        spec["devices"][0]["containerEdits"]["mounts"][0]["containerPath"],
        format!(
            "{CONTAINER_ROOT}/resourceclaimtemplates/pod-local/a/dra.example.com-metadata.json"
        )
    );
}
#[test]
fn all_requests_preserves_physical_cdi_scope_without_inventing_metadata_memberships() {
    let tmp = tempfile::tempdir().unwrap();
    let runtime = new_runtime(tmp.path());
    let mut device = prepared("a", "widget-0");
    device.device.request_names.clear();
    let plan = Plan::build(&runtime, &claim(), vec![device]).unwrap();
    assert!(plan.devices[0].request_names.is_empty());
    assert_eq!(plan.documents.len(), 1);
    assert!(plan.documents.contains_key("a"));
    assert!(!plan.documents.contains_key("b"));
}
#[test]
fn rejects_foreign_unallocated_conflicting_or_malformed_payloads() {
    let tmp = tempfile::tempdir().unwrap();
    let runtime = new_runtime(tmp.path());
    for device in [
        prepared("b", "widget-0"),
        prepared("../escape", "widget-0"),
        prepared("a", "foreign"),
    ] {
        assert!(Plan::build(&runtime, &claim(), vec![device]).is_err());
    }
    for key in [
        "../escape",
        "bad-key",
        "bad.key",
        "domain/",
        "x/y/z",
        &"x".repeat(33),
    ] {
        assert!(
            Plan::build(
                &runtime,
                &claim(),
                vec![prepared("a", "widget-0").with_attribute(key, 1)]
            )
            .is_err()
        );
    }
    for value in ["x".repeat(65).into(), AttributeValue::version("not-semver")] {
        assert!(
            Plan::build(
                &runtime,
                &claim(),
                vec![prepared("a", "widget-0").with_attribute("invalid", value)]
            )
            .is_err()
        );
    }
    assert!(
        Plan::build(
            &runtime,
            &claim(),
            vec![
                prepared("a", "widget-0"),
                prepared("a", "widget-0").with_attribute("model", "other")
            ]
        )
        .is_err()
    );
    let mut foreign = claim();
    foreign
        .status
        .as_mut()
        .unwrap()
        .allocation
        .as_mut()
        .unwrap()
        .devices
        .as_mut()
        .unwrap()
        .results
        .as_mut()
        .unwrap()[0]
        .driver = "foreign.example.com".into();
    assert!(Plan::build(&runtime, &foreign, vec![prepared("a", "widget-0")]).is_err());
}
#[test]
fn retries_preserve_inode_and_snapshot_and_restart_recovers() {
    let tmp = tempfile::tempdir().unwrap();
    let runtime = new_runtime(tmp.path());
    let plan = plan(&runtime);
    store::persist(&runtime, &plan).unwrap();
    let before = fs::metadata(data(tmp.path(), "a")).unwrap();
    store::persist(&runtime, &plan).unwrap();
    let after = fs::metadata(data(tmp.path(), "a")).unwrap();
    assert_eq!(
        (before.ino(), before.mtime_nsec()),
        (after.ino(), after.mtime_nsec())
    );
    assert_eq!(after.mode() & 0o777, 0o644);
    let restarted = new_runtime(tmp.path());
    store::persist(&restarted, &plan).unwrap();
    let mut changed = prepared("a", "widget-0");
    changed.attributes.insert("model".into(), "changed".into());
    let changed =
        Plan::build(&runtime, &claim(), vec![changed, prepared("b", "widget-1")]).unwrap();
    assert!(store::persist(&runtime, &changed).is_err());
    assert_eq!(
        fs::metadata(data(tmp.path(), "a")).unwrap().ino(),
        before.ino()
    );
    store::cleanup(&restarted, &plan.claim).unwrap();
    store::cleanup(&restarted, &plan.claim).unwrap();
    assert!(!data(tmp.path(), "a").exists());
}
#[test]
fn partial_publication_replays_after_failure_without_replacing_data() {
    let tmp = tempfile::tempdir().unwrap();
    let runtime = new_runtime(tmp.path());
    let plan = plan(&runtime);
    fs::create_dir_all(tmp.path().join("cdi")).unwrap();
    let blocked = tmp
        .path()
        .join("cdi/dra.example.com_metadata_claim-uid_b.json");
    fs::write(&blocked, "foreign").unwrap();
    assert!(store::persist(&runtime, &plan).is_err());
    let before = fs::metadata(data(tmp.path(), "a")).unwrap().ino();
    fs::remove_file(&blocked).unwrap();
    store::persist(&runtime, &plan).unwrap();
    assert_eq!(fs::metadata(data(tmp.path(), "a")).unwrap().ino(), before);
    store::cleanup(&runtime, &plan.claim).unwrap();
}
#[test]
fn symlink_descendants_are_refused_and_outside_files_survive() {
    let tmp = tempfile::tempdir().unwrap();
    let runtime = new_runtime(tmp.path());
    let plan = plan(&runtime);
    let outside = tmp.path().join("outside");
    fs::create_dir_all(&outside).unwrap();
    fs::create_dir_all(tmp.path().join("plugin/dra-device-metadata")).unwrap();
    symlink(
        &outside,
        tmp.path().join("plugin/dra-device-metadata/claim-uid"),
    )
    .unwrap();
    assert!(store::persist(&runtime, &plan).is_err());
    assert!(fs::read_dir(&outside).unwrap().next().is_none());
    fs::remove_file(tmp.path().join("plugin/dra-device-metadata/claim-uid")).unwrap();
    store::persist(&runtime, &plan).unwrap();
    let protected = outside.join("protected");
    fs::write(&protected, "keep").unwrap();
    fs::remove_file(data(tmp.path(), "a")).unwrap();
    symlink(&protected, data(tmp.path(), "a")).unwrap();
    assert!(store::persist(&runtime, &plan).is_err());
    store::cleanup(&runtime, &plan.claim).unwrap();
    assert_eq!(fs::read_to_string(&protected).unwrap(), "keep");
}
#[test]
fn stale_uid_cleanup_preserves_replacement_and_foreign_files() {
    let tmp = tempfile::tempdir().unwrap();
    let runtime = new_runtime(tmp.path());
    let old = plan(&runtime);
    store::persist(&runtime, &old).unwrap();
    let mut replacement = claim();
    replacement.metadata.uid = Some("new-uid".into());
    let new = Plan::build(&runtime, &replacement, vec![prepared("a", "widget-0")]).unwrap();
    store::persist(&runtime, &new).unwrap();
    let foreign = tmp
        .path()
        .join("plugin/dra-device-metadata/claim-uid/foreign");
    fs::write(&foreign, "keep").unwrap();
    assert!(store::cleanup(&runtime, &old.claim).is_err());
    assert!(foreign.exists());
    assert!(
        tmp.path()
            .join("plugin/dra-device-metadata/claim-uid/journal.json")
            .exists()
    );
    fs::remove_file(&foreign).unwrap();
    store::cleanup(&runtime, &old.claim).unwrap();
    assert!(
        tmp.path()
            .join("plugin/dra-device-metadata/new-uid/a/metadata.json")
            .exists()
    );
    let mut stale = old.claim;
    stale.uid = "new-uid".into();
    stale.name = "wrong-name".into();
    assert!(store::cleanup(&runtime, &stale).is_err());
}
#[test]
fn cleanup_recovers_empty_directory_after_final_journal_removal() {
    let tmp = tempfile::tempdir().unwrap();
    let runtime = new_runtime(tmp.path());
    let plan = plan(&runtime);
    fs::create_dir_all(tmp.path().join("plugin/dra-device-metadata/claim-uid")).unwrap();
    store::cleanup(&runtime, &plan.claim).unwrap();
}
#[tokio::test]
async fn uid_lock_serializes_operations_and_is_shared_through_guard_ownership() {
    let tmp = tempfile::tempdir().unwrap();
    let runtime = new_runtime(tmp.path());
    let claim = plan(&runtime).claim;
    let guards = Arc::new(runtime.lock_claims(std::slice::from_ref(&claim)).await);
    let held = Arc::clone(&guards);
    drop(guards);
    assert!(
        tokio::time::timeout(
            Duration::from_millis(10),
            runtime.lock_claims(std::slice::from_ref(&claim))
        )
        .await
        .is_err()
    );
    drop(held);
    let _next = runtime.lock_claims(std::slice::from_ref(&claim)).await;
}

#[test]
fn foreign_shared_cdi_file_is_never_deleted_by_our_cleanup() {
    let tmp = tempfile::tempdir().unwrap();
    let runtime = new_runtime(tmp.path());
    let plan = plan(&runtime);
    fs::create_dir_all(tmp.path().join("cdi")).unwrap();
    let foreign = tmp
        .path()
        .join("cdi/dra.example.com_metadata_claim-uid_a.json");
    fs::write(&foreign, "foreign").unwrap();
    assert!(store::persist(&runtime, &plan).is_err());
    assert!(store::cleanup(&runtime, &plan.claim).is_err());
    assert_eq!(fs::read_to_string(&foreign).unwrap(), "foreign");
    fs::remove_file(foreign).unwrap();
    store::cleanup(&runtime, &plan.claim).unwrap();
}

#[tokio::test]
async fn rpc_batches_once_preserves_other_claims_and_retries_failed_cleanup() {
    use std::sync::atomic::AtomicBool;
    use std::sync::atomic::AtomicUsize;
    use std::sync::atomic::Ordering;

    use k8s_device_plugin_proto::dra::v1::Claim;
    use k8s_device_plugin_proto::dra::v1::DraPlugin as _;
    use k8s_device_plugin_proto::dra::v1::NodePrepareResourcesRequest;
    use k8s_device_plugin_proto::dra::v1::NodeUnprepareResourcesRequest;
    use k8s_device_plugin_test::kube_mock::mock_kube_client;
    #[derive(Debug, Default)]
    struct Backend {
        prepares: AtomicUsize,
        unprepare_fails: AtomicBool,
    }
    #[async_trait::async_trait]
    impl ClaimPreparer for Backend {
        async fn prepare(
            &self,
            claims: &[ResolvedClaim],
        ) -> BTreeMap<ClaimRef, Result<Vec<PreparedDevice>, PrepareError>> {
            self.prepares.fetch_add(1, Ordering::SeqCst);
            claims
                .iter()
                .map(|c| (c.claim.clone(), Ok(vec![prepared("a", "widget-0").device])))
                .collect()
        }
        async fn unprepare(&self, _: &ClaimRef) -> Result<(), PrepareError> {
            if self.unprepare_fails.load(Ordering::SeqCst) {
                Err(PrepareError::HookFailed("backend cleanup failed".into()))
            } else {
                Ok(())
            }
        }
    }
    let tmp = tempfile::tempdir().unwrap();
    fs::create_dir_all(tmp.path().join("cdi")).unwrap();
    let foreign = tmp.path().join("cdi/dra.example.com_metadata_one_a.json");
    fs::write(&foreign, "foreign").unwrap();
    let (client, mut handle) = mock_kube_client();
    let backend = Arc::new(Backend::default());
    let service = DraPluginService::new(ClaimResolver::new(client), Arc::clone(&backend))
        .with_device_metadata(
            "dra.example.com",
            DeviceMetadataOptions::v1beta1()
                .with_plugin_directories(tmp.path().join("plugin"), tmp.path().join("plugin"))
                .with_cdi_directory(tmp.path().join("cdi")),
        )
        .unwrap();
    let responder = tokio::spawn(async move {
        for _ in 0..2 {
            let (request, send) = handle.next_request().await.unwrap();
            let name = if request.uri().path().ends_with("/one") {
                "one"
            } else {
                "two"
            };
            let mut object = claim();
            object.metadata.name = Some(name.into());
            object.metadata.uid = Some(name.into());
            send.send_response(
                http::Response::builder()
                    .status(200)
                    .body(kube::client::Body::from(json::to_vec(&object).unwrap()))
                    .unwrap(),
            );
        }
        // No ResourceClaim lookup is needed during unprepare, even after deletion.
    });
    let refs = ["one", "two"]
        .into_iter()
        .map(|name| Claim {
            namespace: "default".into(),
            name: name.into(),
            uid: name.into(),
        })
        .collect::<Vec<_>>();
    let result = service
        .node_prepare_resources(tonic::Request::new(NodePrepareResourcesRequest {
            claims: refs.clone(),
        }))
        .await
        .unwrap()
        .into_inner();
    responder.await.unwrap();
    assert_eq!(backend.prepares.load(Ordering::SeqCst), 1);
    assert!(!result.claims["one"].error.is_empty());
    assert!(result.claims["two"].error.is_empty());
    assert_eq!(result.claims["two"].devices.len(), 2);
    backend.unprepare_fails.store(true, Ordering::SeqCst);
    let result = service
        .node_unprepare_resources(tonic::Request::new(NodeUnprepareResourcesRequest {
            claims: vec![refs[1].clone()],
        }))
        .await
        .unwrap()
        .into_inner();
    assert!(!result.claims["two"].error.is_empty());
    assert!(
        tmp.path()
            .join("plugin/dra-device-metadata/two/a/metadata.json")
            .exists()
    );
    backend.unprepare_fails.store(false, Ordering::SeqCst);
    let result = service
        .node_unprepare_resources(tonic::Request::new(NodeUnprepareResourcesRequest {
            claims: refs.clone(),
        }))
        .await
        .unwrap()
        .into_inner();
    assert!(!result.claims["one"].error.is_empty());
    assert!(result.claims["two"].error.is_empty());
    assert!(foreign.exists());
    fs::remove_file(foreign).unwrap();
    let result = service
        .node_unprepare_resources(tonic::Request::new(NodeUnprepareResourcesRequest {
            claims: refs,
        }))
        .await
        .unwrap()
        .into_inner();
    assert!(result.claims.values().all(|r| r.error.is_empty()));
}

#[cfg(feature = "resource-health")]
#[tokio::test]
async fn metadata_artifacts_do_not_add_health_monitors_or_hardware_preparations() {
    use std::sync::atomic::AtomicUsize;
    use std::sync::atomic::Ordering;

    use k8s_device_plugin_core::DraDeviceHealth;
    use k8s_device_plugin_core::ResourceHealthError;
    use k8s_device_plugin_core::ResourceHealthReport;
    use k8s_device_plugin_core::ResourceHealthStatus;
    use k8s_device_plugin_proto::dra::health::v1alpha1 as health;
    use k8s_device_plugin_proto::dra::v1;
    use k8s_device_plugin_test::dra_plugin::MockDraPluginClient;
    use k8s_device_plugin_test::kube_mock::mock_kube_client;
    use k8s_device_plugin_test::kube_mock::respond;
    #[derive(Debug, Default)]
    struct Backend {
        prepares: AtomicUsize,
        watches: AtomicUsize,
    }
    #[async_trait::async_trait]
    impl ClaimPreparer for Backend {
        async fn prepare(
            &self,
            claims: &[ResolvedClaim],
        ) -> BTreeMap<ClaimRef, Result<Vec<PreparedDevice>, PrepareError>> {
            self.prepares.fetch_add(1, Ordering::SeqCst);
            claims
                .iter()
                .map(|c| (c.claim.clone(), Ok(vec![prepared("a", "widget-0").device])))
                .collect()
        }
        async fn unprepare(&self, _: &ClaimRef) -> Result<(), PrepareError> {
            Ok(())
        }
    }
    #[async_trait::async_trait]
    impl ResourceHealthReporter for Backend {
        async fn watch_resource_health(
            &self,
            reports: tokio::sync::mpsc::Sender<ResourceHealthReport>,
        ) -> Result<(), ResourceHealthError> {
            self.watches.fetch_add(1, Ordering::SeqCst);
            reports
                .send(ResourceHealthReport {
                    devices: vec![DraDeviceHealth {
                        pool_name: "pool-a".into(),
                        device_name: "widget-0".into(),
                        health: ResourceHealthStatus::Healthy,
                        last_updated_time: 1_700_000_000,
                        health_check_timeout_seconds: 30,
                        message: String::new(),
                    }],
                })
                .await
                .map_err(|e| ResourceHealthError::MonitorFailed(e.to_string()))?;
            reports.closed().await;
            Ok(())
        }
    }
    let tmp = tempfile::tempdir().unwrap();
    let backend = Arc::new(Backend::default());
    let (client, mut handle) = mock_kube_client();
    let service = DraPluginService::new(ClaimResolver::new(client), Arc::clone(&backend))
        .with_resource_health(Arc::clone(&backend))
        .with_device_metadata(
            "dra.example.com",
            DeviceMetadataOptions::v1beta1()
                .with_plugin_directories(tmp.path().join("plugin"), tmp.path().join("plugin"))
                .with_cdi_directory(tmp.path().join("cdi")),
        )
        .unwrap();
    let socket = tmp.path().join("plugin.sock");
    let server = service.spawn_at(&socket).await.unwrap();
    let path = socket.clone();
    let channel = transport::Endpoint::try_from("http://[::]:50051")
        .unwrap()
        .connect_with_connector(tower::service_fn(move |_| {
            let path = path.clone();
            async move {
                net::UnixStream::connect(path)
                    .await
                    .map(hyper_util::rt::TokioIo::new)
            }
        }))
        .await
        .unwrap();
    let mut client = health::DraResourceHealthClient::new(channel);
    let mut stream = client
        .node_watch_resources(health::NodeWatchResourcesRequest {})
        .await
        .unwrap()
        .into_inner();
    let snapshot = stream.message().await.unwrap().unwrap();
    assert_eq!(snapshot.devices.len(), 1);
    let responder = tokio::spawn(async move {
        for _ in 0..2 {
            respond(&mut handle, 200, &json::to_value(claim()).unwrap()).await;
        }
    });
    let mut client = MockDraPluginClient::connect(&socket).await.unwrap();
    for _ in 0..2 {
        let result = client
            .prepare_resources(vec![v1::Claim {
                namespace: "default".into(),
                name: "test-claim".into(),
                uid: "claim-uid".into(),
            }])
            .await
            .unwrap();
        assert_eq!(result.claims["claim-uid"].devices.len(), 2);
        assert!(result.claims["claim-uid"].error.is_empty());
    }
    responder.await.unwrap();
    assert_eq!(backend.watches.load(Ordering::SeqCst), 1);
    assert_eq!(backend.prepares.load(Ordering::SeqCst), 2);
    drop(stream);
    server.abort();
    let _ = server.await;
}

#[test]
fn broad_physical_entry_shared_by_requests_exposes_only_each_requests_metadata_id() {
    let tmp = tempfile::tempdir().unwrap();
    let runtime = new_runtime(tmp.path());
    let mut object = claim();
    object
        .status
        .as_mut()
        .unwrap()
        .allocation
        .as_mut()
        .unwrap()
        .devices
        .as_mut()
        .unwrap()
        .results
        .as_mut()
        .unwrap()[1]
        .device = "widget-0".into();
    let mut physical = prepared("a", "widget-0");
    physical.device.request_names.clear();
    let plan = Plan::build(&runtime, &object, vec![physical]).unwrap();
    assert_eq!(plan.documents.len(), 2);
    assert_eq!(plan.devices.len(), 3);
    for request in ["a", "b"] {
        let selected = plan
            .devices
            .iter()
            .filter(|d| d.request_names.is_empty() || d.request_names.iter().any(|r| r == request))
            .flat_map(|d| d.cdi_device_ids.iter())
            .collect::<Vec<_>>();
        assert_eq!(selected.len(), 2);
        assert_eq!(selected[0], "dra.example.com/widget=widget-0");
        assert!(selected[1].ends_with(&format!("_{request}")));
    }
    let all = plan
        .devices
        .iter()
        .flat_map(|d| d.cdi_device_ids.iter())
        .collect::<Vec<_>>();
    assert_eq!(all.iter().filter(|id| id.contains("/widget=")).count(), 1);
}

#[test]
fn corrupt_journal_refuses_cleanup_and_keeps_owned_data_until_repaired() {
    let tmp = tempfile::tempdir().unwrap();
    let runtime = new_runtime(tmp.path());
    let plan = plan(&runtime);
    store::persist(&runtime, &plan).unwrap();
    let journal = tmp
        .path()
        .join("plugin/dra-device-metadata/claim-uid/journal.json");
    let original = fs::read(&journal).unwrap();
    fs::write(&journal, "corrupt").unwrap();
    assert!(store::cleanup(&runtime, &plan.claim).is_err());
    assert!(data(tmp.path(), "a").exists());
    assert!(
        tmp.path()
            .join("cdi/dra.example.com_metadata_claim-uid_a.json")
            .exists()
    );
    let mut future = json::from_slice::<json::Value>(&original).unwrap();
    future["journalVersion"] = json::json!(2);
    fs::write(&journal, json::to_vec(&future).unwrap()).unwrap();
    assert!(store::cleanup(&runtime, &plan.claim).is_err());
    assert!(data(tmp.path(), "a").exists());
    fs::write(journal, original).unwrap();
    store::cleanup(&runtime, &plan.claim).unwrap();
}

#[test]
fn zero_device_allocation_has_no_container_edits_and_cleans_its_journal() {
    let tmp = tempfile::tempdir().unwrap();
    let runtime = new_runtime(tmp.path());
    let mut object = claim();
    object.spec.devices = None;
    object
        .status
        .as_mut()
        .unwrap()
        .allocation
        .as_mut()
        .unwrap()
        .devices = None;
    // ClaimResolver has already checked allocation presence and UID. Optional
    // device/results fields have the same empty-list meaning as its projection.
    let plan = Plan::build(&runtime, &object, Vec::new()).unwrap();
    assert!(plan.devices.is_empty());
    assert!(plan.documents.is_empty());
    assert!(plan.specs.is_empty());
    store::persist(&runtime, &plan).unwrap();
    assert!(
        fs::read_dir(tmp.path().join("cdi"))
            .unwrap()
            .next()
            .is_none()
    );
    store::cleanup(&runtime, &plan.claim).unwrap();
    assert!(
        !tmp.path()
            .join("plugin/dra-device-metadata/claim-uid")
            .exists()
    );
}
