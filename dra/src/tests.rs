use std::future;

use camino_tempfile::Utf8TempDir;
use kube::client;
use tokio::sync::oneshot;
use tokio::time;

use k8s_device_plugin_test::dra_plugin::MockDraPluginClient;
use k8s_device_plugin_test::dra_registration::MockRegistrationClient;
use k8s_device_plugin_test::kube_mock::allocated_status;
use k8s_device_plugin_test::kube_mock::mock_kube_client;
use k8s_device_plugin_test::kube_mock::node;
use k8s_device_plugin_test::kube_mock::resource_claim_json;
use k8s_device_plugin_test::kube_mock::respond;

use super::*;

struct StaticDraDriver {
    name: DriverName,
    pool: PoolName,
    devices: Vec<PoolDevice>,
}

#[tonic::async_trait]
impl ResourcePool for StaticDraDriver {
    async fn devices(&self) -> BTreeMap<PoolName, Vec<PoolDevice>> {
        BTreeMap::from([(self.pool.clone(), self.devices.clone())])
    }
}

#[tonic::async_trait]
impl ClaimPreparer for StaticDraDriver {
    async fn prepare(
        &self,
        claims: &[ResolvedClaim],
    ) -> BTreeMap<ClaimRef, Result<Vec<PreparedDevice>, PrepareError>> {
        claims
            .iter()
            .map(|resolved| {
                let prepared = resolved
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
                    .collect();
                (resolved.claim.clone(), Ok(prepared))
            })
            .collect()
    }

    async fn unprepare(&self, _claim: &ClaimRef) -> Result<(), PrepareError> {
        Ok(())
    }
}

impl DraDriver for StaticDraDriver {
    fn driver_name(&self) -> &DriverName {
        &self.name
    }
}

struct NotifyOnDrop(Option<oneshot::Sender<()>>);

impl Drop for NotifyOnDrop {
    fn drop(&mut self) {
        if let Some(sender) = self.0.take() {
            let _ = sender.send(());
        }
    }
}

fn wire_claim(name: &str, uid: &str) -> dra::v1::Claim {
    dra::v1::Claim {
        namespace: "default".to_string(),
        uid: uid.to_string(),
        name: name.to_string(),
    }
}

fn tuning_plugin(client: kube::Client) -> DraPlugin {
    DraPlugin::new(
        client,
        "example.com".parse().unwrap(),
        "node-0".parse().unwrap(),
        StaticDraDriver {
            name: "example.com".parse().unwrap(),
            pool: "pool-0".parse().unwrap(),
            devices: vec![],
        },
    )
    .unwrap()
}

#[tokio::test(start_paused = true)]
async fn plugin_claim_concurrency_bounds_each_batch() {
    use dra::v1::DraPlugin as _;
    for (configured, limit) in [(None, 16), (Some(0), 1), (Some(2), 2)] {
        let (client, mut handle) = mock_kube_client();
        let mut plugin = tuning_plugin(client);
        if let Some(value) = configured {
            plugin = plugin
                .with_max_concurrent_resolves(9)
                .with_max_concurrent_resolves(value);
        }
        let claims = (0..=limit)
            .map(|i| wire_claim(&format!("claim-{i}"), &format!("uid-{i}")))
            .collect();
        let request = tonic::Request::new(dra::v1::NodePrepareResourcesRequest { claims });
        let task = tokio::spawn(async move {
            plugin
                .service
                .node_prepare_resources(request)
                .await
                .unwrap()
                .into_inner()
        });
        let mut pending = Vec::new();
        for _ in 0..limit {
            pending.push(handle.next_request().await.unwrap().1);
        }
        assert!(
            time::timeout(Duration::from_millis(1), handle.next_request())
                .await
                .is_err()
        );
        let not_found =
            json::to_vec(&kube::core::Status::failure("missing", "NotFound").with_code(404))
                .unwrap();
        for send in pending {
            send.send_response(
                http::Response::builder()
                    .status(404)
                    .body(client::Body::from(not_found.clone()))
                    .unwrap(),
            );
        }
        let (_, send) = handle.next_request().await.unwrap();
        send.send_response(
            http::Response::builder()
                .status(404)
                .body(client::Body::from(not_found))
                .unwrap(),
        );
        assert_eq!(task.await.unwrap().claims.len(), limit + 1);
    }
}

