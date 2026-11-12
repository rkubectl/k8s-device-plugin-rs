#!/bin/sh
# Build an independent application so workspace feature unification cannot hide
# a library accidentally selecting a Kubernetes schema on the consumer's behalf.
set -eu

repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
check_dir=$(mktemp -d)
trap 'rm -rf "$check_dir"' EXIT
trap 'exit 1' HUP INT TERM

facade_path=$(printf '%s' "$repo_root/k8s-device-plugin" | jq -Rs .)
dra_core_path=$(printf '%s' "$repo_root/dra-core" | jq -Rs .)
dra_path=$(printf '%s' "$repo_root/dra" | jq -Rs .)
test_path=$(printf '%s' "$repo_root/test" | jq -Rs .)
mkdir "$check_dir/src"
cat > "$check_dir/Cargo.toml" <<EOF
[package]
name = "k8s-schema-consumer"
version = "0.0.0"
edition = "2024"
publish = false

[workspace]

[dependencies]
k8s-openapi = "0.29"
kube = "5.0"
k8s-device-plugin = { path = $facade_path }
k8s-device-plugin-dra = { path = $dra_path }
k8s-device-plugin-dra-core = { path = $dra_core_path }
k8s-device-plugin-test = { path = $test_path, features = ["dra"] }
EOF
cat > "$check_dir/src/main.rs" <<'EOF'
use k8s_openapi::api::resource::v1 as resourcev1;

fn main() {
    let inventory = k8s_device_plugin::PoolInventory::new(vec![
        k8s_device_plugin::InventoryDevice::new(k8s_device_plugin::PoolDevice::new("card-0".parse().unwrap()))
            .with_consumes_counters(k8s_device_plugin::DeviceCounterConsumption::new("pair-a".parse().unwrap())
                .counter("cards".parse().unwrap(), k8s_device_plugin::Quantity::from(1))),
    ]).with_counter_set(k8s_device_plugin::CounterSet::new("pair-a".parse().unwrap())
        .counter("cards".parse().unwrap(), k8s_device_plugin::Quantity::from(2)));
    assert_eq!(inventory.shared_counters.len(), 1);
    let exact: k8s_device_plugin::Quantity = "0.5".parse().unwrap();
    let wire: k8s_openapi::apimachinery::pkg::api::resource::Quantity = exact.into();
    assert_eq!(wire.0, "500m");
    assert_eq!(k8s_device_plugin::Quantity::try_from(&wire).unwrap(), "0.5".parse().unwrap());
    assert_eq!(k8s_device_plugin::Quantity::try_from(wire).unwrap(), "0.5".parse().unwrap());
    let claim = k8s_device_plugin::ClaimRef {
        namespace: "default".into(),
        name: "claim".into(),
        uid: "claim-uid".into(),
    };
    let (client, _handle) = k8s_device_plugin_test::kube_mock::mock_kube_client();
    let _: kube::Api<resourcev1::ResourceClaim> =
        <kube::Client as k8s_device_plugin::dra::KubeClientExt>::resource_claims(&client, &claim);
    let _: kube::Api<resourcev1::ResourceClaim> =
        <kube::Client as k8s_device_plugin_dra::KubeClientExt>::resource_claims(&client, &claim);
    let _ = std::mem::size_of::<k8s_device_plugin::DraResourceHealthService>();
    let _ = std::mem::size_of::<k8s_device_plugin_dra_core::ResourceHealthReport>();
    let _ = std::mem::size_of::<k8s_device_plugin_dra::DraResourceHealthService>();
    let publisher = k8s_device_plugin::ClaimDeviceStatusPublisher::new(client, "example.com".parse().unwrap());
    let _removal = publisher.remove(&claim, [k8s_device_plugin::ClaimDeviceStatusKey {
        pool_name: "pool".parse().unwrap(), device_name: "device".parse().unwrap(), share_id: None,
    }]);
}
EOF

