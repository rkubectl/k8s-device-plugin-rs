#!/usr/bin/env bash
# Native opt-in identity/attribute, isolation, restart and writer-retry acceptance.
set -euo pipefail
KUBECTL=${KUBECTL:-kubectl}
CONTAINER=${CONTAINER:-container}
JQ=${JQ:-jq}
KUBE_CONTEXT=${KUBE_CONTEXT:?select the native validation context}
NAMESPACE=k8s-device-plugin-dra-example
NAME=dra-example-metadata
DS=daemonset/k8s-device-plugin-dra-example
SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
SOURCE=$SCRIPT_DIR/e2e-metadata.json
CHANGED=0
CLAIM_UID=
CLAIM_NAME=
OWNED_COLLISION=
OWNED_UIDS=
k() { "$KUBECTL" --context "$KUBE_CONTEXT" --request-timeout=15s "$@"; }
node() { "$CONTAINER" exec "$KUBE_CONTEXT" "$@"; }
rollout() { k -n "$NAMESPACE" rollout status "$DS" --timeout=180s; }
configure() {
    k -n "$NAMESPACE" set env "$DS" DRA_EXAMPLE_DEVICE_METADATA=1 "DEVICE_NAMES=$1"
    rollout
    for attempt in {1..60}; do
        inventory=$(k get resourceslices --field-selector=spec.driver=dra.example.com -o json)
        if "$JQ" -e --argjson count "$2" '[.items[].spec.devices[]?] | length == $count and all(.[]; .attributes.model.string == "validation-widget")' <<< "$inventory" >/dev/null; then return 0; fi
        sleep 2
    done
    return 1
}
cleanup() {
    local result=$? cleanup_result=0
    trap - EXIT
    set +e
    if [[ -n "$OWNED_COLLISION" ]]; then node rm -f -- "$OWNED_COLLISION" || cleanup_rc=$?; fi
    # These names were proven absent before mutation.
    for file in "$WORK_DIR"/*.json; do
        [[ -f "$file" ]] || continue
        k delete --ignore-not-found -f "$file" --timeout=180s >/dev/null || cleanup_result=$?
    done
    if [[ -n "$OWNED_COLLISION" ]]; then node rm -f -- "$OWNED_COLLISION" || cleanup_result=$?; fi
    if ((CHANGED)); then
        k -n "$NAMESPACE" set env "$DS" DRA_EXAMPLE_DEVICE_METADATA- DEVICE_NAMES=widget-0 || cleanup_result=$?
        rollout || cleanup_result=$?
    fi
    rm -rf "$WORK_DIR"
    if ((result==0)); then result=$cleanup_result; fi
    exit "$result"
}
claim_json() { k -n "$NAMESPACE" get "resourceclaim/$CLAIM_NAME" -o json; }
wait_unprepared() {
    for attempt in {1..90}; do
        logs=$(k -n "$NAMESPACE" logs "$DS" --tail=250)
        if grep -F "$CLAIM_UID" <<< "$logs" | grep 'unpreparing DRA claim' >/dev/null; then return 0; fi
        sleep 2
    done
    return 1
}
assert_clean() {
    for attempt in {1..60}; do
        if node /bin/sh -ec 'test ! -e "/var/lib/kubelet/plugins/dra.example.com/dra-device-metadata/$1"; test -z "$(find /var/run/cdi -maxdepth 1 -name "dra.example.com_metadata_${1}_*.json" -print)"' -- "$CLAIM_UID"; then return 0; fi
        sleep 2
    done
    return 1
}
inspect_payloads() {
    local base=$1 pod=$2 request expected object
    object=$(claim_json)
    for request in a b; do
        expected=$("$JQ" -r --arg request "$request" '[.status.allocation.devices.results[] | select((.request | split("/")[0]) == $request)][0].request' <<< "$object")
        k -n "$NAMESPACE" exec "$pod" -c "$request" -- cat "$base/$request/dra.example.com-metadata.json" |
            "$JQ" -e --arg uid "$CLAIM_UID" --arg name "$CLAIM_NAME" --arg request "$expected" '
                .apiVersion == "metadata.resource.k8s.io/v1beta1" and .kind == "DeviceMetadata" and
                .metadata.uid == $uid and .metadata.name == $name and .metadata.generation == 1 and
                .requests[0].name == $request and (.requests[0].devices | length) == 1 and
                all(.requests[0].devices[]; .attributes.model.string == "validation-widget")' >/dev/null
    done
}
case_run() {
    local mode=$1 fixture=$WORK_DIR/$1.json base=/var/run/kubernetes.io/dra-device-attributes
    case "$mode" in
        direct|reused) cp "$SOURCE" "$fixture"; base=$base/resourceclaims/$NAME ;;
        template)
            "$JQ" '.items |= map(if .kind == "ResourceClaim" then .kind="ResourceClaimTemplate" | .spec={spec:.spec} elif .kind == "Pod" then .spec.resourceClaims=[{name:"widgets",resourceClaimTemplateName:"dra-example-metadata"}] | (.spec.containers[].env[] | select(.name=="DRA_METADATA_BASE").value)="/var/run/kubernetes.io/dra-device-attributes/resourceclaimtemplates/widgets" else . end)' "$SOURCE" > "$fixture"
            base=$base/resourceclaimtemplates/widgets ;;
        first-available)
            "$JQ" '(.items[] | select(.kind=="ResourceClaim").spec.devices.requests[0])={name:"a",firstAvailable:[{name:"fast",deviceClassName:"dra-example-metadata"}]}' "$SOURCE" > "$fixture"
            base=$base/resourceclaims/$NAME ;;
    esac
    k apply -f "$fixture"
    k -n "$NAMESPACE" wait --for=condition=Ready "pod/$NAME" --timeout=180s
    CLAIM_NAME=$NAME
    if [[ "$mode" == template ]]; then
        CLAIM_NAME=$(k -n "$NAMESPACE" get "pod/$NAME" -o json | "$JQ" -er '.status.resourceClaimStatuses[] | select(.name=="widgets").resourceClaimName')
    fi
    CLAIM_UID=$(claim_json | "$JQ" -er '.metadata.uid')
    OWNED_UIDS="$OWNED_UIDS $CLAIM_UID"
    for c in a b all none; do k -n "$NAMESPACE" logs "$NAME" -c "$c" | grep "metadata $c passed"; done
    inspect_payloads "$base" "$NAME"
    if [[ "$mode" == direct ]]; then
        before=$(node stat -c '%i' "/var/lib/kubelet/plugins/dra.example.com/dra-device-metadata/$CLAIM_UID/a/metadata.json")
        k -n "$NAMESPACE" rollout restart "$DS"; rollout
        inspect_payloads "$base" "$NAME"
        after=$(node stat -c '%i' "/var/lib/kubelet/plugins/dra.example.com/dra-device-metadata/$CLAIM_UID/a/metadata.json")
        test "$before" = "$after"
        OLD_UID=$CLAIM_UID
    elif [[ "$mode" == reused ]]; then
        test "$CLAIM_UID" != "$OLD_UID"
    fi
    k -n "$NAMESPACE" delete "pod/$NAME" --wait=true --timeout=180s
    wait_unprepared
    assert_clean
    k delete --ignore-not-found -f "$fixture" --timeout=180s >/dev/null
    rm "$fixture"
    printf 'metadata %s lifecycle passed (claim UID %s)\n' "$mode" "$CLAIM_UID"
}

for command in "$KUBECTL" "$CONTAINER" "$JQ"; do command -v "$command" >/dev/null; done
"$CONTAINER" list --all --format json | "$JQ" -e --arg name "$KUBE_CONTEXT" '[.[] | select(.id==$name)] | length==1 and .[0].status.state=="running" and .[0].configuration.rosetta==false' >/dev/null
test -z "$(k get -f "$SOURCE" --ignore-not-found -o name)"
test -z "$(k -n "$NAMESPACE" get pod,resourceclaim -o name | grep '/dra-example-metadata-peer$' || true)"
k -n "$NAMESPACE" get "$DS" -o json | "$JQ" -e 'all(.spec.template.spec.containers[].env[]?; .name != "DRA_EXAMPLE_DEVICE_METADATA" and .name != "DRA_EXAMPLE_TAINT_EFFECT" and .name != "DRA_EXAMPLE_BINDING_CONDITIONS")' >/dev/null
if [[ ${DRA_E2E_METADATA_SHARED:-0} == 1 ]]; then k get --raw /apis/scheduling.k8s.io/v1beta1 | "$JQ" -e 'any(.resources[]; .name=="podgroups")' >/dev/null; fi
WORK_DIR=$(mktemp -d)
trap cleanup EXIT
CHANGED=1
configure widget-0,widget-1 2
case_run direct
case_run reused
case_run template
case_run first-available
# Live writer-failure/retry acceptance, with a second independently prepared claim.
configure widget-0,widget-1,widget-2 3
"$JQ" '.items |= map(if .kind=="ResourceClaim" then .spec.devices.requests |= [.[0]] elif .kind=="Pod" then .spec.containers |= map(select(.name=="a")) else . end)' "$SOURCE" > "$WORK_DIR/fault.json"
"$JQ" '.items |= map(if .kind=="Pod" then empty else . end)' "$WORK_DIR/fault.json" > "$WORK_DIR/fault-claim.json"
k apply -f "$WORK_DIR/fault-claim.json"
CLAIM_NAME=$NAME
CLAIM_UID=$(claim_json | "$JQ" -er '.metadata.uid')
OWNED_UIDS="$OWNED_UIDS $CLAIM_UID"
collision_path=/var/run/cdi/dra.example.com_metadata_${CLAIM_UID}_a.json
node /bin/sh -ec 'test ! -e "$1"; printf "%s" "foreign collision for acceptance" > "$1"' -- "$collision_path"
OWNED_COLLISION=$collision_path
k apply -f "$WORK_DIR/fault.json"
POD_UID=$(k -n "$NAMESPACE" get "pod/$NAME" -o jsonpath='{.metadata.uid}')
for attempt in {1..60}; do
    events=$(k -n "$NAMESPACE" get events --field-selector="involvedObject.uid=$POD_UID" -o json)
    if "$JQ" -e 'any(.items[]; .message | contains("container metadata"))' <<< "$events" >/dev/null; then break; fi
    sleep 2
done
"$JQ" -e 'any(.items[]; .message | contains("container metadata"))' <<< "$events" >/dev/null
k -n "$NAMESPACE" get "pod/$NAME" -o json | "$JQ" -e 'all(.status.conditions[]? | select(.type=="Ready"); .status != "True")' >/dev/null
before=$(node stat -c '%i' "/var/lib/kubelet/plugins/dra.example.com/dra-device-metadata/$CLAIM_UID/a/metadata.json")
"$JQ" '.items |= map(if .kind=="DeviceClass" then empty elif .kind=="ResourceClaim" then .metadata.name="dra-example-metadata-peer" elif .kind=="Pod" then .metadata.name="dra-example-metadata-peer" | .spec.resourceClaims[0].resourceClaimName="dra-example-metadata-peer" | (.spec.containers[].env[]|select(.name=="DRA_METADATA_BASE").value)="/var/run/kubernetes.io/dra-device-attributes/resourceclaims/dra-example-metadata-peer" else . end)' "$WORK_DIR/fault.json" > "$WORK_DIR/peer.json"
k apply -f "$WORK_DIR/peer.json"
k -n "$NAMESPACE" wait --for=condition=Ready pod/dra-example-metadata-peer --timeout=180s
PEER_UID=$(k -n "$NAMESPACE" get resourceclaim/dra-example-metadata-peer -o jsonpath='{.metadata.uid}')
OWNED_UIDS="$OWNED_UIDS $PEER_UID"
node rm -- "$OWNED_COLLISION"
OWNED_COLLISION=
k -n "$NAMESPACE" wait --for=condition=Ready "pod/$NAME" --timeout=180s
k -n "$NAMESPACE" wait --for=condition=Ready pod/dra-example-metadata-peer --timeout=30s
after=$(node stat -c '%i' "/var/lib/kubelet/plugins/dra.example.com/dra-device-metadata/$CLAIM_UID/a/metadata.json")
test "$before" = "$after"
k delete -f "$WORK_DIR/peer.json" --timeout=180s >/dev/null
rm "$WORK_DIR/peer.json"
k -n "$NAMESPACE" delete "pod/$NAME" --wait=true --timeout=180s
wait_unprepared; assert_clean
k delete --ignore-not-found -f "$WORK_DIR/fault.json" --timeout=180s >/dev/null
rm "$WORK_DIR/fault.json" "$WORK_DIR/fault-claim.json"
printf 'metadata writer failure, independent claim and retry passed\n'
if [[ ${DRA_E2E_METADATA_SHARED:-0} == 1 ]]; then
    configure widget-0 1
    DRA_E2E_WORKLOAD_METADATA=1 KUBE_CONTEXT="$KUBE_CONTEXT" KUBECTL="$KUBECTL" CONTAINER="$CONTAINER" "$SCRIPT_DIR/e2e-workload.sh"
fi
printf 'Container metadata native acceptance passed on %s\n' "$KUBE_CONTEXT"
