#!/usr/bin/env bash
# Opt-in v1.37 gated fixture: one generated PodGroup claim shared by two Pods.
set -euo pipefail
KUBECTL=${KUBECTL:-kubectl}
JQ=${JQ:-jq}
KUBE_CONTEXT=${KUBE_CONTEXT:-}
NAMESPACE=k8s-device-plugin-dra-example
NAME=dra-example-workload
DAEMONSET=daemonset/k8s-device-plugin-dra-example
FIXTURE=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)/e2e-workload.json
CLAIM_NAME=
CLAIM_UID=
GROUP_UID=

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
    if [[ -n "$CLAIM_NAME" ]]; then
        kubectl_run -n "$NAMESPACE" wait --for=delete "resourceclaim/$CLAIM_NAME" --timeout=180s || cleanup_result=$?
    fi
    if ((result == 0)); then result=$cleanup_result; fi
    exit "$result"
}
claim_json() { kubectl_run -n "$NAMESPACE" get "resourceclaim/$CLAIM_NAME" -o json; }
assert_metadata() {
    local pod=$1 object base
    object=$(claim_json)
    base=$("$JQ" -er '.metadata.annotations["resource.kubernetes.io/pod-claim-name"] as $local |
        if ($local // "") != "" then "resourceclaimtemplates/" + $local else "resourceclaims/" + .metadata.name end' <<< "$object")
    kubectl_run -n "$NAMESPACE" exec "$pod" -- cat "/var/run/kubernetes.io/dra-device-attributes/$base/widget/dra.example.com-metadata.json" |
        "$JQ" -e --arg uid "$CLAIM_UID" '.metadata.uid == $uid and .requests[0].name == "widget" and
            all(.requests[0].devices[]; .attributes.model.string == "validation-widget")' >/dev/null
}
assert_group_reservation() {
    claim_json | "$JQ" -e --arg uid "$CLAIM_UID" --arg group_uid "$GROUP_UID" '
        .metadata.uid == $uid and
        any(.metadata.ownerReferences[]?; .kind == "PodGroup" and .uid == $group_uid and .controller == true) and
        (.status.reservedFor | length) == 1 and
        any(.status.reservedFor[]; .resource == "podgroups" and .apiGroup == "scheduling.k8s.io" and .uid == $group_uid) and
        any(.status.devices[]?; .driver == "dra.example.com" and .data.phase == "prepared")
    ' >/dev/null
}
wait_unprepared() {
    local logs attempt
    for attempt in {1..90}; do
        logs=$(kubectl_run -n "$NAMESPACE" logs "$DAEMONSET" --tail=200)
        if grep -F "$CLAIM_UID" <<< "$logs" | grep 'unpreparing DRA claim' >/dev/null; then
            grep -F "$CLAIM_UID" <<< "$logs"
            return 0
        fi
        sleep 2
    done
    printf 'no unprepare for the shared claim UID %s\n' "$CLAIM_UID" >&2
    return 1
}

command -v "$KUBECTL" >/dev/null
command -v "$JQ" >/dev/null
if [[ ${DRA_E2E_WORKLOAD_METADATA:-0} == 1 ]]; then command -v "${CONTAINER:-container}" >/dev/null; fi
kubectl_run get --raw /apis/scheduling.k8s.io/v1beta1 |
    "$JQ" -e 'any(.resources[]; .name == "podgroups") and any(.resources[]; .name == "workloads")' >/dev/null
test -z "$(kubectl_run get -f "$FIXTURE" --ignore-not-found -o name)"
kubectl_run -n "$NAMESPACE" get "$DAEMONSET" -o json |
    "$JQ" -e 'all(.spec.template.spec.containers[].env[]?;
        .name != "DRA_EXAMPLE_BINDING_CONDITIONS" and .name != "DRA_EXAMPLE_TAINT_EFFECT")' >/dev/null
trap cleanup EXIT
kubectl_run apply -f "$FIXTURE"
GROUP_UID=$(kubectl_run -n "$NAMESPACE" get "podgroup/$NAME" -o json | "$JQ" -er '.metadata.uid')
for attempt in {1..90}; do
    group=$(kubectl_run -n "$NAMESPACE" get "podgroup/$NAME" -o json)
    CLAIM_NAME=$("$JQ" -r '[.status.resourceClaimStatuses[]? | select(.name == "widget") | .resourceClaimName][0] // empty' <<< "$group")
    if [[ -n "$CLAIM_NAME" ]]; then break; fi
    sleep 2
done
test -n "$CLAIM_NAME"
CLAIM_UID=$(claim_json | "$JQ" -er '.metadata.uid')
printf 'PodGroup UID %s generated shared claim %s (UID %s)\n' "$GROUP_UID" "$CLAIM_NAME" "$CLAIM_UID"
for index in 1 2; do
    kubectl_run -n "$NAMESPACE" wait --for=condition=Ready "pod/${NAME}-${index}" --timeout=180s
    kubectl_run -n "$NAMESPACE" logs "pod/${NAME}-${index}" | grep -F 'DRA_E2E_DEVICE=widget-0'
    if [[ ${DRA_E2E_WORKLOAD_METADATA:-0} == 1 ]]; then assert_metadata "${NAME}-${index}"; fi
    kubectl_run -n "$NAMESPACE" get "pod/${NAME}-${index}" -o json |
        "$JQ" -e --arg claim "$CLAIM_NAME" 'any(.status.resourceClaimStatuses[]?;
            .name == "widget" and .resourceClaimName == $claim)' >/dev/null
done
assert_group_reservation
kubectl_run -n "$NAMESPACE" delete "pod/${NAME}-1" --wait=true --timeout=180s
# Observe the remaining consumer over several kubelet cleanup opportunities.
for attempt in {1..5}; do
    kubectl_run -n "$NAMESPACE" wait --for=condition=Ready "pod/${NAME}-2" --timeout=30s
    assert_group_reservation
    logs=$(kubectl_run -n "$NAMESPACE" logs "$DAEMONSET" --tail=200)
    if grep -F "$CLAIM_UID" <<< "$logs" | grep 'unpreparing DRA claim' >/dev/null; then
        printf 'shared claim was unprepared while its second consumer remained\n' >&2
        exit 1
    fi
    sleep 2
done
kubectl_run -n "$NAMESPACE" exec "pod/${NAME}-2" -- sh -ec 'test "$DRA_E2E_DEVICE" = "widget-0"'
if [[ ${DRA_E2E_WORKLOAD_METADATA:-0} == 1 ]]; then assert_metadata "${NAME}-2"; fi
kubectl_run -n "$NAMESPACE" delete "pod/${NAME}-2" --wait=true --timeout=180s
wait_unprepared
if [[ ${DRA_E2E_WORKLOAD_METADATA:-0} == 1 ]]; then
    for attempt in {1..60}; do
        if "${CONTAINER:-container}" exec "$KUBE_CONTEXT" test ! -e "/var/lib/kubelet/plugins/dra.example.com/dra-device-metadata/$CLAIM_UID"; then break; fi
        sleep 2
    done
    "${CONTAINER:-container}" exec "$KUBE_CONTEXT" test ! -e "/var/lib/kubelet/plugins/dra.example.com/dra-device-metadata/$CLAIM_UID"
fi
kubectl_run -n "$NAMESPACE" delete "podgroup/$NAME" --wait=true --timeout=180s
kubectl_run -n "$NAMESPACE" wait --for=delete "resourceclaim/$CLAIM_NAME" --timeout=180s
printf 'Workload ResourceClaims shared lifecycle smoke passed\n'