#[tokio::test(start_paused = true)]
async fn plugin_claim_limit_is_per_rpc_not_global() {
    use dra::v1::DraPlugin as _;
    let (client, mut handle) = mock_kube_client();
    let service = Arc::new(
        tuning_plugin(client)
            .with_max_concurrent_resolves(1)
            .service,
    );
    let mut tasks = Vec::new();
    for i in 0..2 {
        let service = Arc::clone(&service);
        tasks.push(tokio::spawn(async move {
            service
                .node_prepare_resources(tonic::Request::new(dra::v1::NodePrepareResourcesRequest {
                    claims: vec![wire_claim(&format!("claim-{i}"), &format!("uid-{i}"))],
                }))
                .await
                .unwrap()
        }));
    }
    let (_, first) = handle.next_request().await.unwrap();
    let (_, second) = time::timeout(Duration::from_millis(1), handle.next_request())
        .await
        .expect("both RPCs may read concurrently")
        .unwrap();
    let body =
        json::to_vec(&kube::core::Status::failure("missing", "NotFound").with_code(404)).unwrap();
    for send in [first, second] {
        send.send_response(
            http::Response::builder()
                .status(404)
                .body(client::Body::from(body.clone()))
                .unwrap(),
        );
    }
    for task in tasks {
        task.await.unwrap();
    }
}

#[tokio::test(start_paused = true)]
async fn plugin_claim_retry_attempts_and_delay_are_applied() {
    use dra::v1::DraPlugin as _;
    for (configured, attempts, delay) in [
        (None, 3, Duration::from_millis(50)),
        (Some((0, Duration::ZERO)), 1, Duration::ZERO),
        (
            Some((2, Duration::from_millis(7))),
            2,
            Duration::from_millis(7),
        ),
        (Some((2, Duration::ZERO)), 2, Duration::ZERO),
    ] {
        let (client, mut handle) = mock_kube_client();
        let mut plugin = tuning_plugin(client);
        if let Some((count, wait)) = configured {
            plugin = plugin
                .with_claim_retry_policy(8, Duration::from_secs(9))
                .with_claim_retry_policy(count, wait);
        }
        let task = tokio::spawn(async move {
            plugin
                .service
                .node_prepare_resources(tonic::Request::new(dra::v1::NodePrepareResourcesRequest {
                    claims: vec![wire_claim("claim", "uid")],
                }))
                .await
                .unwrap()
                .into_inner()
        });
        let mut previous = time::Instant::now();
        for index in 0..attempts {
            let (_, send) = handle.next_request().await.unwrap();
            let now = time::Instant::now();
            if index > 0 {
                assert_eq!(now - previous, delay);
            }
            previous = now;
            send.send_response(
                http::Response::builder()
                    .status(500)
                    .body(client::Body::from(
                        json::to_vec(
                            &kube::core::Status::failure("transient", "InternalError")
                                .with_code(500),
                        )
                        .unwrap(),
                    ))
                    .unwrap(),
            );
        }
        let response = task.await.unwrap();
        assert!(
            response.claims["uid"]
                .error
                .contains(&format!("{attempts} attempt(s)"))
        );
        assert!(matches!(
            time::timeout(Duration::from_millis(1), handle.next_request()).await,
            Err(_) | Ok(None)
        ));
    }
}

