#!/bin/sh
# Prove CDI injection when a container selects a FirstAvailable request.
set -eu
SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
if [ -n "${KUBE_CONTEXT:-}" ]; then set -- --context "$KUBE_CONTEXT"; else set --; fi
metadata=$("${KUBECTL:-kubectl}" "$@" -n k8s-device-plugin-dra-example \
    get daemonset/k8s-device-plugin-dra-example \
    -o 'jsonpath={.spec.template.spec.containers[0].env[?(@.name=="DRA_EXAMPLE_DEVICE_METADATA")].value}')
case "$metadata" in ""|0) ;; *) printf 'FirstAvailable regression requires metadata disabled\n' >&2; exit 1 ;; esac
DRA_E2E_FIXTURE="$SCRIPT_DIR/e2e-first-available.yaml" \
DRA_E2E_CONSUMER=dra-example-first-available-consumer \
DRA_E2E_CLAIM_NAME=dra-example-first-available \
exec "$SCRIPT_DIR/e2e-smoke.sh"
