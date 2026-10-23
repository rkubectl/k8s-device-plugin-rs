#!/usr/bin/env bash
# Explicit one-shot snapshots before, during and after ordinary claim use.
set -euo pipefail
KUBECTL=${KUBECTL:-kubectl}
CARGO=${CARGO:-cargo}
JQ=${JQ:-jq}
KUBE_CONTEXT=${KUBE_CONTEXT:-}
NAMESPACE=k8s-device-plugin-dra-example
NAME=dra-example-pool-status
DAEMONSET=daemonset/k8s-device-plugin-dra-example
FIXTURE=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)/e2e-pool-status.json
CLAIM_UID=
kubectl_run() {
    if [[ -n "$KUBE_CONTEXT" ]]; then
        "$KUBECTL" --context "$KUBE_CONTEXT" --request-timeout=15s "$@"
    else
        "$KUBECTL" --request-timeout=15s "$@"
    fi
}
cleanup() {
    local result=$? cleanup_result=0
    trap - EXIT
    set +e
    kubectl_run delete --ignore-not-found -f "$FIXTURE" --timeout=180s || cleanup_result=$?
    rm -rf "$WORK_DIR"
    if ((result == 0)); then result=$cleanup_result; fi
    exit "$result"
}
snapshot() {
    local phase=$1 allocated=$2 available=$3 result deadline request
    deadline=$((SECONDS + 120))
    while ((SECONDS < deadline)); do
        result=$(DRIVER_NAME=dra.example.com POOL_NAME=widget-pool KUBE_CONTEXT="$KUBE_CONTEXT" \
            "$CARGO" run --manifest-path "$CARGO_MANIFEST" --locked --quiet \
                -p k8s-device-plugin-dra --example pool_status 2> "$WORK_DIR/query.log") || {
                    cat "$WORK_DIR/query.log" >&2; return 1;
                }
        cat "$WORK_DIR/query.log" >&2
        request=$(sed -n 's/^created pool-status request //p' "$WORK_DIR/query.log")
        test -n "$request"
        test -z "$(kubectl_run get "resourcepoolstatusrequest/$request" --ignore-not-found -o name)"
        if "$JQ" -e --argjson allocated "$allocated" --argjson available "$available" '
            .poolCount == 1 and (.pools | length) == 1 and
            all(.pools[]; .driver == "dra.example.com" and .poolName == "widget-pool" and
                .validationError == null and .resourceSliceCount == 1 and .totalDevices == 1 and
                .allocatedDevices == $allocated and .availableDevices == $available and .unavailableDevices == 0)
        ' <<< "$result" >/dev/null; then
            printf '%s: %s\n' "$phase" "$result"
            return 0
        fi
        sleep 2
    done
    printf 'pool snapshot did not converge for %s: %s\n' "$phase" "$result" >&2
    return 1
}
for command in "$KUBECTL" "$CARGO" "$JQ"; do command -v "$command" >/dev/null; done
CARGO_MANIFEST=$("$CARGO" locate-project --workspace --message-format plain)
test -f "$CARGO_MANIFEST"
kubectl_run get --raw /apis/resource.k8s.io/v1alpha3 |
    "$JQ" -e 'any(.resources[]; .name == "resourcepoolstatusrequests")' >/dev/null
test -z "$(kubectl_run get -f "$FIXTURE" --ignore-not-found -o name)"
kubectl_run -n "$NAMESPACE" get "$DAEMONSET" -o json |
    "$JQ" -e 'all(.spec.template.spec.containers[].env[]?;
        .name != "DRA_EXAMPLE_BINDING_CONDITIONS" and .name != "DRA_EXAMPLE_TAINT_EFFECT")' >/dev/null
WORK_DIR=$(mktemp -d)
trap cleanup EXIT
snapshot before 0 1
kubectl_run apply -f "$FIXTURE"
CLAIM_UID=$(kubectl_run -n "$NAMESPACE" get "resourceclaim/$NAME" -o json | "$JQ" -er '.metadata.uid')
kubectl_run -n "$NAMESPACE" wait --for=condition=Ready "pod/$NAME" --timeout=180s
kubectl_run -n "$NAMESPACE" logs "pod/$NAME" | grep -F 'DRA_E2E_DEVICE=widget-0'
snapshot allocated 1 0
kubectl_run -n "$NAMESPACE" delete "pod/$NAME" --wait=true --timeout=180s
for attempt in {1..90}; do
    logs=$(kubectl_run -n "$NAMESPACE" logs "$DAEMONSET" --tail=200)
    if grep -F "$CLAIM_UID" <<< "$logs" | grep 'unpreparing DRA claim' >/dev/null; then break; fi
    sleep 2
done
grep -F "$CLAIM_UID" <<< "$logs" | grep 'unpreparing DRA claim'
# Allocation persists on an explicitly authored claim until that claim is deleted.
kubectl_run -n "$NAMESPACE" delete "resourceclaim/$NAME" --wait=true --timeout=180s
snapshot released 0 1
printf 'Resource-pool status snapshot smoke passed; every query request was deleted\n'
