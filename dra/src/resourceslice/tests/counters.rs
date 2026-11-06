use k8s_device_plugin_core::CounterSet;
use k8s_device_plugin_core::DeviceCounterConsumption;
use k8s_device_plugin_core::Quantity;

use super::*;

struct SnapshotPool(PoolInventory);

#[tonic::async_trait]
impl ResourcePool for SnapshotPool {
    async fn devices(&self) -> BTreeMap<String, Vec<PoolDevice>> {
        panic!("publisher must read one coherent snapshot")
    }

    async fn snapshot(&self) -> BTreeMap<String, PoolInventory> {
        BTreeMap::from([("pool-0".into(), self.0.clone())])
    }
}

fn counter_set(name: &str, value: i64) -> CounterSet {
    CounterSet::new(name).counter("cards", Quantity::from(value))
}

fn consuming_device(name: &str, set: &str, value: i64) -> InventoryDevice {
    InventoryDevice::new(PoolDevice::new(name)).with_consumes_counters(
        DeviceCounterConsumption::new(set).counter("cards", Quantity::from(value)),
    )
}

fn paired_cards() -> PoolInventory {
    PoolInventory::new(vec![
        consuming_device("card-0", "pair-a", 1),
        consuming_device("card-1", "pair-a", 1),
        consuming_device("bridge-a", "pair-a", 2),
    ])
    .with_counter_set(counter_set("pair-a", 2))
}

#[test]
fn integer_counter_quantities_use_kubernetes_canonical_wire_values() {
    for (value, expected) in [
        (0, "0"),
        (1000, "1k"),
        (1_000_000, "1M"),
        (1_000_000_000_000_000_000, "1E"),
        (-1000, "-1k"),
        (i64::MIN, "-9223372036854775808"),
        (i64::MAX, "9223372036854775807"),
    ] {
        let sets = counter_sets_to_wire(vec![counter_set("pair-a", value)], "pool-0").unwrap();
        assert_eq!(sets[0].counters["cards"].value.0, expected);
    }
}

fn snapshot_publisher(inventory: PoolInventory) -> (ResourceSlicePublisher, MockKubeHandle) {
    let (client, handle) = mock_kube_client();
    let pool: Arc<dyn ResourcePool> = Arc::new(SnapshotPool(inventory));
    (
        ResourceSlicePublisher::new(client, "example.com", "node-0", pool).unwrap(),
        handle,
    )
}

async fn reply_to_write(
    handle: &mut MockKubeHandle,
    method: http::Method,
) -> resourcev1::ResourceSlice {
    let (request, send) = tokio::time::timeout(Duration::from_secs(2), handle.next_request())
        .await
        .expect("slice write before timeout")
        .expect("slice write");
    assert_eq!(request.method(), method);
    let bytes = request.into_body().collect().await.unwrap().to_bytes();
    let mut slice = json::from_slice::<resourcev1::ResourceSlice>(&bytes).unwrap();
    slice.metadata.uid = Some(format!("uid-{}", slice.metadata.name.as_deref().unwrap()));
    slice.metadata.resource_version = Some("8".into());
    send.send_response(http::Response::new(client::Body::from(
        json::to_vec(&slice).unwrap(),
    )));
    slice
}

async fn assert_no_writes(handle: &mut MockKubeHandle) {
    assert!(
        tokio::time::timeout(Duration::from_millis(50), handle.next_request())
            .await
            .is_err(),
        "unchanged or invalid pool must not mutate slices"
    );
}

