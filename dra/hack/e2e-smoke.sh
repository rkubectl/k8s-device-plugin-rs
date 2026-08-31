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
kubectl_run apply -f "$FIXTURE"
kubectl_run -n "$NAMESPACE" wait --for=condition=Ready pod/dra-example-consumer --timeout=180s
kubectl_run -n "$NAMESPACE" logs pod/dra-example-consumer | grep -F 'DRA_E2E_DEVICE=widget-0'

# Deleting the consumer asks kubelet to invoke NodeUnprepareResources. The
# driver emits a structured "unpreparing DRA claim" event that an operator can
# inspect with `kubectl logs -n $NAMESPACE daemonset/k8s-device-plugin-dra-example`.
kubectl_run -n "$NAMESPACE" delete pod/dra-example-consumer --wait=true