#[tokio::test(start_paused = true)]
async fn plugin_publisher_failure_backoff_is_bounded_and_resets() {
    for (configured, delays) in [
        (
            None,
            [
                Duration::from_secs(1),
                Duration::from_secs(2),
                Duration::from_secs(4),
            ],
        ),
        (
            Some((Duration::from_secs(3), Duration::from_secs(5))),
            [
                Duration::from_secs(3),
                Duration::from_secs(5),
                Duration::from_secs(5),
            ],
        ),
        (
            Some((Duration::ZERO, Duration::ZERO)),
            [Duration::from_millis(1); 3],
        ),
        (
            Some((Duration::from_secs(3), Duration::from_secs(1))),
            [Duration::from_secs(3); 3],
        ),
    ] {
        let (client, mut handle) = mock_kube_client();
        let mut plugin = tuning_plugin(client).with_poll_interval(Duration::from_secs(7));
        if let Some((base, max)) = configured {
            plugin = plugin
                .with_publisher_retry_backoff(Duration::from_secs(9), Duration::from_secs(20))
                .with_publisher_retry_backoff(base, max);
        }
        let publisher = plugin.publisher.spawn();
        let mut previous = time::Instant::now();
        for index in 0..4 {
            let (request, send) = handle.next_request().await.unwrap();
            assert_eq!(request.uri().path(), "/api/v1/nodes/node-0");
            let now = time::Instant::now();
            if index > 0 {
                assert_eq!(now - previous, delays[index - 1]);
            }
            previous = now;
            if index < 3 {
                send.send_response(
                    http::Response::builder()
                        .status(500)
                        .body(client::Body::from(
                            json::to_vec(
                                &kube::core::Status::failure("transient", "InternalError")
                                    .with_code(500),
                            )
                            .unwrap(),
                        ))
                        .unwrap(),
                );
            } else {
                send.send_response(http::Response::new(client::Body::from(
                    json::to_vec(&node("node-0", "node-uid-0")).unwrap(),
                )));
            }
        }
        respond(
            &mut handle,
            200,
            &k8s_openapi::List::<resourcev1::ResourceSlice>::default(),
        )
        .await;
        let (_, send) = handle.next_request().await.unwrap();
        assert_eq!(time::Instant::now() - previous, Duration::from_secs(7));
        let failed_at = time::Instant::now();
        send.send_response(
            http::Response::builder()
                .status(500)
                .body(client::Body::from(
                    json::to_vec(
                        &kube::core::Status::failure("transient", "InternalError").with_code(500),
                    )
                    .unwrap(),
                ))
                .unwrap(),
        );
        let (_, send) = handle.next_request().await.unwrap();
        assert_eq!(time::Instant::now() - failed_at, delays[0]);
        drop(send);
        publisher.abort();
        assert!(publisher.await.unwrap_err().is_cancelled());
    }
}

