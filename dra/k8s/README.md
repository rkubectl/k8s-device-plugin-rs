# DRA validation manifests

These manifests deploy the `minimal_driver` example from this crate. Build
the image from the repository root, load or publish it for the target cluster,
then apply the kustomization:

```sh
container build -f dra/Dockerfile -t k8s-device-plugin-dra-example .
kubectl apply -k dra/k8s
```

The target cluster must expose the `resource.k8s.io/v1` DRA API and have DRA
enabled before deployment. Kubernetes v1.36 with its default DRA configuration
is the compatibility target; its core path intentionally does not require
optional DRA feature gates. The checked-in smoke path completed on Kubernetes
v1.36.1 linux/arm64. This driver publishes one static `widget-0` device in the
`widget-pool` pool on each node; it is for registration and API-path validation
only. It emits a harmless CDI environment marker for each static device so an
end-to-end consumer pod can verify DRA attachment, but it does not provide a
real hardware device.

The manifest is configured for the `dra.example.com` driver name and mounts
only that driver's kubelet plugin directory. If you override `DRIVER_NAME`,
update the matching `hostPath` and `mountPath` in `daemonset.yaml` too.

After the DaemonSet is ready, use [`../hack/e2e-smoke.sh`](../hack/e2e-smoke.sh)
to validate ResourceClaim allocation, kubelet preparation, CDI injection, and
unpreparation with a temporary consumer pod. The example also publishes its
driver-owned `ResourceClaim.status.devices` entry during preparation, and the
smoke script asserts its `data.phase=prepared` value. Before it creates the consumer,
the script executes the driver's real-RPC liveness probe and requires a
`ResourceSlice` on every node that runs a driver pod. It therefore exercises
all DaemonSet replicas when the target has multiple nodes.

The `ClusterRole` is intentionally broader than a production driver needs:
it can read `ResourceClaim`s cluster-wide and manage `ResourceSlice`s
cluster-wide. Upstream's DRA example uses a Validating Admission Policy to
limit access to the local node. That policy is deliberately not copied into
this small validation fixture; do not deploy these manifests as production
RBAC.

`maxSurge: 0` is explicit because a phase-1 driver cannot assume a target
kubelet supports seamless node-plugin upgrades. Deleting the DaemonSet fully
removes the validation fixture:

```sh
kubectl delete -k dra/k8s
```

## Production deployment contract

Do not promote this kustomization itself to production. A production DRA
driver must substitute a signed, immutable driver image, hardware-specific
device mounts and Linux permissions, resource requests/limits, node targeting,
and its own driver configuration. Keep the host mounts narrow: the registry
directory, only that driver's plugin directory, and only the CDI and device
paths the driver genuinely needs. Running as root is common for hardware
drivers but is not a default to copy; use the minimum user, capabilities, and
SELinux/AppArmor profile that the target kernel interface permits.

The runtime currently needs exactly these Kubernetes API permissions:

```yaml
rules:
  - apiGroups: ["resource.k8s.io"]
    resources: ["resourceclaims"]
    verbs: ["get"]
  - apiGroups: ["resource.k8s.io"]
    resources: ["resourceclaims/status"]
    verbs: ["get", "patch"]
  - apiGroups: ["resource.k8s.io"]
    resources: ["resourceclaims/driver"]
    verbs: ["associated-node:patch"]
    resourceNames: ["your.driver.example.com"]
  - apiGroups: ["resource.k8s.io"]
    resources: ["resourceslices"]
    verbs: ["get", "list", "create", "update", "patch", "delete"]
  - apiGroups: [""]
    resources: ["nodes"]
    verbs: ["get"]
```

Bind that `ClusterRole` only to the driver DaemonSet's dedicated service
account. `ResourceSlice` is cluster-scoped and a node-local driver must read
claims that can originate in any workload namespace, so ordinary RBAC cannot
express “only this node's objects.” Use a cluster-admin-owned
`ValidatingAdmissionPolicy` or equivalent admission control to require the
driver name and expected Node owner reference on every slice mutation. Treat
that admission policy as part of the production deployment and validate its
exact CEL expressions against the target Kubernetes version before enforcement.
RBAC alone does not provide that isolation. `resourceclaims/driver` is a
Kubernetes v1.36 synthetic subresource: bind the driver service account to a
node-local, pod-bound token so the API server can enforce the associated-node
check. `DRAResourceClaimDeviceStatus` is beta and enabled by default in v1.36,
then GA in v1.37; a v1.36 cluster that explicitly disables it must omit the
reporting path and these permissions.

### Liveness and readiness

The example image contains `/usr/local/bin/k8s-device-plugin-dra-liveness`.
It calls the registration socket's `GetInfo` RPC, checks that the advertised
endpoint is the expected plugin socket, then calls `NodePrepareResources` with
an empty claim batch. The latter is non-mutating, so it is safe for the
`startupProbe`, `livenessProbe`, and `readinessProbe` in `daemonset.yaml`.
Copy that strategy into a real driver image by using
[`DraPluginLivenessProbe`](../src/health.rs) or an equivalent real-RPC
implementation; checking merely that `*.sock` files exist does not prove that a
server is accepting requests.

Readiness means that both node-local gRPC servers answer. It does not by itself
prove that inventory reached the API server, so rollout gates must also verify
at least one correctly-owned `ResourceSlice` for every ready driver pod. If
any registration server, DRA service, or publisher task exits,
`DraPlugin::run()` aborts the other tasks and returns an error; a normal
DaemonSet pod restart then restarts the process. Alert on restarts and failed
probes instead of assuming that a healthy process implies healthy inventory.

### Multi-node, drain, and upgrade gates

Run these checks on a representative multi-node target before approving a
driver release. They are operational gates, not claims that the single-node
fixture has already proved them.

1. Schedule the DaemonSet only on nodes with the target hardware. Wait until
   desired, current, updated, and ready DaemonSet counts agree. Run
   `dra/hack/e2e-smoke.sh`; it probes every driver pod and confirms a local
   `ResourceSlice` for each one.
2. Keep a consumer with a prepared claim on one node. Cordon and drain a
   different maintenance node according to that cluster's workload and Pod
   disruption policy, then uncordon it. Verify its driver becomes ready again,
   republishes its slice, and the probe succeeds. Before draining or removing
   a node with a prepared claim, wait for kubelet's
   `dra_resource_claims_in_use{driver_name="..."}` metric to reach zero and
   verify `NodeUnprepareResources` in driver logs; removing an in-use driver
   can strand pod cleanup.
3. Upgrade with an immutable image reference while a consumer remains active.
   `maxSurge: 0` is mandatory here: observe the old driver pod fully terminate
   before its replacement starts, verify the replacement's liveness probe and
   slice, then delete the consumer and observe unprepare. Do not enable a
   surge rollout merely because a Kubernetes version is new enough on paper;
   first demonstrate the target kubelet's seamless DRA-plugin upgrade behavior
   with the real driver and retain evidence with the release.
