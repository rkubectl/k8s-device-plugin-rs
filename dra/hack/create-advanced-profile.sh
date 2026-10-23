#!/usr/bin/env bash
# Opt-in: create a NEW native v1.37 profile for the selected gated DRA fixtures.
# Refuses existing names and any concurrently running Kubernetes profile.
set -euo pipefail
umask 077
CONTAINER=${CONTAINER:-container}
KUBECTL=${KUBECTL:-kubectl}
JQ=${JQ:-jq}
YQ=${YQ:-yq}
PYTHON=${PYTHON:-python3}
CLUSTER=${DRA_E2E_ADVANCED_CLUSTER:-dra-validation-advanced-137}
SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
CREATED=0

for command in "$CONTAINER" "$KUBECTL" "$JQ" "$YQ" "$PYTHON"; do command -v "$command" >/dev/null; done
if [[ $(uname -s) != Darwin || $(uname -m) != arm64 ]]; then
    printf 'advanced-profile provisioning requires native arm64 macOS\n' >&2
    exit 1
fi
case "$CLUSTER" in dra-validation-advanced-*) ;; *) printf 'use a dedicated dra-validation-advanced-* name\n' >&2; exit 1 ;; esac
inventory=$("$CONTAINER" list --all --format json)
"$JQ" -e --arg name "$CLUSTER" 'all(.[]; .id != $name)' <<< "$inventory" >/dev/null
"$JQ" -e 'all(.[]; .status.state != "running" or
    (.configuration.labels["com.apple.container.plugin"] != "k8s" and
     .configuration.labels["com.apple.container.plugin"] != "rosettaless-k8s"))' <<< "$inventory" >/dev/null
WORK_DIR=$(mktemp -d)
cleanup() {
    local result=$?
    trap - EXIT
    rm -rf "$WORK_DIR"
    if ((result != 0 && CREATED)); then "$CONTAINER" stop "$CLUSTER" || true; fi
    exit "$result"
}
trap cleanup EXIT
"$CONTAINER" k8s-versioned create --name "$CLUSTER" --kubernetes-version v1.37.0 --cpus 4 --memory 4G
CREATED=1
"$CONTAINER" list --all --format json | "$JQ" -e --arg name "$CLUSTER" '
    [.[] | select(.id == $name)] | length == 1 and .[0].configuration.rosetta == false' >/dev/null

# Keep all credential-bearing configuration inside a mode-0700 temporary folder;
# transform through JSON without evaluating tags or printing configuration data.
"$CONTAINER" copy "$CLUSTER:/kind/kubeadm.conf" "$WORK_DIR/kubeadm.original.yaml"
"$CONTAINER" copy "$CLUSTER:/var/lib/kubelet/config.yaml" "$WORK_DIR/kubelet.original.yaml"
"$YQ" -o=json -I=0 '.' "$WORK_DIR/kubeadm.original.yaml" |
    "$PYTHON" "$SCRIPT_DIR/configure-advanced-profile.py" --mode kubeadm |
    "$YQ" -p=json -o=yaml '.' > "$WORK_DIR/kubeadm.yaml"
"$YQ" -o=json -I=0 '.' "$WORK_DIR/kubelet.original.yaml" |
    "$PYTHON" "$SCRIPT_DIR/configure-advanced-profile.py" --mode kubelet |
    "$YQ" -p=json -o=yaml '.' > "$WORK_DIR/kubelet.yaml"

"$CONTAINER" exec "$CLUSTER" /bin/sh -ec '
    cp /kind/kubeadm.conf /kind/kubeadm.conf.before-dra-advanced
    cp /etc/kubernetes/kubeadm-config.yaml /etc/kubernetes/kubeadm-config.yaml.before-dra-advanced
    cp /var/lib/kubelet/config.yaml /var/lib/kubelet/config.yaml.before-dra-advanced'
"$CONTAINER" copy "$WORK_DIR/kubeadm.yaml" "$CLUSTER:/kind/kubeadm.conf"
"$CONTAINER" copy "$WORK_DIR/kubeadm.yaml" "$CLUSTER:/etc/kubernetes/kubeadm-config.yaml"
"$CONTAINER" copy "$WORK_DIR/kubelet.yaml" "$CLUSTER:/var/lib/kubelet/config.yaml"
"$CONTAINER" exec "$CLUSTER" /bin/sh -ec '
    chown root:root /kind/kubeadm.conf /etc/kubernetes/kubeadm-config.yaml /var/lib/kubelet/config.yaml
    chmod 600 /kind/kubeadm.conf /etc/kubernetes/kubeadm-config.yaml /var/lib/kubelet/config.yaml
    kubeadm init phase control-plane all --config /etc/kubernetes/kubeadm-config.yaml
    systemctl restart kubelet'
"$CONTAINER" k8s-versioned start --name "$CLUSTER"
# Prove the opt-in configuration survives another guarded VM/IP restart.
"$CONTAINER" stop "$CLUSTER"
"$CONTAINER" k8s-versioned start --name "$CLUSTER"
"$KUBECTL" --context "$CLUSTER" --request-timeout=15s get --raw /apis/scheduling.k8s.io/v1beta1 |
    "$JQ" -e 'any(.resources[]; .name == "podgroups") and any(.resources[]; .name == "workloads")' >/dev/null
"$KUBECTL" --context "$CLUSTER" --request-timeout=15s get --raw /apis/resource.k8s.io/v1alpha3 |
    "$JQ" -e 'any(.resources[]; .name == "resourcepoolstatusrequests")' >/dev/null
printf 'Native gated profile %s is ready; default validation profiles were not reconfigured.\n' "$CLUSTER"
