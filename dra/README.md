# k8s-device-plugin-dra

`k8s-device-plugin-dra` is the Dynamic Resource Allocation (DRA) runtime for
this workspace. A DRA driver publishes the devices present on each node as
Kubernetes `ResourceSlice` objects, then serves kubelet's
`NodePrepareResources` and `NodeUnprepareResources` RPCs for the claims
allocated to that node.

## Status and scope

Phase 1 is ready for a driver backend to integrate and validate against a
real cluster. Its compatibility target is Kubernetes v1.36 using the stable
`resource.k8s.io/v1` API with the default DRA configuration; its core path does
not require optional DRA feature gates. Live validation of that baseline completed
on Kubernetes v1.36.1 on linux/arm64. The validation environment used Apple
Container with Rosetta disabled and a `kindest/node` v1.36.1 image; the
checked-in smoke test completed the ResourceSlice-to-CDI lifecycle. The older
Kubernetes v1.37.0 linux/arm64 run remains historical evidence. It
authoritatively reconciles each local pool into one or more `ResourceSlice`s,
pluginwatcher registration, and claim preparation/unpreparation.

It is not yet a turnkey production driver: resource health is opt-in,
pre-v1.34 API compatibility is absent, and it has no hardware-specific
production deployment.
Extended-resource integration, device taints, and other optional DRA APIs are
also outside this baseline. The checked-in DaemonSet is a validation fixture,
not a deployment template. See the
[DRA design](../docs/dra-design.md) for the complete boundary and roadmap.

## Compatibility and dependency override

Kubelet currently sends a percent-encoded Unix-socket path in the HTTP/2
`:authority` header during pluginwatcher registration. Upstream `h2` rejects
that header before Tonic can dispatch the request. Until upstream `h2` releases
a fix or kubelet fixes its DRA client, this workspace pins a narrow compatibility
fork.

Building this repository's workspace uses the override automatically. If an
application depends on `k8s-device-plugin-dra` from Git or crates.io, Cargo does
**not** propagate the override: the application's top-level `Cargo.toml` must
repeat the following block, then regenerate its lockfile (for example with
`cargo update -p h2`). CI must also be able to fetch Git dependencies.

```toml
[patch.crates-io]
h2 = { git = "https://github.com/rkubectl/h2", rev = "852915ba20501b2a0d39bd54ab4521cde8ee54c9" }
```

