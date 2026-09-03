#!/bin/sh
# Validate Kubernetes v1.37 DRA allocation through a container resource request.

set -eu

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)

DRA_E2E_FIXTURE="$SCRIPT_DIR/e2e-extended-resource.yaml" \
DRA_E2E_CONSUMER=dra-example-extended-resource-consumer \
DRA_E2E_GENERATED_CLAIM=1 \
exec "$SCRIPT_DIR/e2e-smoke.sh"
