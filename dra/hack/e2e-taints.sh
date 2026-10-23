#!/usr/bin/env bash
# Opt-in: exercise driver policy and v1.37 operator-owned DeviceTaintRules.
set -euo pipefail
KUBECTL=${KUBECTL:-kubectl}
JQ=${JQ:-jq}
KUBE_CONTEXT=${KUBE_CONTEXT:-}
NAMESPACE=k8s-device-plugin-dra-example
NAME=dra-example-taints
DAEMONSET=daemonset/k8s-device-plugin-dra-example
SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
FIXTURE=$SCRIPT_DIR/e2e-taints.yaml
MODE=driver
RULE_CREATED=0
DRIVER_CHANGED=0

kubectl_run() {
    if [[ -n "$KUBE_CONTEXT" ]]; then
        "$KUBECTL" --context "$KUBE_CONTEXT" --request-timeout=15s "$@"
    else
        "$KUBECTL" --request-timeout=15s "$@"
    fi
}
rollout() {
    kubectl_run -n "$NAMESPACE" rollout status "$DAEMONSET" --timeout=180s
}
clear_driver_taint() {
    kubectl_run -n "$NAMESPACE" set env "$DAEMONSET" DRA_EXAMPLE_TAINT_EFFECT- || return $?
    rollout
}
cleanup() {
    local result=$? cleanup_result=0
    trap - EXIT
    set +e
    kubectl_run delete --ignore-not-found -f "$FIXTURE" --timeout=180s || cleanup_result=$?
    if ((RULE_CREATED)); then
        kubectl_run delete devicetaintrule "$NAME" --ignore-not-found || cleanup_result=$?
    fi
    if ((DRIVER_CHANGED)); then
        clear_driver_taint || cleanup_result=$?
    fi
    rm -rf "$WORK_DIR"
    if ((result == 0)); then result=$cleanup_result; fi
    exit "$result"
}

# Refuse collisions before arming cleanup or changing a policy.
command -v "$KUBECTL" >/dev/null
command -v "$JQ" >/dev/null
test -z "$(kubectl_run get -f "$FIXTURE" --ignore-not-found -o name)"
kubectl_run -n "$NAMESPACE" get "$DAEMONSET" -o json |
    "$JQ" -e 'all(.spec.template.spec.containers[].env[]?; .name != "DRA_EXAMPLE_TAINT_EFFECT")' >/dev/null
SERVER_VERSION=$(kubectl_run version -o json | "$JQ" -er '.serverVersion.gitVersion')
if [[ "$SERVER_VERSION" == v1.37.* ]]; then
    # This cycle validates the stable API, not an implicitly enabled beta API.
    kubectl_run get --raw /apis/resource.k8s.io/v1 |
        "$JQ" -e 'any(.resources[]; .name == "devicetaintrules")' >/dev/null
    test -z "$(kubectl_run get devicetaintrule "$NAME" --ignore-not-found -o name)"
fi
WORK_DIR=$(mktemp -d)
trap cleanup EXIT
kubectl_run create --dry-run=client -f "$FIXTURE" -o json |
    "$JQ" -se -f "$SCRIPT_DIR/without-device-tolerations.jq" > "$WORK_DIR/blocked.json"

slice_policy() {
    kubectl_run get resourceslices --field-selector=spec.driver=dra.example.com -o json |
        "$JQ" -ce '[.items[] | select(any(.spec.devices[]?; .name == "widget-0")) |
            {generation:.spec.pool.generation, taints:[.spec.devices[] | select(.name == "widget-0") | .taints // []]}]'
}
wait_driver_policy() {
    local effect=$1 policy attempt
    for attempt in {1..90}; do
        policy=$(slice_policy)
        if "$JQ" -e --arg effect "$effect" '
            length == 1 and all(.[].taints[]; if $effect == "" then length == 0 else
                any(.[]; .key == "dra.example.com/maintenance" and .effect == $effect and .timeAdded != null) end)
        ' <<< "$policy" >/dev/null; then return 0; fi
        sleep 2
    done
    printf 'driver policy did not converge to %s\n' "$effect" >&2
    return 1
}
set_policy() {
    local effect=$1
    if [[ "$MODE" == driver ]]; then
        DRIVER_CHANGED=1
        kubectl_run -n "$NAMESPACE" set env "$DAEMONSET" "DRA_EXAMPLE_TAINT_EFFECT=$effect"
        rollout
        wait_driver_policy "$effect"
    else
        RULE_CREATED=1
        kubectl_run apply -f - <<RULE
apiVersion: resource.k8s.io/v1
kind: DeviceTaintRule
metadata:
  name: $NAME
spec:
  deviceSelector:
    driver: dra.example.com
    pool: widget-pool
    device: widget-0
  taint:
    key: dra.example.com/maintenance
    effect: $effect
RULE
    fi
}
wait_unprepared() {
    local uid=$1 attempt
    for attempt in {1..90}; do
        if kubectl_run -n "$NAMESPACE" logs "$DAEMONSET" --tail=200 |
            grep -F "$uid" | grep 'unpreparing DRA claim' >/dev/null; then return 0; fi
        sleep 2
    done
    printf 'no unprepare event for claim UID %s\n' "$uid" >&2
    return 1
}
exercise_policy() {
    local claim_uid
    printf '\nTesting %s NoSchedule, toleration, and NoExecute\n' "$MODE"
    set_policy NoSchedule
    if [[ "$MODE" == driver ]]; then
        slice_policy > "$WORK_DIR/policy-before.json"
        # Cross the publisher's 30-second successful reconciliation interval.
        sleep 35
        slice_policy > "$WORK_DIR/policy-after.json"
        cmp "$WORK_DIR/policy-before.json" "$WORK_DIR/policy-after.json"
    fi
    kubectl_run apply -f "$WORK_DIR/blocked.json"
    kubectl_run -n "$NAMESPACE" wait --for=condition=PodScheduled=false "pod/$NAME" --timeout=180s
    kubectl_run -n "$NAMESPACE" get "pod/$NAME" -o json |
        "$JQ" -e 'any(.status.conditions[]; .type == "PodScheduled" and .reason == "Unschedulable")' >/dev/null
    kubectl_run -n "$NAMESPACE" get "resourceclaim/$NAME" -o json |
        "$JQ" -e '.status.allocation == null' >/dev/null
    kubectl_run delete -f "$WORK_DIR/blocked.json" --timeout=180s
    kubectl_run apply -f "$FIXTURE"
    kubectl_run -n "$NAMESPACE" wait --for=condition=Ready "pod/$NAME" --timeout=180s
    kubectl_run -n "$NAMESPACE" logs "pod/$NAME" | grep -F 'DRA_E2E_DEVICE=widget-0'
    claim_uid=$(kubectl_run -n "$NAMESPACE" get "resourceclaim/$NAME" -o json | "$JQ" -er '.metadata.uid')
    set_policy NoExecute
    kubectl_run -n "$NAMESPACE" wait --for=delete "pod/$NAME" --timeout=180s
    wait_unprepared "$claim_uid"
    kubectl_run delete --ignore-not-found -f "$FIXTURE" --timeout=180s
}

exercise_policy
clear_driver_taint
DRIVER_CHANGED=0
wait_driver_policy ""
if [[ "$SERVER_VERSION" == v1.37.* ]]; then
    MODE=administrator
    exercise_policy
else
    printf 'Administrator rule fixture requires v1.37; driver taints passed on %s\n' "$SERVER_VERSION"
fi
printf 'Device taint policy smoke passed on %s\n' "$SERVER_VERSION"
