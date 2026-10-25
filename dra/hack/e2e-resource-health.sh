#!/usr/bin/env bash
# A real kubelet must negotiate health, open a watch and publish device health.
# Requires the validation image built with CARGO_FEATURES=resource-health.
set -euo pipefail
KUBECTL=${KUBECTL:-kubectl}
KUBE_CONTEXT=${KUBE_CONTEXT:?select a native validation context}
JQ=${JQ:-jq}
SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
NAMESPACE=k8s-device-plugin-dra-example
DS=daemonset/k8s-device-plugin-dra-example
FIXTURE=$SCRIPT_DIR/e2e-first-available.yaml
CONSUMER=dra-example-first-available-consumer
k() { "$KUBECTL" --context "$KUBE_CONTEXT" --request-timeout=15s "$@"; }
# The script owns only this opt-in env var. Refuse an existing override so
# cleanup can restore the absent/default state exactly.
k -n "$NAMESPACE" get "$DS" -o json |
    "$JQ" -e 'all(.spec.template.spec.containers[].env[]; .name != "DRA_EXAMPLE_RESOURCE_HEALTH" and .name != "DRA_EXAMPLE_DEVICE_METADATA")' >/dev/null
cleanup() {
    local result=$? cleanup_result=0
    trap - EXIT
    set +e
    k delete --ignore-not-found -f "$FIXTURE" --timeout=180s >/dev/null || cleanup_result=$?
    k -n "$NAMESPACE" set env "$DS" DRA_EXAMPLE_RESOURCE_HEALTH- >/dev/null || cleanup_result=$?
    k -n "$NAMESPACE" rollout status "$DS" --timeout=180s || cleanup_result=$?
    if ((result == 0)); then result=$cleanup_result; fi
    exit "$result"
}
trap cleanup EXIT
k -n "$NAMESPACE" set env "$DS" DRA_EXAMPLE_RESOURCE_HEALTH=1
k -n "$NAMESPACE" rollout status "$DS" --timeout=180s
k apply -f "$FIXTURE"
k -n "$NAMESPACE" wait --for=condition=Ready "pod/$CONSUMER" --timeout=180s
k -n "$NAMESPACE" logs "pod/$CONSUMER" | grep -Fx 'DRA_E2E_DEVICE=widget-0'
for attempt in {1..60}; do
    if k -n "$NAMESPACE" get "pod/$CONSUMER" -o json |
        "$JQ" -e 'any(.status.containerStatuses[]?.allocatedResourcesStatus[]?.resources[]?; .health == "Healthy")' >/dev/null; then
        k -n "$NAMESPACE" logs "$DS" --tail=100 | grep -F 'kubelet opened resource health watch'
        printf 'kubelet health negotiation, watch and Pod health passed\n'
        exit 0
    fi
    sleep 2
done
printf 'kubelet did not publish Healthy resource status\n' >&2
exit 1