The exact pin in the [workspace Cargo manifest](https://github.com/rkubectl/k8s-device-plugin-rs/blob/main/Cargo.toml)
is authoritative. Do not change it independently; the deferred upstream-cleanup
task removes this override once either upstream route is released.

## Implementation and automation contract

This is the compact integration contract for both driver authors and automated
changes:

1. Implement `ResourcePool` to return the complete current device snapshot,
   and `ClaimPreparer` to prepare and release allocations. Together they form
   `DraDriver`.
2. Use one stable driver name everywhere: `DraDriver::driver_name()`,
   `DraPlugin::new`, the DaemonSet `DRIVER_NAME`, and its mounted kubelet plugin
   directory must describe the same driver.
3. Make `ClaimPreparer::prepare` idempotent. It receives a batch and must
   return exactly one result for every input claim; never assume a restart will
   replay preparation.
4. Return the real CDI device IDs or other preparation artifacts required by
   the workload. The minimal driver uses only a harmless environment marker;
   it does not provide hardware.
5. Give the driver Kubernetes API permissions to read claims, publish its
   slices, and read its node. Keep the base role minimal and use a
   cluster-owned admission policy to constrain slice mutations to the driver
   and local node; the fixture's cluster-wide role is deliberately broad.
6. Validate changes with the commands below before treating an integration as
   ready.

### ResourceClaim device status

`ClaimDeviceStatusPublisher` is an explicit backend API for durable,
per-allocation observability. It is separate from both `ResourcePool`
inventory and optional resource-health streaming: create it with the same
Kubernetes client and driver name as `DraPlugin`, then call `publish` from
preparation or a backend monitor whenever a device's configured state changes.
Each `ClaimDeviceStatus` owns its optional JSON `data` value; the publisher
serializes it when it sends the status subresource request and rejects data
larger than Kubernetes' 10 KiB limit.

The publisher validates the `ClaimRef` UID and the exact
`pool`/`device`/`shareID` against the current allocation before sending an
update. It only applies entries for its own driver, never removes omitted
entries, and returns `Unchanged` without a write when the supplied entries are
already current. Server-side apply keeps status list entries owned by other
drivers intact. Treat authorization failures as configuration errors instead
of retrying them: a node-local driver needs `get,patch` on
`resourceclaims/status` plus `associated-node:patch` on the synthetic
`resourceclaims/driver` subresource, restricted with `resourceNames` to its
driver name.

The API is available in Kubernetes v1.36 and v1.37 using
`resource.k8s.io/v1`. In v1.36, `DRAResourceClaimDeviceStatus` is beta and
enabled by default, so cluster operators that explicitly disable it should not
enable this reporting path. It is GA in v1.37. The validation example publishes
`{"phase":"prepared"}` during `NodePrepareResources`, and the smoke script
asserts that value after the consumer starts.

### Optional resource-health reporting

Enable the `resource-health` feature to compile the kubelet
`dra-health/v1alpha1` protocol. A backend can then implement
`ResourceHealthReporter` and use `DraPlugin::with_resource_health(...)`
instead of `DraPlugin::new(...)`. The health service shares the normal DRA
plugin socket; it streams snapshots to kubelet through
`DRAResourceHealth.NodeWatchResources`.

`ResourceHealthReporter` is deliberately separate from `ResourcePool` and
`PoolDevice::health`: inventory snapshots determine what is allocatable,
whereas reports describe the current state of devices already allocated to
workloads. A reporter receives a bounded channel and must stop when that
channel closes. Each report may cover one source's subset of devices, and the
source must resend it before its per-device timeout expires. Returning from a
watch ends that RPC with `Unavailable`, allowing kubelet to reconnect and
start a fresh monitor session. A feature-enabled driver that does not call
`with_resource_health` explicitly returns `Unimplemented`, so kubelet stops
opening health watches for it.

The DRA resource-health protocol is optional on the Kubernetes side as well.
For the v1.36 baseline, `ResourceHealthStatus` is beta and enabled by default;
clusters that disable it will not call the service.

## Quickstart

Implement `ResourcePool` to report the pool/device snapshot and
`ClaimPreparer` to prepare or release allocated devices. `DraDriver` ties the
two traits together. Then create `DraPlugin` with a Kubernetes client, the
driver name, and the local node name, and call `.run()`:

```rust
let client = kube::Client::try_default().await?;
let plugin = DraPlugin::new(client, "dra.example.com", node_name, driver);
plugin.run().await
```

[`examples/minimal_driver.rs`](examples/minimal_driver.rs) is the complete,
copyable version of that pattern. It implements a static `widget-0` device
with a harmless CDI environment marker instead of hardware backing, so it is
suitable for repeatable cluster validation. Keep it as the source of truth for
the quickstart instead of copying a second implementation into this README;
Cargo compiles and tests it directly:

```bash
cargo test -p k8s-device-plugin-dra --example minimal_driver
```

The example reads these environment variables:

| Variable | Default | Meaning |
|---|---|---|
| `DRIVER_NAME` | `dra.example.com` | DRA driver name and kubelet plugin-directory component. |
| `POOL_NAME` | `widget-pool` | Resource pool advertised by the `ResourceSlice`. |
| `DEVICE_NAMES` | `widget-0` | Comma-separated static device names. |
| `NODE_NAME` | *(required)* | Kubernetes node that owns the published `ResourceSlice`. |
| `RUST_LOG` | *(unset)* | Standard `tracing-subscriber` filter, for example `info`. |

`DraPlugin::run()` creates and serves sockets below
`/var/lib/kubelet/plugins_registry/` and
`/var/lib/kubelet/plugins/<driver-name>/`. It also needs Kubernetes API access
to look up the local `Node`, publish its `ResourceSlice`, and resolve claims;
unlike a classic device plugin, it cannot run meaningfully without a
Kubernetes client configuration and `NODE_NAME`. Both DRA sockets refuse to
replace an actively-serving instance and recover only a stale socket left by a
terminated instance.

### Validation checklist

```sh
cargo test --locked -p k8s-device-plugin-dra --all-targets
container build -f dra/Dockerfile -t k8s-device-plugin-dra-example .
kubectl apply -k dra/k8s
KUBE_CONTEXT=<context> dra/hack/e2e-smoke.sh
```

The smoke test proves registration, `ResourceClaim` allocation,
`NodePrepareResources`, CDI attachment, `ResourceClaim.status.devices`
publication, and `NodeUnprepareResources`. It does not by itself validate
production RBAC boundaries, multi-node behavior, or upgrade availability; the production gates are in the
[manifest guide](k8s/README.md#production-deployment-contract).

On Apple Container 1.3.1, a large local `target/` directory may be walked
while the build context is prepared despite this repository's `.dockerignore`.
If the build stalls before `load build context`, run `cargo clean` and retry;
this is a local builder-context issue, not a Rust or Dockerfile failure.

## Cluster validation deployment

[`k8s/`](k8s/README.md) provides a small validation-only DaemonSet, RBAC, and
Kustomization for `minimal_driver`. Build the image from the repository root,
make it available to the target cluster, then apply it:

```bash
container build -f dra/Dockerfile -t k8s-device-plugin-dra-example .
kubectl apply -k dra/k8s
```

The target cluster must expose `resource.k8s.io/v1` with DRA enabled. The
DaemonSet explicitly uses `maxSurge: 0`: Phase 1 cannot assume kubelet support
for seamless DRA-plugin upgrades, so an update may leave a brief gap while the
old node plugin terminates before the new one starts. The RBAC is intentionally
broad for this validation fixture and is not production guidance; see the
[manifest README](k8s/README.md) for production RBAC, real-RPC liveness, and
multi-node maintenance requirements.

### End-to-end smoke test

With the DaemonSet ready, run the checked-in consumer fixture:

```sh
KUBE_CONTEXT=<context> dra/hack/e2e-smoke.sh
```

The script creates a `DeviceClass`, `ResourceClaim`, and one consumer pod. It
asserts that containerd applied `DRA_E2E_DEVICE=widget-0` from the fixture's
CDI specification, then deletes the consumer. Inspect the DaemonSet log for
`preparing DRA claim` and `unpreparing DRA claim` to see the corresponding
kubelet RPCs. The script removes all of its fixture objects on exit.

#### Verified local Apple Container run

The v1.36.1 validation used a local, native arm64 Apple Container Kubernetes
cluster named `dra-validation`, provided by a separately installed
`rosettaless-k8s` plugin. That plugin is host tooling, not a project
dependency. After building the image, the verified flow was:

```sh
container rosettaless-k8s load-image --name dra-validation k8s-device-plugin-dra-example
kubectl apply -k dra/k8s
KUBE_CONTEXT=dra-validation dra/hack/e2e-smoke.sh
```

The current local plugin's `load-image` operation imports the image but can
omit the fully qualified CRI tag that kubelet resolves. If the driver pod is
in `ImagePullBackOff` for `docker.io/library/k8s-device-plugin-dra-example`,
add that tag in the node and recreate only the failed DaemonSet pod:

```sh
container exec dra-validation ctr -n k8s.io images tag \
  k8s-device-plugin-dra-example:latest \
  docker.io/library/k8s-device-plugin-dra-example:latest
kubectl -n k8s-device-plugin-dra-example delete pod \
  -l app=k8s-device-plugin-dra-example
```

The subsequent smoke run reported `DRA_E2E_DEVICE=widget-0` and deleted the
consumer successfully. Rosetta was not installed or enabled for this run.