#[tokio::test]
async fn run_spawns_all_three_components_and_they_work() {
    let (client, mut kube_handle) = mock_kube_client();

    let registration_dir = Utf8TempDir::new().expect("create temp dir for registration socket");
    let registration_socket_path = registration_dir.path().join("plugin-reg.sock");
    let plugin_dir = Utf8TempDir::new().expect("create temp dir for plugin socket");
    let plugin_socket_path = plugin_dir.path().join("plugin.sock");

    let driver = StaticDraDriver {
        name: "example.com".parse().unwrap(),
        pool: "pool-0".parse().unwrap(),
        devices: vec![PoolDevice::new("widget-0".parse().unwrap())],
    };
    let plugin = DraPlugin::for_test(
        client,
        "example.com".parse().unwrap(),
        "node-0".parse().unwrap(),
        driver,
        registration_socket_path.clone(),
    );

    let run_plugin_socket = plugin_socket_path.clone();
    let run_handle = tokio::spawn(async move { plugin.run_at(&run_plugin_socket).await });

    // The publisher's default 30s poll interval means exactly one publish
    // cycle (Node get, list slices, create) happens during this test -- drain
    // it before triggering any other kube traffic, so ordering stays
    // deterministic instead of racing against the resolver's later request.
    respond(&mut kube_handle, 200, &node("node-0", "node-uid-0")).await;
    respond(
        &mut kube_handle,
        200,
        &k8s_openapi::List::<resourcev1::ResourceSlice>::default(),
    )
    .await;
    let (create_request, send) = kube_handle
        .next_request()
        .await
        .expect("create request sent");
    assert_eq!(create_request.method(), http::Method::POST);
    send.send_response(http::Response::new(create_request.into_body()));

    // Registration server came up: GetInfo answers correctly.
    let mut registration_client = MockRegistrationClient::connect(&registration_socket_path)
        .await
        .expect("connect to registration socket");
    let info = registration_client.get_info().await.expect("get_info call");
    assert_eq!(info.r#type, "DRAPlugin");
    assert_eq!(info.name, "example.com");
    assert_eq!(
        info.supported_versions,
        vec![dra::DRA_PLUGIN_SERVICE.to_string()]
    );

    // An ordinary plugin serves the health endpoint but explicitly declines
    // watches and never starts the backend reporter.
    let health_path = plugin_socket_path.clone();
    let channel = transport::Endpoint::from_static("http://[::]:50051")
        .connect_with_connector(tower::service_fn(move |_| {
            let path = health_path.clone();
            async move {
                net::UnixStream::connect(path)
                    .await
                    .map(hyper_util::rt::TokioIo::new)
            }
        }))
        .await
        .expect("connect to plugin health endpoint");
    let mut stable = dra::health::v1::DraResourceHealthClient::new(channel.clone());
    assert_eq!(
        stable
            .node_watch_resources(dra::health::v1::NodeWatchResourcesRequest {})
            .await
            .expect_err("ordinary plugin declines stable health watches")
            .code(),
        tonic::Code::Unimplemented
    );
    let mut health = dra::health::v1alpha1::DraResourceHealthClient::new(channel);
    assert_eq!(
        health
            .node_watch_resources(dra::health::v1alpha1::NodeWatchResourcesRequest {})
            .await
            .expect_err("ordinary plugin declines health watches")
            .code(),
        tonic::Code::Unimplemented
    );

    // DRAPlugin gRPC service came up and can resolve+prepare a claim
    // through to a real response, driving the resolver's kube request
    // concurrently with the client call.
    let mut plugin_client = MockDraPluginClient::connect(&plugin_socket_path)
        .await
        .expect("connect to plugin socket");

    let responder = tokio::spawn(async move {
        let (request, send) = kube_handle
            .next_request()
            .await
            .expect("resolve request sent");
        assert_eq!(request.method(), http::Method::GET);
        let body = resource_claim_json(
            "my-claim",
            "default",
            "uid-0",
            allocated_status("pool-0", "widget-0", "req-0"),
        );
        let bytes = json::to_vec(&body).unwrap();
        send.send_response(http::Response::new(client::Body::from(bytes)));
    });

    let response = plugin_client
        .prepare_resources(vec![wire_claim("my-claim", "uid-0")])
        .await
        .expect("prepare_resources call");
    responder.await.expect("kube responder completes");

    let entry = response.claims.get("uid-0").expect("entry for uid-0");
    assert!(entry.error.is_empty());
    assert_eq!(entry.devices[0].pool_name, "pool-0");
    assert_eq!(entry.devices[0].device_name, "widget-0");

    run_handle.abort();
}

#[tokio::test(start_paused = true)]
async fn plugin_poll_interval_controls_successful_reconciliation() {
    for (configured, expected) in [
        (None, Duration::from_secs(30)),
        (Some(Duration::from_secs(5)), Duration::from_secs(5)),
        (Some(Duration::ZERO), Duration::from_millis(1)),
        (Some(Duration::from_micros(500)), Duration::from_millis(1)),
    ] {
        let (client, mut kube_handle) = mock_kube_client();
        let mut plugin = DraPlugin::new(
            client,
            "example.com".parse().unwrap(),
            "node-0".parse().unwrap(),
            StaticDraDriver {
                name: "example.com".parse().unwrap(),
                pool: "pool-0".parse().unwrap(),
                devices: vec![],
            },
        )
        .unwrap();
        if let Some(interval) = configured {
            plugin = plugin
                .with_poll_interval(Duration::from_secs(60))
                .with_poll_interval(interval);
        }

        // Run the publisher owned by the configured plugin. Empty inventory
        // needs only a node lookup and a slice list per successful cycle.
        let started = time::Instant::now();
        let publisher = plugin.publisher.spawn();
        respond(&mut kube_handle, 200, &node("node-0", "node-uid-0")).await;
        respond(
            &mut kube_handle,
            200,
            &k8s_openapi::List::<resourcev1::ResourceSlice>::default(),
        )
        .await;

        assert!(
            time::timeout(
                expected - Duration::from_millis(1),
                kube_handle.next_request(),
            )
            .await
            .is_err(),
            "publisher must not reconcile before its configured interval"
        );
        let (request, send) = time::timeout(Duration::from_millis(2), kube_handle.next_request())
            .await
            .expect("publisher reconciles at the configured interval")
            .expect("node lookup sent");
        assert_eq!(time::Instant::now() - started, expected);
        assert_eq!(request.method(), http::Method::GET);
        assert_eq!(request.uri().path(), "/api/v1/nodes/node-0");
        send.send_response(http::Response::new(client::Body::from(
            json::to_vec(&node("node-0", "node-uid-0")).unwrap(),
        )));
        respond(
            &mut kube_handle,
            200,
            &k8s_openapi::List::<resourcev1::ResourceSlice>::default(),
        )
        .await;
        publisher.abort();
        assert!(publisher.await.unwrap_err().is_cancelled());
    }
}

#[tokio::test]
async fn liveness_probe_rejects_other_plugin_types_with_plain_string_diagnostics() {
    use k8s_device_plugin_test::dra_registration::start_mock_registration_server;

    let registration = start_mock_registration_server(dra::registration::PluginInfo {
        r#type: "DevicePlugin".into(),
        ..default()
    });
    let registration_path = registration.socket_path();
    let plugin_dir = Utf8TempDir::new().expect("create temp dir for plugin socket");
    let error = DraPluginLivenessProbe::with_socket_paths(
        &"example.com".parse().unwrap(),
        registration_path,
        plugin_dir.path().join("missing.sock"),
    )
    .check()
    .await
    .expect_err("a classic device plugin must not pass the DRA probe");

    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert_eq!(
        error.to_string(),
        format!(
            "registration socket {registration_path} reported plugin type DevicePlugin, not DRAPlugin"
        )
    );
    registration.shutdown();
}

#[tokio::test]
async fn liveness_probe_checks_both_dra_sockets_with_real_rpcs() {
    let (client, _kube_handle) = mock_kube_client();
    let registration_dir = Utf8TempDir::new().expect("create temp dir for registration socket");
    let registration_socket_path = registration_dir.path().join("plugin-reg.sock");
    let plugin_dir = Utf8TempDir::new().expect("create temp dir for plugin socket");
    let plugin_socket_path = plugin_dir.path().join("plugin.sock");
    let driver = StaticDraDriver {
        name: "example.com".parse().unwrap(),
        pool: "pool-0".parse().unwrap(),
        devices: vec![PoolDevice::new("widget-0".parse().unwrap())],
    };

    let registration = DraRegistrationServer::for_test(
        &"example.com".parse().unwrap(),
        &plugin_socket_path,
        registration_socket_path.clone(),
    );
    let registration_handle = registration
        .spawn()
        .await
        .expect("spawn registration server");
    let service = DraPluginService::new(
        ClaimResolver::new(client, "example.com".parse().unwrap()),
        driver,
    );
    let service_handle = service
        .spawn_at(&plugin_socket_path)
        .await
        .expect("spawn DRAPlugin service");

    DraPluginLivenessProbe::with_socket_paths(
        &"example.com".parse().unwrap(),
        registration_socket_path,
        plugin_socket_path,
    )
    .check()
    .await
    .expect("both DRA sockets answer the liveness probe");

    registration_handle.abort();
    service_handle.abort();
}

#[tokio::test]
async fn run_at_stops_registration_when_service_startup_fails() {
    let (client, _kube_handle) = mock_kube_client();
    let registration_dir = Utf8TempDir::new().expect("create temp dir for registration socket");
    let registration_socket_path = registration_dir.path().join("plugin-reg.sock");
    let plugin_dir = Utf8TempDir::new().expect("create temp dir for plugin socket");
    let plugin_socket_path = plugin_dir.path().join("missing").join("plugin.sock");

    let plugin = DraPlugin::for_test(
        client,
        "example.com".parse().unwrap(),
        "node-0".parse().unwrap(),
        StaticDraDriver {
            name: "example.com".parse().unwrap(),
            pool: "pool-0".parse().unwrap(),
            devices: vec![PoolDevice::new("widget-0".parse().unwrap())],
        },
        registration_socket_path.clone(),
    );

    let err = plugin
        .run_at(&plugin_socket_path)
        .await
        .expect_err("service bind should fail when its parent directory is absent");
    assert_eq!(err.kind(), io::ErrorKind::NotFound);
    assert!(
        net::UnixStream::connect(&registration_socket_path)
            .await
            .is_err(),
        "registration server must not survive a service startup failure"
    );
}

#[tokio::test]
async fn wait_for_component_exit_aborts_and_reaps_surviving_tasks() {
    let (registration_started_tx, registration_started) = oneshot::channel();
    let (registration_stopped_tx, registration_stopped) = oneshot::channel();
    let registration_handle = tokio::spawn(async move {
        let _on_drop = NotifyOnDrop(Some(registration_stopped_tx));
        let _ = registration_started_tx.send(());
        future::pending::<Result<(), transport::Error>>().await
    });

    let (publisher_started_tx, publisher_started) = oneshot::channel();
    let (publisher_stopped_tx, publisher_stopped) = oneshot::channel();
    let publisher_handle = tokio::spawn(async move {
        let _on_drop = NotifyOnDrop(Some(publisher_stopped_tx));
        let _ = publisher_started_tx.send(());
        future::pending::<()>().await
    });

    registration_started
        .await
        .expect("registration task starts");
    publisher_started.await.expect("publisher task starts");
    let service_handle = tokio::spawn(async { Ok::<(), transport::Error>(()) });

    let err = wait_for_component_exit(registration_handle, service_handle, publisher_handle)
        .await
        .expect_err("an exited service is an error");
    assert!(
        err.to_string()
            .contains("DRAPlugin service exited unexpectedly")
    );
    time::timeout(Duration::from_secs(1), registration_stopped)
        .await
        .expect("registration task is reaped")
        .expect("registration drop notifier runs");
    time::timeout(Duration::from_secs(1), publisher_stopped)
        .await
        .expect("publisher task is reaped")
        .expect("publisher drop notifier runs");
}

#[test]
fn component_error_reports_unexpected_exit() {
    let err = component_error("registration server", Ok(Ok(())));
    assert!(
        err.to_string()
            .contains("registration server exited unexpectedly")
    );
}

#[tokio::test]
async fn component_error_reports_panic() {
    let join_err = tokio::spawn(async { panic!("boom") }).await.unwrap_err();
    let err = component_error("DRAPlugin service", Err(join_err));
    assert!(err.to_string().contains("DRAPlugin service panicked"));
}

#[test]
fn publisher_error_reports_unexpected_exit() {
    let err = publisher_error(Ok(()));
    assert!(
        err.to_string()
            .contains("ResourceSlice publisher exited unexpectedly")
    );
}

#[tokio::test]
async fn publisher_error_reports_panic() {
    let join_err = tokio::spawn(async { panic!("boom") }).await.unwrap_err();
    let err = publisher_error(Err(join_err));
    assert!(err.to_string().contains("ResourceSlice publisher panicked"));
}

#[tokio::test]
async fn dra_constructors_reject_invalid_or_mismatched_driver_identity() {
    let (client, _) = mock_kube_client();
    for name in ["", "..", "example.com/widget", "UPPER.example"] {
        assert!(DriverName::try_from(name).is_err());
    }
    assert!(NodeName::try_from("../node").is_err());
    let driver = StaticDraDriver {
        name: "example.com".parse().unwrap(),
        pool: "pool".parse().unwrap(),
        devices: vec![],
    };
    assert!(matches!(
        DraPlugin::new(
            client,
            "other.example".parse().unwrap(),
            "node".parse().unwrap(),
            driver
        ),
        Err(DriverIdentityMismatch { .. })
    ));
}

#[tokio::test]
async fn registration_failure_aborts_the_already_bound_plugin_service() {
    let (client, _) = mock_kube_client();
    let directory = Utf8TempDir::new().unwrap();
    let registration_path = directory.path().join("missing/registration.sock");
    let plugin_path = directory.path().join("plugin.sock");
    let plugin = DraPlugin::for_test(
        client,
        "example.com".parse().unwrap(),
        "node".parse().unwrap(),
        StaticDraDriver {
            name: "example.com".parse().unwrap(),
            pool: "pool".parse().unwrap(),
            devices: vec![],
        },
        registration_path,
    );
    let error = plugin.run_at(&plugin_path).await.unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::NotFound);
    assert!(
        plugin_path.exists(),
        "service must be bound before registration is attempted"
    );
    assert!(
        net::UnixStream::connect(&plugin_path).await.is_err(),
        "failed startup must reap the service"
    );
}

