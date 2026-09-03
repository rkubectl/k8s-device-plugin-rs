#!/usr/bin/env bash
# Serially validate the DRA fixture against the two local native-arm64 clusters.
#
# This is intentionally opt-in: it builds a container image and changes local
# cluster state. It restores the v1.36 baseline as the sole running cluster.

set -euo pipefail

CONTAINER=${CONTAINER:-container}
KUBECTL=${KUBECTL:-kubectl}
JQ=${JQ:-jq}
IMAGE=${DRA_E2E_IMAGE:-k8s-device-plugin-dra-example}
NAMESPACE=k8s-device-plugin-dra-example
V136_CLUSTER=${DRA_E2E_V136_CLUSTER:-dra-validation}
V137_CLUSTER=${DRA_E2E_V137_CLUSTER:-dra-validation-137}
SMOKE_SCRIPT=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)/e2e-smoke.sh

require_command() {
    command -v "$1" >/dev/null 2>&1 || {
        printf 'required command not found: %s\n' "$1" >&2
        exit 1
    }
}

cluster_json() {
    "$CONTAINER" list --all --format json | "$JQ" -cer --arg cluster "$1" '
        map(select(.id == $cluster)) |
        if length == 1 then .[0] else error("expected exactly one cluster named \($cluster), found \(length)") end
    '
}

cluster_state() {
    cluster_json "$1" | "$JQ" -er '.status.state'
}

assert_native_cluster() {
    local cluster=$1
    local rosetta

    rosetta=$(cluster_json "$cluster" | "$JQ" -er '
        if (.configuration.rosetta | type) == "boolean" then
            .configuration.rosetta | tostring
        else
            error("missing boolean rosetta configuration")
        end
    ')
    if [[ "$rosetta" != false ]]; then
        printf 'cluster %s has rosetta=%s; native validation requires rosetta=false\n' "$cluster" "$rosetta" >&2
        exit 1
    fi

    return 0
}

stop_if_running() {
    local cluster=$1

    if [[ $(cluster_state "$cluster") == running ]]; then
        "$CONTAINER" stop "$cluster"
    fi

    return 0
}

assert_server_version() {
    local cluster=$1
    local expected_version=$2
    local actual_version

    actual_version=$("$KUBECTL" --context "$cluster" version -o json | "$JQ" -er '.serverVersion.gitVersion')
    if [[ "$actual_version" != "$expected_version" ]]; then
        printf 'cluster %s is %s, expected %s\n' "$cluster" "$actual_version" "$expected_version" >&2
        exit 1
    fi

    return 0
}

node_internal_ip() {
    local cluster=$1

    "$KUBECTL" --context "$cluster" get node "$cluster" -o json |
        "$JQ" -er '.status.addresses[] | select(.type == "InternalIP") | .address'
}

start_cluster() {
    local cluster=$1
    local previous_ip=
    local current_ip
    local stable_samples=0
    local attempt

    if [[ $(cluster_state "$cluster") != running ]]; then
        "$CONTAINER" start "$cluster"
    fi

    for attempt in {1..18}; do
        if current_ip=$(node_internal_ip "$cluster" 2>/dev/null); then
            if [[ "$current_ip" == "$previous_ip" ]]; then
                ((stable_samples += 1))
                if ((stable_samples == 2)); then
                    return 0
                fi
            else
                previous_ip=$current_ip
                stable_samples=0
            fi
        fi
        sleep 5
    done

    printf 'cluster %s did not report a stable InternalIP after start\n' "$cluster" >&2
    exit 1
}

repair_kube_proxy_endpoint() {
    local cluster=$1
    local node_ip
    local patch

    node_ip=$(node_internal_ip "$cluster")
    patch=$("$KUBECTL" --context "$cluster" -n kube-system get configmap/kube-proxy -o json |
        "$JQ" -ce --arg server "https://${node_ip}:6443" '
            .data["kubeconfig.conf"] as $config |
            if $config == null then
                error("kube-proxy ConfigMap has no kubeconfig.conf")
            else
                {data: {"kubeconfig.conf": ($config | sub("https://[0-9.]+:6443"; $server))}}
            end
        ')

    "$KUBECTL" --context "$cluster" -n kube-system patch configmap/kube-proxy --type=merge -p "$patch"
    "$KUBECTL" --context "$cluster" -n kube-system rollout restart daemonset/kube-proxy
    "$KUBECTL" --context "$cluster" -n kube-system rollout status daemonset/kube-proxy --timeout=180s
    "$KUBECTL" --context "$cluster" -n kube-system rollout status deployment/coredns --timeout=180s
}

restore_baseline() {
    local result=$?

    trap - EXIT
    set +e
    stop_if_running "$V137_CLUSTER"
    start_cluster "$V136_CLUSTER"
    repair_kube_proxy_endpoint "$V136_CLUSTER"
    exit "$result"
}
trap restore_baseline EXIT

run_profile() {
    local expected_version=$1
    local cluster=$2
    local other_cluster=$3

    stop_if_running "$other_cluster"
    start_cluster "$cluster"
    assert_server_version "$cluster" "$expected_version"
    repair_kube_proxy_endpoint "$cluster"

    "$CONTAINER" k8s-versioned load-image --name "$cluster" "$IMAGE"
    "$KUBECTL" --context "$cluster" apply -k dra/k8s
    "$KUBECTL" --context "$cluster" -n "$NAMESPACE" rollout restart daemonset/k8s-device-plugin-dra-example
    "$KUBECTL" --context "$cluster" -n "$NAMESPACE" rollout status daemonset/k8s-device-plugin-dra-example --timeout=180s
    KUBE_CONTEXT="$cluster" KUBECTL="$KUBECTL" "$SMOKE_SCRIPT"

    stop_if_running "$cluster"
}

require_command "$CONTAINER"
require_command "$KUBECTL"
require_command "$JQ"
test -x "$SMOKE_SCRIPT"

assert_native_cluster "$V136_CLUSTER"
assert_native_cluster "$V137_CLUSTER"

if [[ ${DRA_E2E_SKIP_BUILD:-0} != 1 ]]; then
    "$CONTAINER" build --platform linux/arm64 -f dra/Dockerfile -t "$IMAGE" .
fi

run_profile v1.36.1 "$V136_CLUSTER" "$V137_CLUSTER"
run_profile v1.37.0 "$V137_CLUSTER" "$V136_CLUSTER"
