#!/bin/sh
# Core remains independent of generated gRPC and Kubernetes API types, even
# when all of its own features are enabled. An isolated app avoids workspace
# feature unification (DRA legitimately enables wire features on dependencies).
set -eu

repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
check_dir=$(mktemp -d)
trap 'rm -rf "$check_dir"' EXIT
trap 'exit 1' HUP INT TERM
core_path=$(printf '%s' "$repo_root/core" | jq -Rs .)
mkdir "$check_dir/src"
cat > "$check_dir/Cargo.toml" <<EOF
[package]
name = "core-boundary-consumer"
version = "0.0.0"
edition = "2024"
publish = false

[workspace]

[dependencies]
k8s-device-plugin-core = { path = $core_path, default-features = true }
EOF
cat > "$check_dir/src/main.rs" <<'EOF'
use k8s_device_plugin_core::Device;

fn main() {
    Device::rdwr("widget", "/dev/widget").validate().unwrap();
}
EOF

unset K8S_OPENAPI_ENABLED_VERSION
export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-"$repo_root/target"}
cd "$check_dir"
check_boundary() {
    cargo check "$@"
    cargo tree --edges normal --prefix none --format '{p}' "$@" > "$check_dir/tree"
    jq -Rse '(contains("k8s-openapi v") or contains("kube v") or
        contains("k8s-device-plugin-proto v") or contains("tonic v") or contains("k8s-device-plugin-dra") or
        contains("k8s-quantity v") or contains("bigdecimal v")) | not' \
        < "$check_dir/tree" >/dev/null
}

check_boundary
printf 'default core consumer passed without transport or Kubernetes schema\n'
sed 's/default-features = true/default-features = false/' Cargo.toml > isolated.toml
mv isolated.toml Cargo.toml
check_boundary
printf 'minimal core consumer passed without transport or Kubernetes schema\n'
# Discover the owner's features instead of only testing today's known list.
core_features=$(cargo metadata --format-version 1 | jq -r '
    .packages[] | select(.name == "k8s-device-plugin-core") |
    .features | keys | map("k8s-device-plugin-core/" + .) | join(",")
')
if [ -n "$core_features" ]; then
    check_boundary --features "$core_features"
else
    check_boundary --all-features
fi
printf 'all core features passed without transport or Kubernetes schema\n'

facade_path=$(printf '%s' "$repo_root/k8s-device-plugin" | jq -Rs .)
cat > Cargo.toml <<EOF
[package]
name = "classic-facade-consumer"
version = "0.0.0"
edition = "2024"
[workspace]
[dependencies]
k8s-device-plugin = { path = $facade_path, default-features = false, features = ["device-plugin"] }
EOF
cat > src/main.rs <<'EOF'
fn main() { k8s_device_plugin::Device::rdwr("widget", "/dev/widget").validate().unwrap(); }
EOF
cargo check
cargo tree --edges normal --prefix none --format '{p}' > "$check_dir/tree"
jq -Rse '(contains("k8s-openapi v") or contains("kube v") or contains("k8s-device-plugin-dra") or contains("k8s-quantity v") or contains("bigdecimal v")) | not' < "$check_dir/tree" >/dev/null
printf 'classic facade consumer passed without DRA or schema\n'

quantity_path=$(printf '%s' "$repo_root/quantity" | jq -Rs .)
cat > Cargo.toml <<EOF2
[package]
name = "quantity-boundary-consumer"
version = "0.0.0"
edition = "2024"
[workspace]
[dependencies]
k8s-quantity = { path = $quantity_path, default-features = false }
EOF2
cat > src/main.rs <<'EOF2'
fn main() { assert_eq!("0.5".parse::<k8s_quantity::Quantity>().unwrap().to_string(), "500m"); }
EOF2
cargo check
cargo tree --edges normal --prefix none --format '{p}' > "$check_dir/tree"
jq -Rse '(contains("k8s-openapi v") or contains("kube v") or contains("tonic v")) | not' < "$check_dir/tree" >/dev/null
printf 'quantity consumer passed without Kubernetes schema\n'
