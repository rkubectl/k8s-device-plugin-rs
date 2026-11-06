#!/bin/sh
# Build an independent application so workspace feature unification cannot hide
# a library accidentally selecting a Kubernetes schema on the consumer's behalf.
set -eu

repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
check_dir=$(mktemp -d)
trap 'rm -rf "$check_dir"' EXIT
trap 'exit 1' HUP INT TERM

facade_path=$(printf '%s' "$repo_root/k8s-device-plugin" | jq -Rs .)
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

[features]
resource-health = ["k8s-device-plugin/resource-health", "k8s-device-plugin-dra/resource-health"]

[dependencies]
k8s-openapi = "0.29"
kube = "5.0"
k8s-device-plugin = { path = $facade_path }
k8s-device-plugin-dra = { path = $dra_path }
k8s-device-plugin-test = { path = $test_path, features = ["dra"] }
EOF
cat > "$check_dir/src/main.rs" <<'EOF'
use k8s_openapi::api::resource::v1 as resourcev1;

fn main() {
    let inventory = k8s_device_plugin::PoolInventory::new(vec![
        k8s_device_plugin::InventoryDevice::new(k8s_device_plugin::PoolDevice::new("card-0"))
            .with_consumes_counters(k8s_device_plugin::DeviceCounterConsumption::new("pair-a")
                .counter("cards", k8s_device_plugin::Quantity(1))),
    ]).with_counter_set(k8s_device_plugin::CounterSet::new("pair-a")
        .counter("cards", k8s_device_plugin::Quantity(2)));
    assert_eq!(inventory.shared_counters.len(), 1);
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
    #[cfg(feature = "resource-health")]
    let _ = std::mem::size_of::<k8s_device_plugin::DraResourceHealthService>();
}
EOF

# The application's Cargo features, not a maintainer's environment, must select
# the schema. Its dependencies' dev-dependencies are also outside this graph.
unset K8S_OPENAPI_ENABLED_VERSION
export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-"$repo_root/target"}
cd "$check_dir"
for schema in v1_36 v1_37; do
    cargo check --manifest-path Cargo.toml --features "k8s-openapi/$schema"
    cargo check --manifest-path Cargo.toml --features "k8s-openapi/$schema,resource-health"
    printf 'consumer-selected %s passed, with and without resource-health\n' "$schema"
done