#[tokio::test]
async fn counters_and_devices_publish_separately_and_second_poll_is_unchanged() {
    let (publisher, mut handle) = snapshot_publisher(paired_cards());
    let script = async {
        respond(&mut handle, 200, &node("node-0", "node-uid-0")).await;
        respond(&mut handle, 200, &resource_slice_list(vec![])).await;
        let devices = reply_to_write(&mut handle, http::Method::POST).await;
        let counters = reply_to_write(&mut handle, http::Method::POST).await;
        assert_eq!(devices.spec.pool, counters.spec.pool);
        assert_eq!(devices.spec.pool.generation, 1);
        assert_eq!(devices.spec.pool.resource_slice_count, 2);
        let devices_json = json::to_value(&devices).unwrap();
        assert!(devices_json["spec"].get("sharedCounters").is_none());
        assert_eq!(
            devices_json["spec"]["devices"][0]["consumesCounters"],
            json::json!([
                {"counterSet": "pair-a", "counters": {"cards": {"value": "2"}}}
            ])
        );
        let counters_json = json::to_value(&counters).unwrap();
        assert!(counters_json["spec"].get("devices").is_none());
        assert_eq!(
            counters_json["spec"]["sharedCounters"],
            json::json!([
                {"name": "pair-a", "counters": {"cards": {"value": "2"}}}
            ])
        );
        respond(&mut handle, 200, &node("node-0", "node-uid-0")).await;
        respond(
            &mut handle,
            200,
            &resource_slice_list(vec![devices, counters]),
        )
        .await;
        assert_no_writes(&mut handle).await;
    };
    let publish = async {
        publisher.publish_once().await.unwrap();
        publisher.publish_once().await.unwrap();
    };
    tokio::join!(script, publish);
}

#[tokio::test]
async fn server_returned_empty_collections_do_not_republish() {
    let inventory = PoolInventory::new(vec![
        consuming_device("card-0", "pair-a", 1),
        InventoryDevice::new(PoolDevice::new("plain")),
    ])
    .with_counter_set(counter_set("pair-a", 2));
    let (publisher, mut handle) = snapshot_publisher(inventory);
    let script = async {
        respond(&mut handle, 200, &node("node-0", "node-uid-0")).await;
        respond(&mut handle, 200, &resource_slice_list(vec![])).await;
        let mut devices = reply_to_write(&mut handle, http::Method::POST).await;
        let mut counters = reply_to_write(&mut handle, http::Method::POST).await;
        devices.spec.shared_counters = Some(vec![]);
        counters.spec.devices = Some(vec![]);
        for device in devices.spec.devices.iter_mut().flatten() {
            device.attributes = Some(BTreeMap::new());
            device.capacity = Some(BTreeMap::new());
            device.binding_conditions = Some(vec![]);
            device.binding_failure_conditions = Some(vec![]);
            device.taints = Some(vec![]);
            if device.consumes_counters.is_none() {
                device.consumes_counters = Some(vec![]);
            }
            k8s_openapi::k8s_if_ge_1_37! {
                device.node_allocatable_resources = Some(BTreeMap::new());
                for consumption in device.consumes_counters.iter_mut().flatten() {
                    consumption.compatibility_groups = Some(vec![]);
                }
            }
            k8s_openapi::k8s_if_le_1_36! {
                device.node_allocatable_resource_mappings = Some(BTreeMap::new());
            }
        }
        k8s_openapi::k8s_if_ge_1_37! {
            devices.spec.skip_node_operations = Some(vec![]);
            counters.spec.skip_node_operations = Some(vec![]);
        }
        for _ in 0..2 {
            let request = respond(&mut handle, 200, &node("node-0", "node-uid-0")).await;
            assert_eq!(request.method(), http::Method::GET);
            let request = respond(
                &mut handle,
                200,
                &resource_slice_list(vec![devices.clone(), counters.clone()]),
            )
            .await;
            assert_eq!(request.method(), http::Method::GET);
        }
        assert_no_writes(&mut handle).await;
    };
    let publish = async {
        for _ in 0..3 {
            publisher.publish_once().await.unwrap();
        }
    };
    tokio::join!(script, publish);
}

