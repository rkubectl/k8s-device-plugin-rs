# Dynamic Resource Allocation (DRA) support — design

Status: **Phase 2 reconciliation implemented**. The compatibility target is
Kubernetes v1.36 using stable `resource.k8s.io/v1` with the default DRA
configuration. Live baseline validation completed on Kubernetes v1.36.1
linux/arm64 through the checked-in smoke path. The native v1.36.1/v1.37.0 profiles also passed the guarded matrix with
opt-in device-taint and binding-condition fixtures on 2026-10-23. See
[cluster validation](#cluster-validation) for the exact exercised path and
remaining production boundaries.

## Background

The framework currently implements the classic [Device Plugin
API](https://kubernetes.io/docs/concepts/extend-kubernetes/compute-storage-net/device-plugins/)
(`core`/`proto`/`lib`, see root [README.md](../README.md)). That work is
essentially complete (epic `o99`, only validation — `o99.12` — still open).

[Dynamic Resource Allocation](https://kubernetes.io/docs/concepts/scheduling-eviction/dynamic-resource-allocation/)
(DRA) is Kubernetes' newer resource-scheduling model: instead of a plugin
answering kubelet's `Allocate` call synchronously, a driver publishes its
devices as `ResourceSlice` API objects, the **scheduler** decides which
devices satisfy a pod's `ResourceClaim`s, and kubelet calls the driver's
`NodePrepareResources` RPC with a claim reference for the driver to resolve
and turn into container-visible devices (CDI names, mounts, etc.).

This is exploratory, post-POC scope — see project memory: the customer POC on
the classic Device Plugin API takes priority, and this design deliberately
does not gold-plate DRA support before that ships.

### Compatibility baseline

The supported compatibility target for this work is Kubernetes **v1.36** with
the stable `resource.k8s.io/v1` DRA API and default DRA feature configuration.
This is deliberately narrower than every capability exposed by a newer
cluster: Phase 1 uses the core ResourceSlice/ResourceClaim allocation and
kubelet prepare/unprepare path only. It does not rely on extended-resource
integration, device taints, device-health reporting, partitionable devices,
consumable capacity, or other optional DRA APIs. Driver-owned
`ResourceClaim.status.devices` reporting is an explicit exception: it is beta
and default-on in v1.36, then GA in v1.37; a v1.36 operator that disables
`DRAResourceClaimDeviceStatus` must not enable the publisher.

The v1.36 target is both a documented contract and a completed live-cluster
test result. On 2026-09-01, Kubernetes v1.36.1 on linux/arm64 completed the
checked-in ResourceSlice-to-CDI smoke path under Apple Container with Rosetta
disabled. Beads task `k8s-device-plugin-rs-qt7.2` records that evidence.

### Reference material

This design leans on the official KEP and its two companion Go
implementations rather than inventing the shape from scratch:

- [KEP-4381: DRA with structured parameters](https://github.com/kubernetes/enhancements/tree/master/keps/sig-node/4381-dra-structured-parameters)
  — the design doc itself, plus `components.png`/`kubelet.png` diagrams of
  how the scheduler, kubelet, API server, and drivers interact.
- [`k8s.io/dynamic-resource-allocation`](https://github.com/kubernetes/kubernetes/tree/master/staging/src/k8s.io/dynamic-resource-allocation)
  — the official (no-compatibility-guarantee) Go helper library for driver
  authors. Two packages map directly onto this design:
  - [`kubeletplugin`](https://github.com/kubernetes/kubernetes/tree/master/staging/src/k8s.io/dynamic-resource-allocation/kubeletplugin) —
    `noderegistrar.go` (pluginwatcher registration) + `registrationserver.go`
    + `draplugin.go` (the `NodePrepareResources`/`NodeUnprepareResources`
    harness). This is the direct analog of our proposed
    `DraRegistrationServer` + `DraPluginService` + `DraPlugin::run()`.
  - [`resourceslice`](https://github.com/kubernetes/kubernetes/tree/master/staging/src/k8s.io/dynamic-resource-allocation/resourceslice) —
    `resourceslicecontroller.go` + `tracker/` is the analog of our
    `ResourceSlicePublisher`, but production-grade: workqueue-based,
    per-pool sync delay, and a mutation cache to absorb informer lag. Our
    reconciler adopts its desired-state, generation/count, and safe-delete
    model; an informer-backed workqueue and mutation cache remain future
    production scalability work (see [Phasing](#phasing)).
- [`kubernetes-sigs/dra-example-driver`](https://github.com/kubernetes-sigs/dra-example-driver)
  — reference driver built on the helper library. It ships **three**
  binaries: `dra-example-kubeletplugin` (the node-local DRA driver — what
  our `dra` crate's Phase 1 corresponds to), `dra-example-controller` (an
  *optional* extension point for the `BindingConditions` plugin, not core
  allocation — structured parameters put allocation in the scheduler, so no
  driver-side allocation controller is required), and `dra-example-webhook`
  (a validating admission webhook, out of scope here). Phase 1 only needs
  the kubeletplugin equivalent.

## Why this isn't just "a new proto to compile"

The Device Plugin API is self-contained: plugin ↔ kubelet, one gRPC service,
one registration call. DRA pulls in a materially different subsystem:

| Concern | Device Plugin API (today) | DRA |
|---|---|---|
| Registration | Plugin is the gRPC **client**; calls kubelet's `Registration.Register` on `/var/lib/kubelet/device-plugins/kubelet.sock` | Inverted: plugin **serves** `Registration` (`GetInfo`/`NotifyRegistrationStatus`) on a socket under `/var/lib/kubelet/plugins_registry/`; kubelet's pluginwatcher connects to it |
| Main service | `DevicePlugin` (`ListAndWatch`, `Allocate`, ...) served on `/var/lib/kubelet/device-plugins/` | `DRAPlugin` (`NodePrepareResources`/`NodeUnprepareResources`) served on `/var/lib/kubelet/plugins/<driver-name>/`, endpoint advertised via `GetInfo` |
| Inventory publishing | Pushed to kubelet directly over the socket (`ListAndWatch` stream) | Published as `ResourceSlice` objects on the **API server** — requires a real Kubernetes client, not just a kubelet socket |
| Allocation decision | Made by the plugin itself (`allocate`/`preferred_allocation`), called synchronously by kubelet | Made by the **scheduler**, written into `ResourceClaim.status.allocation`. The plugin's `NodePrepareResources` only receives a claim reference (namespace/uid/name) and must read the claim back from the API server to see what was allocated |
| Health | Polled via `ListAndWatch` diffing (already built) | Optional `DRAResourceHealth.NodeWatchResources` stream, implemented behind the `resource-health` Cargo feature |

So DRA needs a Kubernetes API client (list/watch `ResourceClaim`, create/
update/delete `ResourceSlice`) in addition to the kubelet-facing gRPC
plumbing — a different dependency footprint than `lib` has today (`tonic`/
`tokio` only, no `kube`/`k8s-openapi`).

## Non-goals (for this phase)

- Supporting DRA API versions other than `v1` (no `v1beta1` compatibility
  shim for pre-1.34 clusters).
- Enabling Kubernetes resource-health feature gates on older clusters; the
  optional runtime protocol itself is implemented behind a Cargo feature.

These are explicitly future phases, not rejected — see [Phasing](#phasing).

## Proposed crate shape

Keep the existing `core`/`proto`/`lib` separation intact for classic
device-plugin users; don't force `kube`/`k8s-openapi` onto them. Add a new
workspace crate, `dra`, that relates to `core`/`proto` the same way `lib`
does today:

```
core   — extend with DRA-facing pure types/traits (no gRPC, no k8s client)
proto  — add a `dra` module, gated behind a `dra` cargo feature, compiling
         dra/v1/api.proto and pluginregistration/v1/api.proto; the additive
         `dra-health` feature compiles dra-health/v1alpha1/api.proto
dra    — new crate: the DRA runtime (registration server, DRAPlugin service,
         ResourceSlice publisher, ResourceClaim resolver). Depends on core,
         proto (dra feature), tonic, kube, k8s-openapi.
```

`lib` and its consumers (including `example/`) are untouched by this work.

The `proto/kubelet` submodule already vendors the relevant `.proto` files
(`dra/v1`, `dra/v1beta1`, `dra-health/v1alpha1`, `pluginregistration/v1{,beta1,alpha1}`)
even though `build.rs` only compiles device-plugin `v1beta1` today. Only
`dra/v1/api.proto` and `pluginregistration/v1/api.proto` are compiled for
this phase; the rest stay vendored but unused until a later phase needs them.

## `core` additions

Mirror the existing `DeviceDiscovery`/`DeviceAllocator`/`K8sDevicePlugin`
pattern, shaped for claims instead of raw device-ID lists:

```rust
/// A device this driver can offer, with the attributes/capacity used for
/// CEL-based selection in DeviceClasses/claims — richer than the classic
/// plugin's `Device` (id + health + paths).
pub struct PoolDevice {
    pub name: String,
    pub attributes: BTreeMap<String, AttributeValue>, // string/int/bool/version
    pub capacity: BTreeMap<String, Quantity>,
    pub health: Health, // reuses core::Health
}

#[async_trait]
pub trait ResourcePool: Send + Sync {
    /// Devices this driver currently offers, keyed by pool name.
    async fn devices(&self) -> BTreeMap<String, Vec<PoolDevice>>;
}

/// What NodePrepareResources needs resolved for one already-allocated claim.
pub struct ResolvedClaim {
    pub claim: ClaimRef,               // namespace/uid/name
    pub devices: Vec<AllocatedDevice>, // pool_name, device_name, request_name
}

#[async_trait]
pub trait ClaimPreparer: Send + Sync {
    /// Prepares every claim needed by a pod in one batch — matching the
    /// batch shape of the wire-level `NodePrepareResourcesRequest` and the
    /// upstream Go helper's `PrepareResourceClaims(claims)` (see reference
    /// material above), rather than one claim per call.
    ///
    /// Must be idempotent: kubelet may call this again for an
    /// already-prepared claim, e.g. after the driver restarts.
    async fn prepare(
        &self,
        claims: &[ResolvedClaim],
    ) -> BTreeMap<ClaimRef, Result<Vec<PreparedDevice>, PrepareError>>;

    async fn unprepare(&self, claim: &ClaimRef) -> Result<(), PrepareError>;
}

pub trait DraDriver: ResourcePool + ClaimPreparer {
    fn driver_name(&self) -> &str;
}
```

`PreparedDevice` plays the role `ContainerAllocation` plays today. DRA
expects CDI device names as the primary artifact back from
`NodePrepareResources`, rather than the raw host-device-path list the
classic `Allocate` RPC uses. Per upstream guidance, a driver should also
defensively verify a device isn't already in use by some *other* claim
before handing it out — the scheduler avoids double-booking, but the driver
is the last line of defense if something upstream went wrong. If a backend
generates CDI spec files on the fly, `unprepare` must remove them, and
regenerated specs must use a fresh unique ID rather than reusing one — some
container runtimes cache CDI specs and won't reliably reload a reused ID.

## `dra` crate runtime components

- **`DraRegistrationServer`** — serves `pluginregistration::v1::Registration`
  on `/var/lib/kubelet/plugins_registry/<driver-name>-reg.sock`. `GetInfo`
  reports `type: "DRAPlugin"`, the real plugin socket as `endpoint`, and
  `supported_versions: ["v1.DRAPlugin"]`, adding
  `"v1alpha1.DRAResourceHealth"` only for a backend that opts into health reporting.
  The AF_UNIX path-length problem this socket
  name has to fit in is the same one `lib`'s `sanitize_socket_name` already
  solves for the classic plugin (truncate + disambiguating hash within the
  108-byte `sun_path` budget) — reuse that approach rather than
  re-deriving it. Upstream's `kubeletplugin.RollingUpdateRegistrarSocketFile`
  extends the same idea further by folding in the driver pod's UID so an
  old and new driver pod can hold distinct registration sockets during a
  rolling update; not needed for Phase 1's single-replica DaemonSet
  deployment, but worth keeping in mind for Phase 2+ if rolling updates
  become a requirement.
- **`DraPluginService`** — serves `dra::v1::DRAPlugin` on
  `/var/lib/kubelet/plugins/<driver-name>/plugin.sock`. `NodePrepareResources`
  reads the referenced `ResourceClaim` through a bounded, retrying
  `kube::Api` resolver, builds a `ResolvedClaim`, and calls
  `ClaimPreparer::prepare`.
- **`ResourceSlicePublisher`** — lists the local driver's current slices,
  derives the complete desired inventory from `ResourcePool::devices()`, and
  reconciles it authoritatively. Pools are split at 128 devices per slice;
  all slices for a changed pool receive one new generation and the same total
  count. Desired slices are created or updated before stale ones are deleted.
  Stable bounded names include an identity hash and explicit chunk index; legacy
  names migrate create-before-delete. Pool ownership is checked before updates,
  and stale deletes carry the listed UID and resourceVersion as preconditions.
  The serialized poll loop coalesces each snapshot and retries API failures
  with bounded exponential backoff.
- **`DraPlugin::run()`** — lifecycle harness analogous to today's
  `DevicePlugin::run()`: bind the DRA service before exposing registration,
  spawn both gRPC servers, recreate the registration
  socket if kubelet's pluginwatcher rescans, drive the slice publisher off
  the same discovery source used by `ResourcePool`. Shared socket startup
  serializes contending plugin instances, refuses to replace an active socket,
  and recovers a stale socket only after a refused connection proves it is no
  longer serving.
- **`DraResourceHealthService` (optional)** — when the `resource-health`
  Cargo feature is enabled, the shared plugin socket also serves
  `DRAResourceHealth.NodeWatchResources`. A backend opts in by implementing
  `ResourceHealthReporter` and constructing with
  `DraPlugin::with_resource_health`; it sends bounded, source-scoped snapshots
  through a channel. The service forwards every buffered final report, then
  ends a completed or failed monitor with `Unavailable` so kubelet reconnects.
  `ResourcePool` and `PoolDevice::health` are never treated as this stream's
  source of truth.
- **`ClaimDeviceStatusPublisher`** — an explicit backend-owned publisher for
  allocated-device configuration and diagnostics. It validates a claim UID and
  each driver/pool/device/share allocation key before merging the caller's
  status entries into the current list. A resource-version precondition and
  bounded conflict retries preserve omitted and other drivers' entries under
  concurrent updates. Each retry revalidates identity and allocation. The
  backend may call it from preparation or a separate monitor; it is neither
  inferred from `ResourcePool` nor coupled to resource-health streaming. On
  Kubernetes v1.36 it relies on the default-on beta
  `DRAResourceClaimDeviceStatus` gate and node-aware synthetic authorization;
  it is GA in v1.37.

## Phasing

1. **Phase 1** — registration server + `DRAPlugin` service + a DRA API `v1`
   `ResourceSlicePublisher`. Enough to demo structured-parameter allocation
   end-to-end for the POC.
2. **Phase 2 (implemented)** — authoritative `ResourceSlice`
   splitting/reconciliation for pools exceeding the per-slice device limit,
   bounded publisher backoff, and bounded/retrying `ResourceClaim`
   resolution. The current poller serializes full snapshots; an informer
   workqueue and mutation cache are intentionally deferred until device scale
   or API-server load demonstrates that they are needed.
3. **Phase 3 (implemented, opt-in)** — `DRAResourceHealth` streaming with
   independent backend reports, bounded forwarding, and reconnect behavior.
4. **Phase 4 (partially implemented)** — driver-owned ResourceClaim device
   status publishing with allocation validation, idempotent conditional merge
   patches, and v1.36 node-aware RBAC. Kubernetes v1.37 extended-resource allocation
   is covered by a separate fixture: it maps a `DeviceClass` to an extended
   resource and proves the generated-claim CDI path without a workload-authored
   `ResourceClaim`. The [extended-resource guide](dra-extended-resources.md)
   documents the driver implementation, consumer manifests, operational
   inspection, and classic/DRA migration boundary. Remaining Phase 4 scope is
   `v1beta1` compatibility (only if a target cluster needs pre-1.34 support)
   and a deployable `dra-example` crate mirroring `example/`.
   The selected advanced-adapter cycle now includes completed opt-in
   [device taints](dra-device-taints.md) and
   [binding conditions](dra-binding-conditions.md), with native acceptance
   on both pinned profiles. [Workload ResourceClaims](dra-workload-claims.md)
   also passed the shared lifecycle on an isolated gated v1.37 profile.
   [Resource-pool status](dra-pool-status.md) now supplies an opt-in client and
   verified controller snapshots on that profile. The four selected adapters
   and remaining deferred scope are recorded in [the tracked plan](plans/dra-next-adapters.md).
   The next selected [container-metadata adapter](dra-container-metadata.md)
   adds opt-in read-only identity and scalar attributes, preserving current
   preparation APIs and default behavior.

## Deployment note: no rolling updates in Phase 1

Per `kubeletplugin` package docs, kubelet historically does not support
rolling updates of node plugins: the DaemonSet must run with `maxSurge: 0`
so the old driver pod fully terminates (unregistering) before the new one
starts, causing a short gap where pods depending on this driver's claims
can't start or finish cleanup. Kubernetes 1.33+ adds a "seamless upgrade"
path that avoids this, but a driver can't tell in advance whether the node's
kubelet supports it. For Phase 1, document `maxSurge: 0` as a hard
requirement; revisit seamless upgrades only if the target cluster is
confirmed to run 1.33+.

## Production deployment and lifecycle gates

The checked-in DaemonSet is a validation fixture, not a generic production
chart. A driver deployment must supply a dedicated service account, immutable
image reference, hardware-specific node selection and host mounts, the least
Linux privilege compatible with its device interface, and resource limits. The
runtime requires `get` on `ResourceClaim` and `Node`, plus
`get/list/create/update/patch/delete` on `ResourceSlice`; it does not need
cluster-wide claim listing or watching. Drivers that opt into
`ResourceClaim.status.devices` additionally require `get,patch` on
`resourceclaims/status` and `associated-node:patch` on the synthetic
`resourceclaims/driver` subresource with a `resourceNames` restriction to their
driver name. A cluster-scoped role remains necessary
because slices are cluster-scoped and claims may belong to workloads in any
namespace. RBAC cannot restrict a shared DaemonSet service account to only the
local node's slices, so a cluster-owned Validating Admission Policy or
equivalent admission control must enforce the expected driver name and Node
owner reference for slice mutations.

The runtime exposes [`DraPluginLivenessProbe`](../dra/src/health.rs), which
checks both node-local sockets with real gRPC calls: `Registration.GetInfo`
must advertise the expected driver name, `v1.DRAPlugin` service and plugin
endpoint, and `DRAPlugin.NodePrepareResources`
must accept an empty, non-mutating batch. The example image executes that probe
for startup, liveness, and readiness. A task exit in the registration server,
DRA service, or publisher already makes `DraPlugin::run()` abort and reap the
other tasks before returning an error, allowing the DaemonSet's restart policy
to recover. Socket-file existence alone is not a valid liveness signal, and a
successful probe must be paired with a `ResourceSlice` check for inventory
readiness.

Before a production release, validate the real driver on multiple eligible
nodes: every ready DaemonSet pod must answer the probe and own a local slice;
drain and uncordon a maintenance node while a different node retains a
consumer; and perform an immutable-image upgrade while a consumer remains
prepared. Never remove or drain a node with an in-use driver until kubelet's
`dra_resource_claims_in_use` metric for that driver is zero and driver logs
show unprepare completion. Retain `maxSurge: 0` for all of these gates. A
target kubelet's seamless-upgrade behavior must be demonstrated with the real
driver before allowing a surge rollout.

## Cluster validation

The Phase 1 fixture was validated on single-node Kubernetes **v1.36.1** and
**v1.37.0 linux/arm64** clusters, with the default kubelet configuration (no
additional DRA feature-gate overrides). The local Apple Container validation
profiles use a separately installed `k8s-versioned` helper, which selects a
pinned `kindest/node` image, passes that version into kubeadm, persists the
kind restart configuration, and sets Rosetta to false. `dra-validation`
(v1.36.1) and `dra-validation-137` (v1.37.0) are started on demand rather than
run together. This is a local validation mechanism, not a repository
dependency. In both environments, the API server exposed the stable
`resource.k8s.io/v1` `DeviceClass`, `ResourceClaim`, and `ResourceSlice`
resources.

The checked-in [`dra/hack/e2e-smoke.sh`](../dra/hack/e2e-smoke.sh) creates a
`DeviceClass`, allocates one `widget-0` device through a `ResourceClaim`, and
starts a consumer that asserts the CDI-provided `DRA_E2E_DEVICE=widget-0`
marker. The live run confirmed exactly one `ResourceSlice` for
`dra.example.com` / `widget-pool`, successful pluginwatcher registration,
`NodePrepareResources`, container startup, and `NodeUnprepareResources` after
consumer deletion. The v1.36.1 run reported `DRA_E2E_DEVICE=widget-0` and
deleted the consumer successfully.

Kubernetes v1.37 additionally completed
[`dra/hack/e2e-extended-resource.sh`](../dra/hack/e2e-extended-resource.sh):
the fixture mapped the DeviceClass to `dra.example.com/widget`, requested that
resource directly in a Pod container, observed the scheduler-generated
ResourceClaim reach the same CDI path, and verified its cleanup after Pod
deletion. This optional v1.37 capability is not part of the v1.36 baseline.

For that Apple Container run, the host's existing 19 GiB `target/` directory
made `container build` stall while it prepared the context despite the
repository `.dockerignore`; `cargo clean` made the same build complete in 125
seconds. The versioned helper adds the `docker.io/library/...` CRI alias when
it imports a short local image name, so kubelet can resolve the driver image
on current containerd. These are host-tooling details, not DRA runtime
failures.

With `maxSurge: 0`, a DaemonSet restart fully terminated the old driver before
starting its replacement. The replacement re-registered in about two seconds;
the already-running consumer remained healthy and kubelet did **not** reissue
`NodePrepareResources` for its in-use claim during the observation window.
The replacement did receive `NodeUnprepareResources` when that consumer was
deleted. Backends must therefore keep `prepare` idempotent for a potential
future retry, but should not rely on restart to replay preparation.

Kubelet's grpc-go Unix-socket client sends the percent-encoded registry socket
path as the HTTP/2 `:authority`. Upstream `h2` rejects that malformed authority
before Tonic can dispatch the RPC, so this workspace pins a narrow
[`rkubectl/h2`](https://github.com/rkubectl/h2) fork: it drops only UDS-shaped
invalid authorities and preserves normal `PROTOCOL_ERROR` handling for other
malformed values. The override is required for real kubelet interoperability,
not for Tonic-to-Tonic tests.

The fork now tracks upstream `h2` v0.4.20 on the
`uds-authority-compat-0.4.20` branch. The original compatibility revision is
preserved; the refreshed branch adds focused authority regression coverage
and [maintenance notes](https://github.com/rkubectl/h2/blob/v0.4.20%2B20261023/UDS-COMPAT.md).

On 2026-10-10, the refreshed pin passed the fork's separate library/doc and
integration suites, strict library Clippy, and workspace CI. A native ARM64
image passed real kubelet registration, RPC liveness, ResourceSlice publication,
claim preparation, CDI injection, and unpreparation on Kubernetes v1.36.1 and
v1.37.0. The v1.37.0 extended-resource fixture also passed generated-claim
cleanup. Both profiles had `rosetta=false`; validation restored v1.36.1 as the
sole running cluster. The matrix used guarded `k8s-versioned` restarts through
its `CONTAINER` override to repair kubeconfig endpoints after VM IP changes.

Cargo patches apply only at the workspace/application root. Building this
repository uses the override automatically; an application consuming the DRA
crate from Git or crates.io must repeat the exact pinned `[patch.crates-io]`
entry from the root `Cargo.toml`. The [crate guide](../dra/README.md) is the
authoritative copy-and-paste instruction. This is a release constraint, not a
hidden implementation detail.

### h2 fork status (2026-10-23)

Retain the compatibility override. The annotated snapshot tag
[`v0.4.20+20261023`](https://github.com/rkubectl/h2/releases/tag/v0.4.20%2B20261023)
and remote `uds-authority-compat-0.4.20` branch both resolve to
`e80d915d7468bc7009d3d38acfcaebc83b747f4b`. The manifest and downstream
instructions select the tag; `Cargo.lock` records the same exact commit.
This replaces the SHA selector without changing the fork's source or its
internal `0.4.20` package version. No source refresh is needed for the latest
released upstream version.

Snapshot tags use `v<upstream-version>+<YYYYMMDD>`, with `.1`, `.2`, and so
on for additional snapshots on the same date. Create annotated tags recording
the upstream base, fork commit, and validation evidence. Never move or reuse a
published tag. The date identifies the snapshot tag, while validation dates
remain explicit in the annotation and documentation.

- [Upstream v0.4.20](https://github.com/hyperium/h2/releases/tag/v0.4.20),
  published 2026-10-06, remains the latest release.
  [PR #487](https://github.com/hyperium/h2/pull/487) remains open and unmerged.
  Both the [released server](https://github.com/hyperium/h2/blob/b0e890842ac16d3c76836fd4fcc06421acb15fa7/src/server.rs#L1703-L1711)
  and [master at e64c1662](https://github.com/hyperium/h2/blob/e64c1662ebb14794fc19cbcbb5b5dd0f67925f27/src/server.rs#L1703-L1711)
  still reject an authority that fails URI parsing.
- The complete pluginwatcher `dial` function in
  [Kubernetes v1.36.5](https://github.com/kubernetes/kubernetes/blob/v1.36.5/pkg/kubelet/pluginmanager/operationexecutor/operation_generator.go#L191-L209),
  [v1.37.1](https://github.com/kubernetes/kubernetes/blob/v1.37.1/pkg/kubelet/pluginmanager/operationexecutor/operation_generator.go#L191-L209),
  and [master at dc5eeea8](https://github.com/kubernetes/kubernetes/blob/dc5eeea813679154dce3bf511ab011221ebd67a1/pkg/kubelet/pluginmanager/operationexecutor/operation_generator.go#L191-L209)
  still passes a bare socket path to `grpc.DialContext` with a custom Unix
  dialer, without an explicit authority or Unix-scheme target. The separate
  [DRA resource client](https://github.com/kubernetes/kubernetes/blob/dc5eeea813679154dce3bf511ab011221ebd67a1/pkg/kubelet/cm/dra/plugin/dra_plugin_manager.go#L405-L420)
  already uses a `unix:` target; that does not fix pluginwatcher registration.

Cleanup task `k8s-device-plugin-rs-9uf.14` remains deferred. Removal requires a
compatible released h2 fix or a released kubelet pluginwatcher fix applicable
to the supported cluster baseline, followed by a smoke test without the patch
that proves registration, claim preparation/CDI injection, and unpreparation.
The tag migration passed `cargo test --locked -p k8s-device-plugin-dra
--all-targets`, with Unix-socket tests run outside the restrictive sandbox.
The lockfile differs only in the h2 source selector; the resolved commit and
all dependency versions are unchanged. No live cluster tests were rerun. The
last recorded kubelet validation remains the 2026-10-10 v1.36.1/v1.37.0 run
above.

### Guarded matrix validation (2026-10-23)

Task `k8s-device-plugin-rs-m4z` moved every matrix start and restoration to
`container k8s-versioned start --name`, including already-running profiles.
The installed helper was `1.6.0-k8s-versioned` (`9a1b26b`) with Container
CLI/server 1.6.0. The helper owns guest/host kubeconfig recovery, kube-proxy
reconciliation, and stable node/DNS readiness. The matrix now preserves
failure codes, reports failed restoration, refuses concurrent restoration
when stopping v1.37 fails, and prints driver logs before stopping profiles.
Twelve VM-free orchestration/failure tests passed and run in CI.

The accepted live run used a fixed copy of the checked-in matrix (SHA-256
`4b4e55e6ae16849704d2459d629a84b1299328c49cb2ff6e7544a891bb4ff500`)
and the freshly built native linux/arm64 fixture. Both Pods' image IDs matched
the fresh image config digest
`sha256:dc36d7836b501c069acd900f0da933d11f26b073fdac26f76141dc30698d5ab1`.
Build/import use the canonical
`docker.io/library/k8s-device-plugin-dra-example:latest` reference, avoiding
ambiguity with a separately stored older short-name image.

The v1.36.1 profile moved from `192.168.64.23` to `.24`; v1.37.0 moved
from `.22` to `.25`. Both completed real kubelet registration, RPC liveness,
ResourceSlice publication, claim preparation, CDI injection, and unpreparation.
The v1.37 extended-resource fixture also completed generated-claim cleanup.
Captured driver logs contain prepare/unprepare events for all three claims.
The matrix exited successfully and restored v1.36.1 at `.26`, Ready with a
passing driver RPC probe, as the only running VM after stopping the build
helper. All seven profiles, including both validation targets, reported
`rosetta=false`. No Rosetta or feature-gate changes were needed.

An earlier exploratory run was discarded: the host had distinct old short-name
and fresh canonical image entries, and editing the executing shell script
caused a trailing command error. Its cleanup restored the baseline; the
accepted rerun above used a fixed script snapshot and verified Pod digests.
See the [implementation plan](plans/m4z-guarded-restarts.md) for the checked
failure and restoration contract. This run also supplies fresh kubelet
validation for the v0.1.0 dependency/protox changes and retained h2 snapshot.

## Open risks / things to verify against a real cluster before Phase 1 is "done"

- RBAC scope needed for the `ResourceSlice`/`ResourceClaim` API client
  (namespaced vs. cluster-scoped, field selectors for node-local claims).
  Upstream's `kubeletplugin` docs recommend scoping `ResourceClaim`/
  `ResourceSlice` access to the local node via a Validating Admission Policy
  — see [`dra-example-driver`'s Helm templates](https://github.com/kubernetes-sigs/dra-example-driver/tree/main/deployments/helm/dra-example-driver/templates)
  for a concrete VAP example to adapt.

### Third-review acceptance (2026-10-25)

Beads epic `k8s-device-plugin-rs-ly1` records the third-review fixes and each
finding's proof. The completed source passed 213 all-feature tests, 208 default
feature tests, strict Clippy in both configurations, formatting, strict rustdoc,
and the doctest suite. The packaged protocol crate verified with `dra-health`
from its 14-file allowlist. The matrix tooling passed 28 orchestration tests and
five profile-configuration tests.

The native linux/arm64 validation image used config digest
`sha256:6d43c8c1ff80bb3463413b6aced5a61f0689c4ed68cccfde731b4d10ebad10a4`.
Both v1.36.1 and v1.37.0 passed the ordinary CDI lifecycle, an explicit
FirstAvailable request selection with metadata disabled, and negotiated
resource-health reporting through to `Healthy` allocated-resource status in
the consumer Pod. The v1.37.0 run also passed extended-resource consumption.
Health reporting was enabled only for its fixture and then removed. The
validation retained the h2 pin, `maxSurge: 0`, native arm64 and `rosetta=false`.
The matrix exited successfully and restored v1.36.1 as the sole running
cluster, Ready with a passing driver RPC probe and no health override. The
build helper was stopped; every profile remained arm64 with `rosetta=false`.