#[tokio::test]
async fn liveness_rejects_wrong_driver_or_missing_dra_service() {
    use k8s_device_plugin_test::dra_registration::start_mock_registration_server;
    for (name, supported_versions) in [
        ("foreign.example", vec![dra::DRA_PLUGIN_SERVICE.to_string()]),
        ("example.com", vec![]),
    ] {
        let registration = start_mock_registration_server(dra::registration::PluginInfo {
            r#type: "DRAPlugin".into(),
            name: name.into(),
            endpoint: "/missing/plugin.sock".into(),
            supported_versions,
        });
        let error = DraPluginLivenessProbe::with_socket_paths(
            &"example.com".parse().unwrap(),
            registration.socket_path(),
            "/missing/plugin.sock",
        )
        .check()
        .await
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(
            error
                .to_string()
                .contains("must advertise driver example.com and service v1.DRAPlugin")
        );
        registration.shutdown();
    }
}

#[tonic::async_trait]
impl ResourceHealthReporter for StaticDraDriver {
    async fn watch_resource_health(
        &self,
        _: tokio::sync::mpsc::Sender<k8s_device_plugin_dra_core::ResourceHealthReport>,
    ) -> Result<(), k8s_device_plugin_dra_core::ResourceHealthError> {
        Ok(())
    }
}