#[tokio::test]
async fn fractional_capacity_and_counters_converge_across_server_notation() {
    let inventory = PoolInventory::new(vec![
        InventoryDevice::new(
            PoolDevice::new("card-0").capacity("memory", "1.5Gi".parse().unwrap()),
        )
        .with_consumes_counters(
            DeviceCounterConsumption::new("pair-a").counter("cards", "0.125".parse().unwrap()),
        ),
    ])
    .with_counter_set(CounterSet::new("pair-a").counter("cards", "0.5".parse().unwrap()));
    let (publisher, mut handle) = snapshot_publisher(inventory);
    let script = async {
        respond(&mut handle, 200, &node("node-0", "node-uid-0")).await;
        respond(&mut handle, 200, &resource_slice_list(vec![])).await;
        let mut devices = reply_to_write(&mut handle, http::Method::POST).await;
        let mut counters = reply_to_write(&mut handle, http::Method::POST).await;
        let device = &mut devices.spec.devices.as_mut().unwrap()[0];
        let capacity = &mut device
            .capacity
            .as_mut()
            .unwrap()
            .get_mut("memory")
            .unwrap()
            .value;
        assert_eq!(capacity.0, "1536Mi");
        capacity.0 = "1610612736".into();
        let consumption = &mut device.consumes_counters.as_mut().unwrap()[0]
            .counters
            .get_mut("cards")
            .unwrap()
            .value;
        assert_eq!(consumption.0, "125m");
        consumption.0 = "0.125".into();
        let shared = &mut counters.spec.shared_counters.as_mut().unwrap()[0]
            .counters
            .get_mut("cards")
            .unwrap()
            .value;
        assert_eq!(shared.0, "500m");
        shared.0 = "5e-1".into();
        respond(&mut handle, 200, &node("node-0", "node-uid-0")).await;
        respond(
            &mut handle,
            200,
            &resource_slice_list(vec![devices, counters]),
        )
        .await;
        assert_no_writes(&mut handle).await;
    };
    let publish = async {
        publisher.publish_once().await.unwrap();
        publisher.publish_once().await.unwrap();
    };
    tokio::join!(script, publish);
}

#[tokio::test]
async fn counter_and_consumption_changes_advance_every_slice_once() {
    for change_counter in [true, false] {
        let initial = paired_cards();
        let mut changed = initial.clone();
        if change_counter {
            changed.shared_counters[0]
                .counters
                .insert("cards".into(), Quantity::from(3));
        } else {
            changed.devices[0].consumes_counters[0]
                .counters
                .insert("cards".into(), Quantity::from(2));
        }
        let (old, mut old_handle) = snapshot_publisher(initial);
        let script = async {
            respond(&mut old_handle, 200, &node("node-0", "node-uid-0")).await;
            respond(&mut old_handle, 200, &resource_slice_list(vec![])).await;
            let devices = reply_to_write(&mut old_handle, http::Method::POST).await;
            let counters = reply_to_write(&mut old_handle, http::Method::POST).await;
            vec![devices, counters]
        };
        let (existing, result) = tokio::join!(script, old.publish_once());
        result.unwrap();
        let (publisher, mut handle) = snapshot_publisher(changed);
        let script = async {
            respond(&mut handle, 200, &node("node-0", "node-uid-0")).await;
            respond(&mut handle, 200, &resource_slice_list(existing)).await;
            let devices = reply_to_write(&mut handle, http::Method::PUT).await;
            let counters = reply_to_write(&mut handle, http::Method::PUT).await;
            assert_eq!(devices.spec.pool.generation, 2);
            assert_eq!(devices.spec.pool, counters.spec.pool);
            respond(&mut handle, 200, &node("node-0", "node-uid-0")).await;
            respond(
                &mut handle,
                200,
                &resource_slice_list(vec![devices, counters]),
            )
            .await;
            assert_no_writes(&mut handle).await;
        };
        let publish = async {
            publisher.publish_once().await.unwrap();
            publisher.publish_once().await.unwrap();
        };
        tokio::join!(script, publish);
    }
}

