use std::collections::HashMap;
use std::future;
use std::io;
use std::time::Duration;

use k8s_device_plugin_core::ClaimPreparer;
use k8s_device_plugin_core::ClaimRef;
use k8s_device_plugin_core::PoolDevice;
use k8s_device_plugin_core::PrepareError;
use k8s_device_plugin_core::PreparedDevice;
use k8s_device_plugin_core::ResolvedClaim;
use k8s_device_plugin_core::ResourcePool;
use k8s_device_plugin_proto::dra::DRA_PLUGIN_SERVICE;
use k8s_device_plugin_proto::dra::v1;
use k8s_device_plugin_test::dra_plugin::MockDraPluginClient;
use k8s_device_plugin_test::dra_registration::MockRegistrationClient;
use k8s_device_plugin_test::kube_mock::allocated_status;
use k8s_device_plugin_test::kube_mock::mock_kube_client;
use k8s_device_plugin_test::kube_mock::node_json;
use k8s_device_plugin_test::kube_mock::not_found_json;
use k8s_device_plugin_test::kube_mock::resource_claim_json;
use k8s_device_plugin_test::kube_mock::respond;
use kube::client::Body;
use serde_json::json;
use tempfile::TempDir;
use tokio::sync::oneshot;

use super::*;

struct StaticDraDriver {
    name: String,
    pool: String,
    devices: Vec<PoolDevice>,
}

#[tonic::async_trait]
impl ResourcePool for StaticDraDriver {
    async fn devices(&self) -> HashMap<String, Vec<PoolDevice>> {
        HashMap::from([(self.pool.clone(), self.devices.clone())])
    }
}

#[tonic::async_trait]
impl ClaimPreparer for StaticDraDriver {
    async fn prepare(
        &self,
        claims: &[ResolvedClaim],
    ) -> HashMap<ClaimRef, Result<Vec<PreparedDevice>, PrepareError>> {
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
    fn driver_name(&self) -> &str {
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

fn wire_claim(name: &str, uid: &str) -> v1::Claim {
    v1::Claim {
        namespace: "default".to_string(),
        uid: uid.to_string(),
        name: name.to_string(),
    }
}

#[tokio::test]
async fn run_spawns_all_three_components_and_they_work() {
    let (client, mut kube_handle) = mock_kube_client();

    let registration_dir = TempDir::new().expect("create temp dir for registration socket");
    let registration_socket_path = registration_dir.path().join("plugin-reg.sock");
    let plugin_dir = TempDir::new().expect("create temp dir for plugin socket");
    let plugin_socket_path = plugin_dir.path().join("plugin.sock");

    let driver = StaticDraDriver {
        name: "example.com".to_string(),
        pool: "pool-0".to_string(),
        devices: vec![PoolDevice::new("widget-0")],
    };
    let plugin = DraPlugin::for_test(
        client,
        "example.com",
        "node-0",
        driver,
        registration_socket_path.clone(),
    );

    let run_plugin_socket = plugin_socket_path.clone();
    let run_handle = tokio::spawn(async move { plugin.run_at(&run_plugin_socket).await });

    // The publisher's default 30s poll interval means exactly one publish
    // cycle (Node get, slice get, create) happens during this test -- drain
    // it before triggering any other kube traffic, so ordering stays
    // deterministic instead of racing against the resolver's later request.
    respond(&mut kube_handle, 200, &node_json("node-0", "node-uid-0")).await;
    respond(&mut kube_handle, 404, &not_found_json()).await;
    let (create_request, send) = kube_handle
        .next_request()
        .await
        .expect("create request sent");
    assert_eq!(create_request.method(), http::Method::POST);
    send.send_response(http::Response::new(Body::from(
        serde_json::to_vec(&json!({})).unwrap(),
    )));

    // Registration server came up: GetInfo answers correctly.
    let mut registration_client = MockRegistrationClient::connect(&registration_socket_path)
        .await
        .expect("connect to registration socket");
    let info = registration_client.get_info().await.expect("get_info call");
    assert_eq!(info.r#type, "DRAPlugin");
    assert_eq!(info.name, "example.com");
    assert_eq!(
        info.supported_versions,
        vec![DRA_PLUGIN_SERVICE.to_string()]
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
        let bytes = serde_json::to_vec(&body).unwrap();
        send.send_response(http::Response::new(Body::from(bytes)));
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

#[tokio::test]
async fn run_at_stops_registration_when_service_startup_fails() {
    let (client, _kube_handle) = mock_kube_client();
    let registration_dir = TempDir::new().expect("create temp dir for registration socket");
    let registration_socket_path = registration_dir.path().join("plugin-reg.sock");
    let plugin_dir = TempDir::new().expect("create temp dir for plugin socket");
    let plugin_socket_path = plugin_dir.path().join("missing").join("plugin.sock");

    let plugin = DraPlugin::for_test(
        client,
        "example.com",
        "node-0",
        StaticDraDriver {
            name: "example.com".to_string(),
            pool: "pool-0".to_string(),
            devices: vec![PoolDevice::new("widget-0")],
        },
        registration_socket_path.clone(),
    );

    let err = plugin
        .run_at(&plugin_socket_path)
        .await
        .expect_err("service bind should fail when its parent directory is absent");
    assert_eq!(err.kind(), io::ErrorKind::NotFound);
    assert!(
        tokio::net::UnixStream::connect(&registration_socket_path)
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
    tokio::time::timeout(Duration::from_secs(1), registration_stopped)
        .await
        .expect("registration task is reaped")
        .expect("registration drop notifier runs");
    tokio::time::timeout(Duration::from_secs(1), publisher_stopped)
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