#[tokio::test]
async fn opted_in_health_is_advertised_by_the_plugin_registration_rpc() {
    let (client, _) = mock_kube_client();
    let directory = Utf8TempDir::new().unwrap();
    let mut plugin = DraPlugin::with_resource_health(
        client,
        "example.com".parse().unwrap(),
        "node".parse().unwrap(),
        StaticDraDriver {
            name: "example.com".parse().unwrap(),
            pool: "pool".parse().unwrap(),
            devices: vec![],
        },
    )
    .unwrap();
    plugin
        .registration
        .set_socket_path(directory.path().join("registration.sock"));
    let handle = plugin.registration.spawn().await.unwrap();
    let mut client = MockRegistrationClient::connect(plugin.registration.socket_path())
        .await
        .unwrap();
    let info = client.get_info().await.unwrap();
    assert_eq!(
        info.supported_versions,
        [
            "v1.DRAPlugin",
            "v1.DRAResourceHealth",
            "v1alpha1.DRAResourceHealth"
        ]
    );
    handle.abort();
}

#[tokio::test]
async fn tuning_composes_with_health_and_metadata() {
    use dra::v1::DraPlugin as _;
    let (client, mut handle) = mock_kube_client();
    let directory = Utf8TempDir::new().unwrap();
    let mut plugin = DraPlugin::with_resource_health(
        client,
        "example.com".parse().unwrap(),
        "node".parse().unwrap(),
        StaticDraDriver {
            name: "example.com".parse().unwrap(),
            pool: "pool".parse().unwrap(),
            devices: vec![],
        },
    )
    .unwrap()
    .with_max_concurrent_resolves(2)
    .with_device_metadata(
        DeviceMetadataOptions::v1beta1()
            .with_plugin_directories(directory.path().join("plugin"), "/node/plugin")
            .with_cdi_directory(directory.path().join("cdi")),
    )
    .unwrap()
    .with_claim_retry_policy(1, Duration::ZERO)
    .with_poll_interval(Duration::from_secs(2))
    .with_publisher_retry_backoff(Duration::from_millis(2), Duration::from_millis(5));
    plugin
        .registration
        .set_socket_path(directory.path().join("registration.sock"));
    let registration = plugin.registration.spawn().await.unwrap();
    let mut reg = MockRegistrationClient::connect(plugin.registration.socket_path())
        .await
        .unwrap();
    assert_eq!(
        reg.get_info().await.unwrap().supported_versions,
        [
            "v1.DRAPlugin",
            "v1.DRAResourceHealth",
            "v1alpha1.DRAResourceHealth"
        ]
    );
    let claim = k8s_device_plugin_test::kube_mock::resource_claim(
        "claim",
        "default",
        "uid",
        Some(resourcev1::ResourceClaimStatus {
            allocation: Some(resourcev1::AllocationResult::default()),
            ..default()
        }),
    );
    let server = tokio::spawn(async move {
        respond(&mut handle, 200, &claim).await;
    });
    let response = plugin
        .service
        .node_prepare_resources(tonic::Request::new(dra::v1::NodePrepareResourcesRequest {
            claims: vec![wire_claim("claim", "uid")],
        }))
        .await
        .unwrap()
        .into_inner();
    server.await.unwrap();
    assert_eq!(response.claims["uid"].error, "");
    assert!(
        directory
            .path()
            .join("plugin/dra-device-metadata/uid/journal.json")
            .exists()
    );
    let socket = directory.path().join("plugin.sock");
    let service = plugin.service.spawn_at(&socket).await.unwrap();
    let channel = transport::Endpoint::from_static("http://[::]:50051")
        .connect_with_connector(tower::service_fn(move |_| {
            let path = socket.clone();
            async move {
                net::UnixStream::connect(path)
                    .await
                    .map(hyper_util::rt::TokioIo::new)
            }
        }))
        .await
        .unwrap();
    let mut stable = dra::health::v1::DraResourceHealthClient::new(channel.clone());
    stable
        .node_watch_resources(dra::health::v1::NodeWatchResourcesRequest {})
        .await
        .expect("stable health remains enabled");
    let mut health = dra::health::v1alpha1::DraResourceHealthClient::new(channel);
    health
        .node_watch_resources(dra::health::v1alpha1::NodeWatchResourcesRequest {})
        .await
        .expect("health remains enabled");
    registration.abort();
    service.abort();
    assert!(registration.await.unwrap_err().is_cancelled());
    assert!(service.await.unwrap_err().is_cancelled());
}