#[tokio::test]
async fn counter_sets_and_consuming_devices_split_at_api_limits() {
    let devices = (0..65)
        .map(|i| consuming_device(&format!("card-{i:03}"), "pair-8", 1))
        .collect();
    let mut inventory = PoolInventory::new(devices);
    inventory.shared_counters = (0..9)
        .map(|i| counter_set(&format!("pair-{i}"), 1000))
        .collect();
    let (publisher, mut handle) = snapshot_publisher(inventory);
    let script = async {
        respond(&mut handle, 200, &node("node-0", "node-uid-0")).await;
        respond(&mut handle, 200, &resource_slice_list(vec![])).await;
        for (device_count, counter_count) in [(64, 0), (1, 0), (0, 8), (0, 1)] {
            let slice = reply_to_write(&mut handle, http::Method::POST).await;
            assert_eq!(slice.spec.pool.generation, 1);
            assert_eq!(slice.spec.pool.resource_slice_count, 4);
            assert_eq!(
                slice.spec.devices.as_ref().map_or(0, Vec::len),
                device_count
            );
            assert_eq!(
                slice.spec.shared_counters.as_ref().map_or(0, Vec::len),
                counter_count
            );
        }
    };
    let (_, result) = tokio::join!(script, publisher.publish_once());
    result.unwrap();
}

#[tokio::test]
async fn reordered_sets_consumptions_and_devices_keep_generation() {
    let mut inventory = paired_cards().with_counter_set(counter_set("pair-b", 1000));
    inventory.devices[0]
        .consumes_counters
        .push(DeviceCounterConsumption::new("pair-b").counter("cards", Quantity::from(1000)));
    let (publisher, mut handle) = snapshot_publisher(inventory);
    let script = async {
        respond(&mut handle, 200, &node("node-0", "node-uid-0")).await;
        respond(&mut handle, 200, &resource_slice_list(vec![])).await;
        let mut devices = reply_to_write(&mut handle, http::Method::POST).await;
        let mut counters = reply_to_write(&mut handle, http::Method::POST).await;
        let wire_devices = devices.spec.devices.as_mut().unwrap();
        wire_devices.reverse();
        for device in wire_devices {
            device.consumes_counters.as_mut().unwrap().reverse();
        }
        let sets = counters.spec.shared_counters.as_mut().unwrap();
        sets.reverse();
        assert_eq!(sets[0].counters["cards"].value.0, "1k");
        respond(&mut handle, 200, &node("node-0", "node-uid-0")).await;
        respond(
            &mut handle,
            200,
            &resource_slice_list(vec![counters, devices]),
        )
        .await;
        assert_no_writes(&mut handle).await;
    };
    let publish = async {
        publisher.publish_once().await.unwrap();
        publisher.publish_once().await.unwrap();
    };
    tokio::join!(script, publish);
}

#[tokio::test]
async fn invalid_counter_pools_fail_before_any_write() {
    let mut cases = vec![];
    let mut unknown_set = paired_cards();
    unknown_set.shared_counters.clear();
    cases.push(unknown_set);
    let mut unknown_counter = paired_cards();
    unknown_counter.devices[0].consumes_counters[0]
        .counters
        .insert("unknown".into(), Quantity::from(1));
    cases.push(unknown_counter);
    cases.push(paired_cards().with_counter_set(counter_set("pair-a", 2)));
    cases.push(paired_cards().with_counter_set(counter_set("bad.name", 2)));
    cases.push(paired_cards().with_counter_set(CounterSet::new("empty")));
    let mut too_many_counters = CounterSet::new("large");
    for i in 0..33 {
        too_many_counters = too_many_counters.counter(format!("c-{i}"), Quantity::from(1));
    }
    cases.push(paired_cards().with_counter_set(too_many_counters));
    cases.push(
        paired_cards()
            .with_counter_set(CounterSet::new("invalid-key").counter("Bad", Quantity::from(1))),
    );
    let mut duplicate_consumption = paired_cards();
    let consumption = duplicate_consumption.devices[0].consumes_counters[0].clone();
    duplicate_consumption.devices[0]
        .consumes_counters
        .push(consumption);
    cases.push(duplicate_consumption);
    let mut too_many_consumptions = paired_cards();
    too_many_consumptions.devices[0].consumes_counters = (0..3)
        .map(|i| {
            DeviceCounterConsumption::new(format!("pair-{i}")).counter("cards", Quantity::from(1))
        })
        .collect();
    cases.push(too_many_consumptions);
    let mut empty_consumption = paired_cards();
    empty_consumption.devices[0].consumes_counters[0]
        .counters
        .clear();
    cases.push(empty_consumption);
    let mut invalid_consumption = paired_cards();
    invalid_consumption.devices[0].consumes_counters[0].counter_set = "Bad".into();
    cases.push(invalid_consumption);
    let mut oversized_consumption = paired_cards();
    oversized_consumption.devices[0].consumes_counters[0].counters = (0..33)
        .map(|i| (format!("c-{i}"), Quantity::from(1)))
        .collect();
    cases.push(oversized_consumption);
    for inventory in cases {
        let (publisher, mut handle) = snapshot_publisher(inventory);
        let script = async {
            respond(&mut handle, 200, &node("node-0", "node-uid-0")).await;
            respond(&mut handle, 200, &resource_slice_list(vec![])).await;
            assert_no_writes(&mut handle).await;
        };
        let (_, result) = tokio::join!(script, publisher.publish_once());
        assert!(matches!(result, Err(kube::Error::Api(status)) if status.code == 422));
    }
}

