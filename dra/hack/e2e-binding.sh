#!/usr/bin/env bash
# Opt-in: publish readiness before binding, then prove CDI and failure recovery.
set -euo pipefail
KUBECTL=${KUBECTL:-kubectl}
CARGO=${CARGO:-cargo}
JQ=${JQ:-jq}
KUBE_CONTEXT=${KUBE_CONTEXT:-}
NAMESPACE=k8s-device-plugin-dra-example
NAME=dra-example-binding
DAEMONSET=daemonset/k8s-device-plugin-dra-example
SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
FIXTURE=$SCRIPT_DIR/e2e-binding.yaml
DRIVER_CHANGED=0
CLAIM_UID=
POD_UID=

kubectl_run() {
    if [[ -n "$KUBE_CONTEXT" ]]; then
        "$KUBECTL" --context "$KUBE_CONTEXT" --request-timeout=15s "$@"
    else
        "$KUBECTL" --request-timeout=15s "$@"
    fi
}
rollout() { kubectl_run -n "$NAMESPACE" rollout status "$DAEMONSET" --timeout=180s; }
clear_binding() {
    kubectl_run -n "$NAMESPACE" set env "$DAEMONSET" DRA_EXAMPLE_BINDING_CONDITIONS- || return $?
    rollout || return $?
    wait_inventory_binding false
}
cleanup() {
    local result=$? cleanup_result=0
    trap - EXIT
    set +e
    kubectl_run delete --ignore-not-found -f "$FIXTURE" --timeout=180s || cleanup_result=$?
    if ((DRIVER_CHANGED)); then clear_binding || cleanup_result=$?; fi
    rm -rf "$WORK_DIR"
    if ((result == 0)); then result=$cleanup_result; fi
    exit "$result"
}
report() {
    local state=$1 uid=${2:-$CLAIM_UID}
    CLAIM_NAMESPACE="$NAMESPACE" CLAIM_NAME="$NAME" CLAIM_UID="$uid" \
        BINDING_STATE="$state" DRIVER_NAME=dra.example.com KUBE_CONTEXT="$KUBE_CONTEXT" \
        "$CARGO" run --manifest-path "$CARGO_MANIFEST" --locked --quiet \
            -p k8s-device-plugin-dra --example binding_controller
}
wait_inventory_binding() {
    local enabled=$1 inventory attempt
    for attempt in {1..90}; do
        inventory=$(kubectl_run get resourceslices --field-selector=spec.driver=dra.example.com -o json)
        if "$JQ" -e --argjson enabled "$enabled" '
            [.items[].spec.devices[]? | select(.name == "widget-0")] as $devices |
            ($devices | length) == 1 and all($devices[];
                (.taints // [] | length) == 0 and
                if $enabled then (.bindsToNode == true and
                    (.bindingConditions // [] | index("dra.example.com/Attached")) != null and
                    (.bindingFailureConditions // [] | index("dra.example.com/AttachFailed")) != null)
                else ((.bindingConditions // [] | length) == 0 and
                    (.bindingFailureConditions // [] | length) == 0 and (.bindsToNode // false) == false) end)
        ' <<< "$inventory" >/dev/null; then return 0; fi
        sleep 2
    done
    printf 'binding inventory did not converge (enabled=%s)\n' "$enabled" >&2
    return 1
}
wait_allocated() {
    local claim attempt
    for attempt in {1..90}; do
        claim=$(kubectl_run -n "$NAMESPACE" get "resourceclaim/$NAME" -o json)
        if "$JQ" -e 'any(.status.allocation.devices.results[]?;
            .driver == "dra.example.com" and
            (.bindingConditions // [] | index("dra.example.com/Attached")) != null)' <<< "$claim" >/dev/null; then
            "$JQ" -e '.status.allocation.nodeSelector != null' <<< "$claim" >/dev/null
            CLAIM_UID=$("$JQ" -er '.metadata.uid' <<< "$claim")
            POD_UID=$(kubectl_run -n "$NAMESPACE" get "pod/$NAME" -o json | "$JQ" -er '.metadata.uid')
            return 0
        fi
        sleep 2
    done
    printf 'claim did not allocate with binding metadata\n' >&2
    return 1
}
assert_unbound() {
    kubectl_run -n "$NAMESPACE" get "pod/$NAME" -o json |
        "$JQ" -e '(.spec.nodeName // "") == "" and .status.phase != "Running"' >/dev/null
}
wait_unprepared() {
    local attempt
    for attempt in {1..90}; do
        if kubectl_run -n "$NAMESPACE" logs "$DAEMONSET" --tail=200 |
            grep -F "$CLAIM_UID" | grep 'unpreparing DRA claim' >/dev/null; then return 0; fi
        sleep 2
    done
    printf 'no unprepare for binding fixture claim UID %s\n' "$CLAIM_UID" >&2
    return 1
}
wait_binding_failure() {
    local events attempt logs
    for attempt in {1..90}; do
        events=$(kubectl_run -n "$NAMESPACE" get events --field-selector="involvedObject.uid=$POD_UID" -o json)
        if "$JQ" -e 'any(.items[]; .reason == "FailedScheduling" and (.message | test("binding"; "i")))' \
            <<< "$events" >/dev/null; then
            assert_unbound
            logs=$(kubectl_run -n "$NAMESPACE" logs "$DAEMONSET" --tail=200)
            if grep -F "$CLAIM_UID" <<< "$logs" | grep -F 'node_prepare_resources' >/dev/null; then
                printf 'failed binding unexpectedly reached kubelet prepare\n' >&2
                return 1
            fi
            return 0
        fi
        sleep 2
    done
    printf 'scheduler did not report the binding failure\n' >&2
    return 1
}

command -v "$KUBECTL" >/dev/null
command -v "$CARGO" >/dev/null
command -v "$JQ" >/dev/null
CARGO_MANIFEST=$("$CARGO" locate-project --workspace --message-format plain)
test -f "$CARGO_MANIFEST"
test -z "$(kubectl_run get -f "$FIXTURE" --ignore-not-found -o name)"
kubectl_run -n "$NAMESPACE" get "$DAEMONSET" -o json |
    "$JQ" -e 'all(.spec.template.spec.containers[].env[]?;
        .name != "DRA_EXAMPLE_BINDING_CONDITIONS" and .name != "DRA_EXAMPLE_TAINT_EFFECT")' >/dev/null
WORK_DIR=$(mktemp -d)
trap cleanup EXIT
DRIVER_CHANGED=1
kubectl_run -n "$NAMESPACE" set env "$DAEMONSET" DRA_EXAMPLE_BINDING_CONDITIONS=1
rollout
wait_inventory_binding true

printf '\nTesting binding readiness before kubelet prepare\n'
kubectl_run apply -f "$FIXTURE"
wait_allocated
report pending
assert_unbound
if report ready "${CLAIM_UID}-stale" > "$WORK_DIR/stale.log" 2>&1; then
    printf 'controller accepted a stale claim UID\n' >&2
    exit 1
fi
grep -F 'claim UID changed' "$WORK_DIR/stale.log" >/dev/null
# Cross the scheduler's five-second binding poll while readiness is False.
sleep 6
assert_unbound
report ready
kubectl_run -n "$NAMESPACE" wait --for=condition=Ready "pod/$NAME" --timeout=180s
kubectl_run -n "$NAMESPACE" logs "pod/$NAME" | grep -F 'DRA_E2E_DEVICE=widget-0'
kubectl_run -n "$NAMESPACE" get "resourceclaim/$NAME" -o json |
    "$JQ" -e 'any(.status.devices[]?; .driver == "dra.example.com" and .data.phase == "prepared" and
        any(.conditions[]?; .type == "dra.example.com/Attached" and .status == "True"))' >/dev/null
report ready | grep -F 'Unchanged'
kubectl_run delete --ignore-not-found -f "$FIXTURE" --timeout=180s
wait_unprepared

printf '\nTesting explicit binding failure and scheduler recovery\n'
kubectl_run apply -f "$FIXTURE"
wait_allocated
report failed
wait_binding_failure
printf 'Device binding condition smoke passed\n'
