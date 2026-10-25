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
checked-in smoke test completed the ResourceSlice-to-CDI lifecycle. Both native v1.36.1 and v1.37.0 profiles passed the guarded serial matrix
on 2026-10-23, including the opt-in device-taint and binding-condition fixtures. It
authoritatively reconciles each local pool into one or more `ResourceSlice`s,
pluginwatcher registration, and claim preparation/unpreparation.

It is not yet a turnkey production driver: resource health is opt-in,
pre-v1.34 API compatibility is absent, and it has no hardware-specific
production deployment.
Extended-resource allocation is additionally supported on Kubernetes v1.37,
where it is stable. Explicit device-taint and binding-condition adapters are
also available and remain opt-in, keeping the core v1.36 baseline independent
of those capabilities. The checked-in DaemonSet is a validation fixture, not a
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
h2 = { git = "https://github.com/rkubectl/h2", tag = "v0.4.20+20261023" }
```

The fork is based on upstream `h2` v0.4.20. Its
[maintenance notes](https://github.com/rkubectl/h2/blob/v0.4.20%2B20261023/UDS-COMPAT.md)
record the patch provenance and regression checks.

The annotated tag `v0.4.20+20261023` identifies the upstream base plus our
fork snapshot date. It resolves to the previously validated commit
`e80d915d7468bc7009d3d38acfcaebc83b747f4b`; Cargo retains that exact commit
in `Cargo.lock`. Published snapshot tags must never be moved or reused. A
second snapshot on the same date uses `.1`, then `.2`, and so on. The crate's
internal version remains `0.4.20`; the date does not imply a new validation run.

Status checked on **2026-10-23**: v0.4.20 remains the latest upstream release,
and [h2 PR #487](https://github.com/hyperium/h2/pull/487) remains open and
unmerged. Kubelet's pluginwatcher registration client still uses a bare socket
path in Kubernetes v1.36.5, v1.37.1, and current master. Retain the exact pin
above; see the [dated upstream audit](../docs/dra-design.md#h2-fork-status-2026-10-23)
for source references and the removal gate.

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

DRA constructors return `Result` and reject invalid driver and node names.
Driver names are DNS subdomains limited to 63 bytes; node names are DNS
subdomains limited to 253 bytes. `DraPlugin` checks that its name agrees with
`DraDriver::driver_name()`. Standalone resolvers and publishers require the same
validated driver identity. Custom-path liveness probes also require the
expected driver name and verify both that name and the advertised DRA service.

`ClaimResolver::new(client, driver_name)` exposes only that driver's allocations
to the preparer. Allocation request references retain `/subrequest` suffixes;
prepared responses normalize them to deduplicated top-level request names for
kubelet. An empty request-name list continues to mean all requests. Local
allocations with share IDs are rejected before preparation: shared/consumable
device preparation is not yet supported, including the metadata path.

Pool names may contain slash-separated DNS subdomains, up to 253 bytes total.
ResourceSlice names use a bounded stable hash of the complete driver/node/pool
identity and an explicit chunk index. Upgrading from legacy names publishes
the new generation before deleting old slices with UID/resourceVersion
preconditions; it may briefly leave two generations visible to the scheduler.
The publisher refuses to replace a slice belonging to a different pool.
Zero polling and publisher retry intervals are clamped to one millisecond.

`PoolDevice::health` is informational backend state and does not exclude devices
from DRA publication or scheduling. Omit unavailable devices from inventory or
use explicit device taints; allocated-device health uses the independent
reporter below.

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
update. It only changes entries for its own driver, never removes omitted
entries, and returns `Unchanged` without a write when the supplied entries are
already current. A JSON merge patch retains the current list and replaces or
adds only the supplied entries, using `metadata.resourceVersion` as a
precondition so concurrent updates cannot be overwritten. A conflict triggers
a fresh read and full UID/allocation revalidation, up to three patch attempts;
an exhausted conflict is returned to the caller. `with_field_manager` controls
update attribution, not server-side apply ownership. Do not mix SSA and this
publisher for the same status entries. Omitted entries are not a deletion
request; their removal remains part of the claim lifecycle.
Treat authorization failures as configuration errors instead
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
start a fresh monitor session. Only a backend that calls `with_resource_health`
advertises `v1alpha1.DRAResourceHealth` in registration. Kubelet does not open
health watches for a backend that does not opt in.

Disconnecting kubelet cancels and reaps the reporter even if it is idle.
Reporters must be cancellation-safe: keep cleanup in owned guards and avoid
detached background work that outlives the watch future.

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
let plugin = DraPlugin::new(client, "dra.example.com", node_name, driver)?;
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

For a topology-aware accelerator backend, see
[`examples/accelerator_pair_driver.rs`](examples/accelerator_pair_driver.rs)
and its self-contained
[Deployment and StatefulSet manifests](examples/accelerator-pairs/README.md).
It models a valid hardware-linked pair as one allocatable DRA device, which is
the safe representation when independently selecting two cards cannot prove
their direct interconnect.

The example reads these environment variables:

| Variable | Default | Meaning |
|---|---|---|
| `DRIVER_NAME` | `dra.example.com` | DRA driver name and kubelet plugin-directory component. |
| `POOL_NAME` | `widget-pool` | Resource pool advertised by the `ResourceSlice`. |
| `DEVICE_NAMES` | `widget-0` | Comma-separated static device names. |
| `NODE_NAME` | *(required)* | Kubernetes node that owns the published `ResourceSlice`. |
| `DRA_EXAMPLE_DEVICE_METADATA` | `0` | Set `1` to expose validation identity/attributes through read-only CDI files. |
| `DRA_EXAMPLE_BINDING_CONDITIONS` | `0` | Set `1` to require simulated external attachment readiness before binding. |
| `DRA_EXAMPLE_TAINT_EFFECT` | *(unset)* | Optional fake-device maintenance policy: `None`, `NoSchedule`, or `NoExecute`. |
| `RUST_LOG` | *(unset)* | Standard `tracing-subscriber` filter, for example `info`. |

For explicit inventory policy, see [device taints](../docs/dra-device-taints.md):
`ResourcePool::inventory()` adds opt-in `InventoryDevice` metadata while
preserving existing backends and `PoolDevice` struct literals.

For asynchronous setup before Pod binding, see [device binding conditions](../docs/dra-binding-conditions.md)
and the separate validation controller. Node preparers can use
`ClaimDeviceStatusPublisher::publish_fields()` to preserve controller-owned
conditions while reporting data.

For controller-calculated pool snapshots, see [resource-pool status](../docs/dra-pool-status.md)
and the explicit `PoolStatusClient`/`pool_status` example. The alpha API is
discovered on demand and does not change the node-driver lifecycle.

For standard read-only identity and attributes inside consuming containers,
see [container device metadata](../docs/dra-container-metadata.md). Enable
`DeviceMetadataOptions::v1beta1()` explicitly and override
`ClaimPreparer::prepare_with_metadata` to select backend attributes; existing
preparation APIs and struct literals remain compatible.

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
host tooling, not a project dependency (the installed helper was verified at
`1.6.0-k8s-versioned` on 2026-10-23). It
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

The task builds the linux/arm64 fixture image once using the canonical
`docker.io/library/k8s-device-plugin-dra-example:latest` reference for both
build and import, avoiding a stale short-name alias in the host store. It
starts each profile in
turn, imports the image, applies and restarts the validation DaemonSet, waits
for rollout, and invokes the core smoke fixture. The v1.37 profile also runs
the extended-resource smoke fixture. It verifies that each server is the
expected patch version and has Rosetta disabled. Apple Container can assign a
fresh guest IP on restart. Every start, including an already-running VM and
baseline restoration, goes through `container k8s-versioned start --name`.
The maintained helper validates the native profile, repairs the guest
`admin.conf` endpoint, reconciles kube-proxy's endpoint, waits for stable node
and DNS readiness, and refreshes the host kubeconfig before returning.

The matrix prints driver logs before stopping a profile, including on smoke
failure, so prepare/unprepare events remain available in the run output.
After preflight, success or failure triggers restoration: stop the v1.37
profile, then restore `dra-validation` (v1.36.1) and verify its server version.
If stopping v1.37 fails, restoration does not start v1.36 concurrently. A
restoration failure makes an otherwise successful run fail; an earlier error
keeps its original exit code. Preflight failures leave cluster state alone.

This is an opt-in local integration gate. Its VM-free orchestration and
failure-path tests run in CI and via `mise run dra-test-matrix`; the live
matrix is not part of `mise run ci`. Set `DRA_E2E_TAINTS=1` to include the
opt-in [device-taint fixture](../docs/dra-device-taints.md) on both profiles,
including administrator-rule checks on v1.37. Set `DRA_E2E_BINDING=1` to include
the [binding-condition fixture](../docs/dra-binding-conditions.md). For gated
v1.37 PodGroup sharing, see [Workload ResourceClaims](../docs/dra-workload-claims.md)
and `DRA_E2E_WORKLOAD=1` with the isolated advanced profile. Its configuration
transforms are tested with `mise run dra-test-profile-config`.
Use `DRA_E2E_POOL_STATUS=1` on that profile for the
[resource-pool count fixture](../docs/dra-pool-status.md).
Use `DRA_E2E_METADATA=1` for the native
[metadata fixture](../docs/dra-container-metadata.md), with
`DRA_E2E_METADATA_SHARED=1` on the advanced v1.37 profile to include shared claims.
Set `DRA_E2E_SKIP_BUILD=1` only when the
current local `docker.io/library/k8s-device-plugin-dra-example:latest` image
has already been built.

### Third-review regression acceptance

The serial matrix also runs `hack/e2e-first-available.sh` on both native
profiles. It selects a top-level request in the consumer, requires metadata to
be disabled, and checks the physical CDI marker. To include negotiated health
watches, set `DRA_E2E_RESOURCE_HEALTH=1` when running
`hack/validate-k8s-versions.sh`; the image then compiles `resource-health` while
backend reporting remains disabled by default. The health fixture temporarily
enables the backend, checks a real kubelet watch and `Healthy` Pod resource
status, then removes its override and waits for the default backend again.
Existing metadata/health overrides are refused rather than overwritten.

For a manual image build, use `--build-arg CARGO_FEATURES=resource-health` with
`dra/Dockerfile`. `DRA_EXAMPLE_RESOURCE_HEALTH=1` is a validation-backend option;
a real driver must implement its own hardware monitor. The matrix's
`DRA_E2E_IMAGE` selects the exact image loaded and deployed on each profile.