#[tokio::test]
async fn removing_counters_updates_devices_before_deleting_the_counter_slice() {
    let (publisher, mut handle) = snapshot_publisher(paired_cards());
    let script = async {
        respond(&mut handle, 200, &node("node-0", "node-uid-0")).await;
        respond(&mut handle, 200, &resource_slice_list(vec![])).await;
        let devices = reply_to_write(&mut handle, http::Method::POST).await;
        let counters = reply_to_write(&mut handle, http::Method::POST).await;
        vec![devices, counters]
    };
    let (existing, result) = tokio::join!(script, publisher.publish_once());
    result.unwrap();
    let (publisher, mut handle) =
        snapshot_publisher(PoolInventory::new(vec![InventoryDevice::new(
            PoolDevice::new("card-0"),
        )]));
    let script = async {
        respond(&mut handle, 200, &node("node-0", "node-uid-0")).await;
        respond(&mut handle, 200, &resource_slice_list(existing)).await;
        let devices = reply_to_write(&mut handle, http::Method::PUT).await;
        assert_eq!(devices.spec.pool.generation, 2);
        assert_eq!(devices.spec.pool.resource_slice_count, 1);
        let (request, send) = handle.next_request().await.unwrap();
        assert_eq!(request.method(), http::Method::DELETE);
        assert!(
            request
                .uri()
                .path()
                .ends_with(&slice_name("example.com", "node-0", "pool-0", 1))
        );
        assert_eq!(
            request_body_json(request).await["preconditions"]["resourceVersion"],
            "8"
        );
        send.send_response(http::Response::new(client::Body::from(
            json::to_vec(&metav1::Status {
                status: Some("Success".into()),
                ..default()
            })
            .unwrap(),
        )));
    };
    let (_, result) = tokio::join!(script, publisher.publish_once());
    result.unwrap();
}

#[tokio::test]
async fn counters_only_pool_is_retained() {
    let (publisher, mut handle) =
        snapshot_publisher(PoolInventory::default().with_counter_set(counter_set("pair-a", 2)));
    let script = async {
        respond(&mut handle, 200, &node("node-0", "node-uid-0")).await;
        respond(&mut handle, 200, &resource_slice_list(vec![])).await;
        let slice = reply_to_write(&mut handle, http::Method::POST).await;
        assert!(slice.spec.devices.is_none());
        assert_eq!(slice.spec.pool.resource_slice_count, 1);
        respond(&mut handle, 200, &node("node-0", "node-uid-0")).await;
        respond(&mut handle, 200, &resource_slice_list(vec![slice])).await;
        assert_no_writes(&mut handle).await;
    };
    let publish = async {
        publisher.publish_once().await.unwrap();
        publisher.publish_once().await.unwrap();
    };
    tokio::join!(script, publish);
}
