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
Extended-resource allocation is additionally supported on Kubernetes v1.37,
where it is stable. Device taints and other optional DRA APIs remain outside
the v1.36 baseline. The checked-in DaemonSet is a validation fixture, not a
deployment template. See the
[DRA design](../docs/dra-design.md) for the complete boundary and roadmap, and
[the extended-resource guide](../docs/dra-extended-resources.md) for a complete
driver-author and workload-consumer walkthrough.

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

## Customer quickstart

Start with the workspace [getting-started guide](../docs/getting-started.md)
when choosing between the classic and DRA runtimes, adding the facade crate to
an application, or following a first deployment. This guide is the DRA-specific
contract for the backend, Kubernetes API access, and validation.

Implement `ResourcePool` to report the pool/device snapshot and
`ClaimPreparer` to prepare or release allocated devices. `DraDriver` ties the
two traits together. Then create `DraPlugin` with a Kubernetes client, the
driver name, and the local node name, and call `.run()`:

```rust
let client = kube::Client::try_default().await?;
let node_name = std::env::var("NODE_NAME")?;
let plugin = DraPlugin::new(client, "dra.example.com", node_name, driver);
plugin.run().await
```

[`examples/minimal_driver.rs`](examples/minimal_driver.rs) is the complete,
copyable DRA deployment example. It covers a stable inventory, idempotent
claim preparation, CDI output, a status publisher, kube client construction,
and structured logging. It implements a static `widget-0` device with a
harmless CDI environment marker instead of hardware backing, so it is suitable
for repeatable cluster validation. Replace its static inventory and CDI writer
with the hardware backend, but retain its claim identity, allocation, and
idempotence checks. Cargo compiles and tests it directly:

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

For the opt-in resource-health protocol, see the standalone
[`examples/resource_health_reporter.rs`](examples/resource_health_reporter.rs)
contract example. It demonstrates source-scoped snapshots and clean shutdown
when kubelet closes the report channel:

```bash
cargo run -p k8s-device-plugin-dra --example resource_health_reporter --features resource-health
```

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

#### Kubernetes v1.37 extended-resource smoke

Kubernetes v1.37 makes DRA extended-resource allocation stable. A
`DeviceClass` can map its matching DRA devices to an ordinary extended resource
name, so a workload can request it in `resources.limits` without naming a
`ResourceClaim` itself:

```sh
KUBE_CONTEXT=<v1.37-context> dra/hack/e2e-extended-resource.sh
```

The fixture maps `dra.example.com/widget`, requests one unit from its consumer
container, verifies the CDI marker, discovers the scheduler-generated claim,
asserts its driver-owned prepared status, and waits for that claim to be
deleted with the consumer. This is v1.37-only validation: v1.36 exposes the
field as beta, while the supported baseline deliberately remains independent
of it. For production-oriented manifests, generated-claim inspection, the
classic/DRA node boundary, and a migration sequence, read
[Consume DRA devices as extended resources](../docs/dra-extended-resources.md).

#### Verified local Apple Container runs

The local native-arm64 Apple Container helper is named `k8s-versioned`. It is
host tooling derived from Apple Container 1.3.1, not a project dependency. It
pins `kindest/node` images for Kubernetes v1.36.1 and v1.37.0, passes the
selected image version to kubeadm, persists the kind restart configuration,
and always provisions with Rosetta disabled. A Homebrew upgrade of `container`
replaces its plugin directory, so reinstall this local helper after upgrading.

The two on-demand validation profiles are intentionally not run together:

```sh
# Switch to the v1.36.1 baseline.
container stop dra-validation-137
container k8s-versioned start --name dra-validation

# Switch to the v1.37.0 validation profile.
container stop dra-validation
container k8s-versioned start --name dra-validation-137
```

Both profiles were created and restarted successfully as linux/arm64 nodes.
The v1.37.0 profile also completed the checked-in DRA smoke test. After
building the image, the validation flow is:

```sh
container k8s-versioned load-image --name <cluster> k8s-device-plugin-dra-example
kubectl --context <cluster> apply -k dra/k8s
KUBE_CONTEXT=<cluster> dra/hack/e2e-smoke.sh
```

`load-image` also adds the canonical `docker.io/library/...` CRI alias for a
short local image name. The smoke run reported `DRA_E2E_DEVICE=widget-0` and
deleted the consumer successfully. Rosetta was not installed or enabled.

#### Serial local validation matrix

Run both local profiles without keeping them running together:

```sh
mise run dra-validate-k8s
```

The task builds the linux/arm64 fixture image once, starts each profile in
turn, imports the image, applies and restarts the validation DaemonSet, waits
for rollout, and invokes the core smoke fixture. The v1.37 profile also runs
the extended-resource smoke fixture. It verifies that each server is the
expected patch version and has Rosetta disabled. Apple Container can assign a
fresh guest IP on restart, so the task refreshes kube-proxy's API endpoint
before exercising service networking. On either
success or failure it stops the v1.37 profile and restores `dra-validation`
(v1.36.1) as the sole running cluster. This is an opt-in local integration
gate, not part of `mise run ci`. Set `DRA_E2E_SKIP_BUILD=1` only when the
current local `k8s-device-plugin-dra-example` image has already been built.
