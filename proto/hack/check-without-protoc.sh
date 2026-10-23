#!/bin/sh
# Use a fresh target directory so cached bindings cannot hide a protoc call.
set -eu

repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
check_dir=$(mktemp -d)
trap 'rm -rf "$check_dir"' EXIT
trap 'exit 1' HUP INT TERM

cat > "$check_dir/protoc" <<'EOF'
#!/bin/sh
printf 'unexpected protoc invocation\n' >> "$PROTOC_CALL_LOG"
printf 'protocol generation must not invoke protoc\n' >&2
exit 1
EOF
chmod +x "$check_dir/protoc"

export PROTOC_CALL_LOG="$check_dir/protoc-calls"
export PROTOC="$check_dir/protoc"
export PROTOC_INCLUDE="$check_dir/missing-include"
export PATH="$check_dir:$PATH"
export CARGO_TARGET_DIR="$check_dir/target"

cd "$repo_root"
cargo check --locked -p k8s-device-plugin-proto --no-default-features
cargo check --locked -p k8s-device-plugin-proto --no-default-features --features dra
cargo check --locked -p k8s-device-plugin-proto --no-default-features --features dra-health

if [ -e "$PROTOC_CALL_LOG" ]; then
    printf 'external protoc was invoked\n' >&2
    exit 1
fi
printf 'all protocol feature combinations compiled without protoc\n'
