#!/bin/sh
# Validate the deployed minimal DRA driver through kubelet and containerd CDI.
#
# The driver DaemonSet must already be running. Set KUBE_CONTEXT when the
# current kubectl context is not the validation cluster.

set -eu

KUBECTL=${KUBECTL:-kubectl}
KUBE_CONTEXT=${KUBE_CONTEXT:-}
NAMESPACE=k8s-device-plugin-dra-example
FIXTURE=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)/e2e-smoke.yaml

kubectl_run() {
    if [ -n "$KUBE_CONTEXT" ]; then
        "$KUBECTL" --context "$KUBE_CONTEXT" "$@"
    else
        "$KUBECTL" "$@"
    fi
}

cleanup() {
    kubectl_run delete --ignore-not-found -f "$FIXTURE" >/dev/null
}

trap cleanup EXIT INT TERM

cleanup

# Exercise the same real-RPC liveness probe configured on every driver pod.
# On a multi-node cluster this also verifies that every DaemonSet replica has
# published a local ResourceSlice before a consumer asks kubelet for a device.
driver_pods=$(kubectl_run -n "$NAMESPACE" get pod -l app=k8s-device-plugin-dra-example -o name)
test -n "$driver_pods"
for driver_pod in $driver_pods; do
    kubectl_run -n "$NAMESPACE" exec "$driver_pod" -- /usr/local/bin/k8s-device-plugin-dra-liveness
    node_name=$(kubectl_run -n "$NAMESPACE" get "$driver_pod" -o jsonpath='{.spec.nodeName}')
    kubectl_run get resourceslices \
        --field-selector="spec.driver=dra.example.com,spec.nodeName=$node_name" \
        -o name | grep -q '^resourceslice.resource.k8s.io/'
done

kubectl_run apply -f "$FIXTURE"
kubectl_run -n "$NAMESPACE" wait --for=condition=Ready pod/dra-example-consumer --timeout=180s
kubectl_run -n "$NAMESPACE" logs pod/dra-example-consumer | grep -F 'DRA_E2E_DEVICE=widget-0'

# Deleting the consumer asks kubelet to invoke NodeUnprepareResources. The
# driver emits a structured "unpreparing DRA claim" event that an operator can
# inspect with `kubectl logs -n $NAMESPACE daemonset/k8s-device-plugin-dra-example`.
kubectl_run -n "$NAMESPACE" delete pod/dra-example-consumer --wait=true