# The application's Cargo features, not a maintainer's environment, must select
# the schema. Its dependencies' dev-dependencies are also outside this graph.
unset K8S_OPENAPI_ENABLED_VERSION
export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-"$repo_root/target"}
cd "$check_dir"
for schema in v1_36 v1_37; do
    cargo check --manifest-path Cargo.toml --features "k8s-openapi/$schema"
    printf 'consumer-selected %s passed with DRA health APIs\n' "$schema"
done

# Isolated graphs ensure the combined facade consumer above cannot hide a
# missing direct dependency or a feature selected by another library.
for schema in v1_36 v1_37; do
    for mode in core runtime facade-dra; do
        case "$mode" in
            core)
                dependency="k8s-device-plugin-dra-core = { path = $dra_core_path }"
                source='let _ = std::mem::size_of::<k8s_device_plugin_dra_core::ResourceHealthReport>();'
                ;;
            runtime)
                dependency="k8s-device-plugin-dra = { path = $dra_path }
k8s-device-plugin-dra-core = { path = $dra_core_path }"
                source='let _ = std::mem::size_of::<k8s_device_plugin_dra::DraResourceHealthService>(); fn configure(p: k8s_device_plugin_dra::DraPlugin) -> k8s_device_plugin_dra::DraPlugin { p.with_max_concurrent_resolves(4).with_claim_retry_policy(2, std::time::Duration::from_millis(20)).with_publisher_retry_backoff(std::time::Duration::from_secs(1), std::time::Duration::from_secs(5)) } let _ = configure as fn(_) -> _;'
                ;;
            facade-dra)
                dependency="k8s-device-plugin = { path = $facade_path, default-features = false, features = [\"dra\"] }"
                source='let _ = std::mem::size_of::<k8s_device_plugin::DraResourceHealthService>(); let _ = std::mem::size_of::<k8s_device_plugin::ResourceHealthReport>(); let _: k8s_device_plugin::DriverName = "example.com".parse().unwrap(); let _: k8s_device_plugin::NodeName = "node".parse().unwrap(); let _: k8s_device_plugin::PoolName = "pool".parse().unwrap(); let _: k8s_device_plugin::DeviceName = "device".parse().unwrap(); let _: k8s_device_plugin::CounterSetName = "set".parse().unwrap(); let _: k8s_device_plugin::CounterName = "counter".parse().unwrap(); fn configure(p: k8s_device_plugin::DraPlugin) -> k8s_device_plugin::DraPlugin { p.with_max_concurrent_resolves(4).with_claim_retry_policy(2, std::time::Duration::from_millis(20)).with_publisher_retry_backoff(std::time::Duration::from_secs(1), std::time::Duration::from_secs(5)) } let _ = configure as fn(_) -> _;'
                ;;
        esac
        case "$mode" in
            runtime) runtime_crate=k8s_device_plugin_dra; claim_crate=k8s_device_plugin_dra_core ;;
            facade-dra) runtime_crate=k8s_device_plugin; claim_crate=k8s_device_plugin ;;
            core) runtime_crate= ;;
        esac
        if [ -n "$runtime_crate" ]; then
            source="$source async fn remove(p: &$runtime_crate::ClaimDeviceStatusPublisher, c: &$claim_crate::ClaimRef, k: $runtime_crate::ClaimDeviceStatusKey) -> Result<$runtime_crate::ClaimDeviceStatusPublishOutcome, $runtime_crate::ClaimDeviceStatusError> { p.remove(c, [k]).await } let _ = remove;"
        fi
        cat > Cargo.toml <<MANIFEST
[package]
name = "isolated-dra-consumer"
version = "0.0.0"
edition = "2024"
[workspace]
[dependencies]
k8s-openapi = { version = "0.29", features = ["$schema"] }
$dependency
MANIFEST
        printf 'fn main() { %s }\n' "$source" > src/main.rs
        cargo check
        printf '%s consumer passed with %s\n' "$mode" "$schema"
    done
done
