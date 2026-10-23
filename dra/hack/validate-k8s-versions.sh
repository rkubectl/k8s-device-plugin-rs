#!/usr/bin/env bash
# Serially validate the DRA fixture against the two local native-arm64 clusters.
#
# This is intentionally opt-in: it builds a container image and changes local
# cluster state. It restores the v1.36 baseline as the sole running cluster.

set -euo pipefail

CONTAINER=${CONTAINER:-container}
KUBECTL=${KUBECTL:-kubectl}
JQ=${JQ:-jq}
# Build and load the same canonical reference; an older short-name alias can
# coexist with it in the host image store.
IMAGE=${DRA_E2E_IMAGE:-docker.io/library/k8s-device-plugin-dra-example:latest}
NAMESPACE=k8s-device-plugin-dra-example
V136_CLUSTER=${DRA_E2E_V136_CLUSTER:-dra-validation}
V137_CLUSTER=${DRA_E2E_V137_CLUSTER:-dra-validation-137}
SMOKE_SCRIPT=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)/e2e-smoke.sh
EXTENDED_RESOURCE_SMOKE_SCRIPT=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)/e2e-extended-resource.sh
TAINT_SMOKE_SCRIPT=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)/e2e-taints.sh
BINDING_SMOKE_SCRIPT=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)/e2e-binding.sh
WORKLOAD_SMOKE_SCRIPT=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)/e2e-workload.sh
ACTIVE_CLUSTER=

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
    local state

    state=$(cluster_state "$cluster") || return $?
    if [[ "$state" == running ]]; then
        "$CONTAINER" stop "$cluster" || return $?
    fi
}

assert_server_version() {
    local cluster=$1
    local expected_version=$2
    local actual_version

    actual_version=$("$KUBECTL" --context "$cluster" version -o json | "$JQ" -er '.serverVersion.gitVersion') || return $?
    if [[ "$actual_version" != "$expected_version" ]]; then
        printf 'cluster %s is %s, expected %s\n' "$cluster" "$actual_version" "$expected_version" >&2
        return 1
    fi

    return 0
}

start_cluster() {
    # Run even for an already-running VM: the helper repairs stale guest and
    # host kubeconfigs, reconciles kube-proxy, and waits for node/DNS readiness.
    "$CONTAINER" k8s-versioned start --name "$1"
}

capture_driver_logs() {
    printf '\nDRA driver logs for %s before stop:\n' "$1"
    "$KUBECTL" --context "$1" -n "$NAMESPACE" logs \
        daemonset/k8s-device-plugin-dra-example --all-containers=true --tail=200 \
        --request-timeout=15s --pod-running-timeout=15s ||
        printf 'could not collect driver logs for %s\n' "$1" >&2
}

restore_baseline() {
    local result=$?
    local restore_result=0

    trap - EXIT
    set +e
    if [[ -n "$ACTIVE_CLUSTER" ]]; then
        capture_driver_logs "$ACTIVE_CLUSTER"
    fi
    # Never boot the baseline if stopping the other profile failed.
    if stop_if_running "$V137_CLUSTER"; then
        start_cluster "$V136_CLUSTER" &&
            assert_server_version "$V136_CLUSTER" v1.36.1
        restore_result=$?
    else
        restore_result=$?
    fi
    if ((restore_result != 0)); then
        printf 'failed to restore the v1.36 baseline (exit %s)\n' "$restore_result" >&2
        if ((result == 0)); then
            result=$restore_result
        fi
    fi
    exit "$result"
}

run_profile() {
    local expected_version=$1
    local cluster=$2
    local other_cluster=$3

    stop_if_running "$other_cluster"
    ACTIVE_CLUSTER=$cluster
    start_cluster "$cluster"
    assert_server_version "$cluster" "$expected_version"

    "$CONTAINER" k8s-versioned load-image --name "$cluster" "$IMAGE"
    "$KUBECTL" --context "$cluster" apply -k dra/k8s
    "$KUBECTL" --context "$cluster" -n "$NAMESPACE" rollout restart daemonset/k8s-device-plugin-dra-example
    "$KUBECTL" --context "$cluster" -n "$NAMESPACE" rollout status daemonset/k8s-device-plugin-dra-example --timeout=180s
    KUBE_CONTEXT="$cluster" KUBECTL="$KUBECTL" "$SMOKE_SCRIPT"
    if [[ "$expected_version" == v1.37.0 ]]; then
        KUBE_CONTEXT="$cluster" KUBECTL="$KUBECTL" "$EXTENDED_RESOURCE_SMOKE_SCRIPT"
    fi
    if [[ ${DRA_E2E_TAINTS:-0} == 1 ]]; then
        KUBE_CONTEXT="$cluster" KUBECTL="$KUBECTL" JQ="$JQ" "$TAINT_SMOKE_SCRIPT"
    fi
    if [[ ${DRA_E2E_BINDING:-0} == 1 ]]; then
        KUBE_CONTEXT="$cluster" KUBECTL="$KUBECTL" JQ="$JQ" "$BINDING_SMOKE_SCRIPT"
    fi

    if [[ "$expected_version" == v1.37.0 && ${DRA_E2E_WORKLOAD:-0} == 1 ]]; then
        KUBE_CONTEXT="$cluster" KUBECTL="$KUBECTL" JQ="$JQ" "$WORKLOAD_SMOKE_SCRIPT"
    fi

    capture_driver_logs "$cluster"
    stop_if_running "$cluster"
    ACTIVE_CLUSTER=
}

require_command "$CONTAINER"
require_command "$KUBECTL"
require_command "$JQ"
test -x "$SMOKE_SCRIPT"
test -x "$EXTENDED_RESOURCE_SMOKE_SCRIPT"
if [[ ${DRA_E2E_TAINTS:-0} == 1 ]]; then
    test -x "$TAINT_SMOKE_SCRIPT"
fi
if [[ ${DRA_E2E_BINDING:-0} == 1 ]]; then
    test -x "$BINDING_SMOKE_SCRIPT"
fi

if [[ ${DRA_E2E_WORKLOAD:-0} == 1 ]]; then
    test -x "$WORKLOAD_SMOKE_SCRIPT"
fi

if [[ "$V136_CLUSTER" == "$V137_CLUSTER" ]]; then
    printf 'validation profiles must have distinct names\n' >&2
    exit 1
fi
assert_native_cluster "$V136_CLUSTER"
assert_native_cluster "$V137_CLUSTER"

# Preflight failures must not mutate cluster state.
trap restore_baseline EXIT

if [[ ${DRA_E2E_SKIP_BUILD:-0} != 1 ]]; then
    "$CONTAINER" build --platform linux/arm64 -f dra/Dockerfile -t "$IMAGE" .
fi

run_profile v1.36.1 "$V136_CLUSTER" "$V137_CLUSTER"
run_profile v1.37.0 "$V137_CLUSTER" "$V136_CLUSTER"
