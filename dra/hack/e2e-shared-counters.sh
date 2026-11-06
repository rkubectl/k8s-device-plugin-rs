#!/usr/bin/env bash
# Opt-in scheduler/CDI acceptance for the fake card/pair shared-counter pool.
set -euo pipefail
KUBECTL=${KUBECTL:-kubectl}
JQ=${JQ:-jq}
KUBE_CONTEXT=${KUBE_CONTEXT:-}
NAMESPACE=dra-shared-counters-e2e
DRIVER_NAMESPACE=k8s-device-plugin-dra-example
DAEMONSET=daemonset/k8s-device-plugin-dra-example
CHANGED=0
CREATED=0
CREATED_CLASSES=()
WORK_DIR=$(mktemp -d)

kubectl_run() {
    if [[ -n "$KUBE_CONTEXT" ]]; then
        "$KUBECTL" --context "$KUBE_CONTEXT" --request-timeout=15s "$@"
    else
        "$KUBECTL" --request-timeout=15s "$@"
    fi
}
rollout() {
    kubectl_run -n "$DRIVER_NAMESPACE" rollout status "$DAEMONSET" --timeout=180s
}
cleanup() {
    local result=$? cleanup_result=0
    trap - EXIT
    set +e
    if ((CREATED)); then
        kubectl_run delete namespace "$NAMESPACE" --wait=true --timeout=180s || cleanup_result=$?
    fi
    for class in "${CREATED_CLASSES[@]}"; do
        kubectl_run delete deviceclass "$class" --ignore-not-found || cleanup_result=$?
    done
    if ((CHANGED)) && ((cleanup_result == 0)); then
        kubectl_run -n "$DRIVER_NAMESPACE" set env "$DAEMONSET" DRA_EXAMPLE_SHARED_COUNTERS- && rollout || cleanup_result=$?
    fi
    rm -rf "$WORK_DIR"
    if ((result == 0)); then result=$cleanup_result; fi
    exit "$result"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

# Refuse existing objects/configuration and active allocations before restarting.
test -z "$(kubectl_run get namespace "$NAMESPACE" --ignore-not-found -o name)"
test -z "$(kubectl_run get deviceclass "$NAMESPACE-card" "$NAMESPACE-pair" --ignore-not-found -o name)"
kubectl_run get nodes -o json | "$JQ" -e '.items | length == 1' >/dev/null
kubectl_run get resourceclaims -A -o json | "$JQ" -e '
    all(.items[]; all(.status.allocation.devices.results[]?; .driver != "dra.example.com"))' >/dev/null
kubectl_run -n "$DRIVER_NAMESPACE" get "$DAEMONSET" -o json | "$JQ" -e '
    all(.spec.template.spec.containers[].env[]?;
        .name != "DRA_EXAMPLE_SHARED_COUNTERS" and .name != "DRA_EXAMPLE_TAINT_EFFECT" and
        .name != "DRA_EXAMPLE_BINDING_CONDITIONS" and .name != "DRA_EXAMPLE_DEVICE_METADATA")' >/dev/null
CHANGED=1
kubectl_run -n "$DRIVER_NAMESPACE" set env "$DAEMONSET" DRA_EXAMPLE_SHARED_COUNTERS=1
rollout

snapshot() {
    kubectl_run get resourceslices --field-selector=spec.driver=dra.example.com -o json | "$JQ" -ce '
        [.items[] | {name:.metadata.name, uid:.metadata.uid, version:.metadata.resourceVersion,
            pool:.spec.pool, devices:(.spec.devices // []), counters:(.spec.sharedCounters // [])}] | sort_by(.name)'
}
for attempt in {1..90}; do
    slices=$(snapshot)
    if "$JQ" -e 'length == 2 and ([.[].pool.generation] | unique | length) == 1 and
        all(.[]; .pool.resourceSliceCount == 2) and
        ([.[].devices[]] | length) == 6 and ([.[].counters[]] | length) == 2' <<< "$slices" >/dev/null; then
        break
    fi
    if ((attempt == 90)); then printf 'shared-counter inventory did not converge\n' >&2; exit 1; fi
    sleep 2
done
snapshot > "$WORK_DIR/before.json"
# Cross a successful reconciliation interval; no writes or generation changes.
sleep 35
snapshot > "$WORK_DIR/after.json"
cmp "$WORK_DIR/before.json" "$WORK_DIR/after.json"

kubectl_run create namespace "$NAMESPACE"
CREATED=1
for kind in card pair; do
    kubectl_run create -f - <<EOF
apiVersion: resource.k8s.io/v1
kind: DeviceClass
metadata:
  name: $NAMESPACE-$kind
spec:
  selectors:
    - cel:
        expression: 'device.driver == "dra.example.com" && device.attributes["dra.example.com"].type == "$kind"'
EOF
    CREATED_CLASSES+=("$NAMESPACE-$kind")
done

consumer() {
    local name=$1 kind=$2 count=$3
    kubectl_run -n "$NAMESPACE" create -f - <<EOF
apiVersion: resource.k8s.io/v1
kind: ResourceClaim
metadata:
  name: $name
spec:
  devices:
    requests:
      - name: device
        exactly:
          deviceClassName: $NAMESPACE-$kind
          count: $count
---
apiVersion: v1
kind: Pod
metadata:
  name: $name
spec:
  restartPolicy: Never
  containers:
    - name: consumer
      image: busybox:1.37
      imagePullPolicy: IfNotPresent
      command: [sh, -ec, 'test -n "\$DRA_E2E_DEVICE"; echo "DRA_E2E_DEVICE=\$DRA_E2E_DEVICE"; sleep 3600']
      resources:
        claims:
          - name: device
  resourceClaims:
    - name: device
      resourceClaimName: $name
EOF
}
ready() {
    kubectl_run -n "$NAMESPACE" wait --for=condition=Ready "pod/$1" --timeout=180s
    kubectl_run -n "$NAMESPACE" logs "$1" | grep -F DRA_E2E_DEVICE=
}
blocked() {
    local attempt
    for attempt in {1..60}; do
        if kubectl_run -n "$NAMESPACE" get "pod/$1" -o json | "$JQ" -e '
            .spec.nodeName == null and any(.status.conditions[]?;
                .type == "PodScheduled" and .status == "False" and .reason == "Unschedulable")' >/dev/null; then
            kubectl_run -n "$NAMESPACE" get "resourceclaim/$1" -o json |
                "$JQ" -e '.status.allocation == null' >/dev/null
            return
        fi
        sleep 1
    done
    printf 'expected consumer %s to remain unschedulable\n' "$1" >&2
    return 1
}
allocated_device() {
    kubectl_run -n "$NAMESPACE" get "resourceclaim/$1" -o json |
        "$JQ" -er '.status.allocation.devices.results[0].device'
}
release() {
    kubectl_run -n "$NAMESPACE" delete pod "$1" --wait=true --timeout=180s
    kubectl_run -n "$NAMESPACE" delete resourceclaim "$1" --wait=true --timeout=180s
}
clear_consumers() {
    kubectl_run -n "$NAMESPACE" delete pods --all --wait=true --timeout=180s
    kubectl_run -n "$NAMESPACE" delete resourceclaims --all --wait=true --timeout=180s
}

# Both pairs can run; a third waits until a pair is released.
consumer pair-1 pair 1; ready pair-1
consumer pair-2 pair 1; ready pair-2
consumer pair-3 pair 1; blocked pair-3
release pair-1
ready pair-3
clear_consumers

# A single card excludes its complete pair, so the pair uses other hardware.
consumer card-1 card 1; ready card-1
consumer pair-1 pair 1; ready pair-1
card=$(allocated_device card-1)
pair=$(allocated_device pair-1)
test "${card%%-*}" != "${pair%%-*}"
clear_consumers

# A pair leaves only two cards; three cards wait, two run. Release permits retry.
consumer pair-1 pair 1; ready pair-1
consumer cards-3 card 3; blocked cards-3
consumer cards-2 card 2; ready cards-2
release pair-1
release cards-2
ready cards-3
printf 'shared-counter publication, stable polls, exclusion, release and CDI passed\n'
