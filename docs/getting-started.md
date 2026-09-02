# Getting started with k8s-device-plugin-rs

This workspace supports two Kubernetes extension models. Pick one before
writing a backend; they solve different problems and use different kubelet
protocols.

| Use case | Start here | Runnable reference |
|---|---|---|
| Kubelet assigns a fixed extended resource such as `example.com/widget` directly to a container. | Classic Device Plugin API | [`lib/examples/example_plugin.rs`](../lib/examples/example_plugin.rs) and the deployable [`example/`](../example/README.md) crate. |
| Scheduler selects devices through `DeviceClass` and `ResourceClaim`; kubelet prepares the selected claim on the node. | Dynamic Resource Allocation (DRA) | [`dra/examples/minimal_driver.rs`](../dra/examples/minimal_driver.rs) and the [`dra/k8s/`](../dra/k8s/README.md) validation deployment. |

The classic runtime is the shortest path for a fixed extended resource. Choose
DRA only when workloads need claim-based, scheduler-visible device selection.
Do not run both runtimes against the same hardware unless the backend owns a
clear, non-overlapping allocation boundary.

## Add the framework

Start with the umbrella crate. It exposes common backend types at the crate
root and the focused crates under modules. Select only the runtime your
application uses:

```toml
[dependencies]
async-trait = "0.1"
k8s-device-plugin = { version = "0.0.4", default-features = false, features = ["dra"] }
kube = "4.0"
tokio = { version = "1.52", features = ["macros", "rt-multi-thread"] }
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
```

The DRA runtime needs a Kubernetes client in addition to kubelet sockets. Its
real pluginwatcher registration currently also needs the workspace's temporary
`h2` patch. Applications consuming the crate outside this workspace must copy
the exact patch from the root [`Cargo.toml`](../Cargo.toml) into their own
top-level manifest, then regenerate their lockfile.

```toml
[patch.crates-io]
h2 = { git = "https://github.com/rkubectl/h2", rev = "852915ba20501b2a0d39bd54ab4521cde8ee54c9" }
```

## First classic device plugin

For a fixed inventory, begin with `StaticDevicePlugin`; the root
[README](../README.md#the-fast-path-a-fixed-device-list) has the smallest
complete program. For a real backend, start from
[`lib/examples/example_plugin.rs`](../lib/examples/example_plugin.rs): it
demonstrates discovery, allocation artifacts, a pre-start hook, preferred
allocation, and structured logs. Use the deployable [`example/`](../example/README.md)
crate as the container and DaemonSet starting point.

Run the example only where a kubelet registration socket is reachable, such as
a node-plugin DaemonSet. On a developer machine without that socket, it exits
with a clear registration error by design.

## First DRA driver

Copy [`dra/examples/minimal_driver.rs`](../dra/examples/minimal_driver.rs) into
your application and make these replacements in order:

1. Give the driver a stable, DNS-style name. Use the same value in
   `DraDriver::driver_name`, `DraPlugin::new`, the DaemonSet `DRIVER_NAME`, and
   the mounted `/var/lib/kubelet/plugins/<driver-name>/` directory.
2. Replace the static `ResourcePool::devices` implementation with the complete
   current inventory. Preserve deterministic `BTreeMap` ordering and stable
   pool/device names; the scheduler records those names in allocations.
3. Replace the example CDI writer with the real CDI specifications or other
   driver-owned preparation artifacts. Return each allocated device as a
   `PreparedDevice`, including its pool, device, request names, and CDI IDs.
4. Make `ClaimPreparer::prepare` idempotent and return exactly one result for
   every `ResolvedClaim` received in the batch. `unprepare` must undo the
   driver-owned artifacts safely after a retry or process restart.
5. Create `kube::Client` from in-cluster configuration and set `NODE_NAME` from
   the DaemonSet's downward API. `DraPlugin::run()` creates the required local
   kubelet socket directories and drives inventory publication.

The example emits a harmless environment-variable CDI edit so the included
smoke test can prove container attachment. It is intentionally not a hardware
driver. Its Kubernetes manifests are a validation fixture, not production
RBAC; use the [production deployment contract](../dra/k8s/README.md#production-deployment-contract)
when creating a real DaemonSet.

### Validate the first DRA deployment

Build from the repository root, make the image available to the target
cluster, and run the checked-in fixture:

```sh
container build -f dra/Dockerfile -t k8s-device-plugin-dra-example .
kubectl apply -k dra/k8s
KUBE_CONTEXT=<context> dra/hack/e2e-smoke.sh
```

The smoke test verifies pluginwatcher registration, `ResourceSlice`
publication, claim allocation, preparation, CDI attachment, device-status
publication, and unpreparation. It is a development gate—not a substitute for
the multi-node, drain, upgrade, image, host-permission, and admission-policy
checks in the deployment contract.

## Add DRA observability deliberately

The full DRA example publishes `{"phase":"prepared"}` in
`ResourceClaim.status.devices`. Use the same explicit publisher when a backend
has configuration or diagnostics worth exposing:

```rust
let publisher = ClaimDeviceStatusPublisher::new(client.clone(), driver_name.clone());

let mut status = ClaimDeviceStatus::new(&device.pool_name, &device.device_name);
status.data = Some(serde_json::json!({ "phase": "prepared" }));
publisher.publish(&resolved.claim, [status]).await?;
```

Call it only after the backend has completed the corresponding device work.
The publisher validates the claim UID and scheduler allocation, is idempotent,
and preserves status entries owned by other drivers. On Kubernetes v1.36 it
depends on the default-on `DRAResourceClaimDeviceStatus` beta gate; it is GA in
v1.37. The required node-aware RBAC and payload limits are documented in the
[DRA integration contract](../dra/README.md#resourceclaim-device-status).

Resource health is a different, opt-in stream for already allocated devices.
Enable it in the umbrella crate and use the runnable
[`resource_health_reporter`](../dra/examples/resource_health_reporter.rs)
example as the trait-level starting point:

```toml
k8s-device-plugin = { version = "0.0.4", default-features = false, features = ["dra", "resource-health"] }
```

Implement `ResourceHealthReporter` on the same backend as `DraDriver`, then
construct it with `DraPlugin::with_resource_health`. Do not infer those reports
from inventory snapshots: health reports describe allocated devices and must be
refreshed before their expiry.

## Before shipping

Run the local quality gate before every change:

```sh
mise run ci
```

For DRA production readiness, also complete every item in the
[production deployment contract](../dra/k8s/README.md#production-deployment-contract),
including minimal host access, node-local credentials, admission control,
real-RPC probes, multi-node validation, drain behavior, and a `maxSurge: 0`
upgrade test.
