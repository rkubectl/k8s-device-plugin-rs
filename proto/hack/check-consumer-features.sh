#!/bin/sh
# Check the public protocol feature boundary in an independent application.
set -eu
repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
check_dir=$(mktemp -d)
trap 'rm -rf "$check_dir"' EXIT
trap 'exit 1' HUP INT TERM
proto_path=$(printf '%s' "$repo_root/proto" | jq -Rs .)
mkdir "$check_dir/src"
cat > "$check_dir/Cargo.toml" <<EOF
[package]
name = "protocol-boundary-consumer"
version = "0.0.0"
edition = "2024"
[workspace]
[features]
dra = ["k8s-device-plugin-proto/dra"]
health = ["k8s-device-plugin-proto/dra-health"]
[dependencies]
k8s-device-plugin-proto = { path = $proto_path, default-features = false }
EOF
cat > "$check_dir/src/main.rs" <<'EOF'
use k8s_device_plugin_proto::v1beta1;

fn main() {
    let _ = v1beta1::RegisterRequest::new("endpoint", "example.com/device");
    #[cfg(feature = "dra")]
    let _ = k8s_device_plugin_proto::dra::v1::SERVICE_NAME;
    #[cfg(feature = "health")]
    let _ = k8s_device_plugin_proto::dra::health::v1alpha1::SERVICE_NAME;
    #[cfg(feature = "health")]
    let _ = k8s_device_plugin_proto::dra::health::v1::SERVICE_NAME;
}
EOF
unset K8S_OPENAPI_ENABLED_VERSION
export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-"$repo_root/target"}
cd "$check_dir"
for features in '' dra health; do
    if [ -n "$features" ]; then
        cargo check --features "$features"
    else
        cargo check
    fi
    printf 'protocol consumer passed with features: %s\n' "${features:-classic}"
done
