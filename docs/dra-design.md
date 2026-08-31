# Dynamic Resource Allocation (DRA) support — design

Status: **Phase 1 implemented and live-validated** on Kubernetes v1.37.0
(linux/arm64 kind). The supported API remains `resource.k8s.io/v1`; see
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
    per-pool sync delay, and a mutation cache to absorb informer lag. Phase 1
    deliberately does not build this; Phase 2 should port its approach
    rather than re-derive one (see [Phasing](#phasing)).
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
| Health | Polled via `ListAndWatch` diffing (already built) | Separate optional streaming RPC, `DRAResourceHealth.NodeWatchResources` (deferred — see [Phasing](#phasing)) |

So DRA needs a Kubernetes API client (list/watch `ResourceClaim`, create/
update/delete `ResourceSlice`) in addition to the kubelet-facing gRPC
plumbing — a different dependency footprint than `lib` has today (`tonic`/
`tokio` only, no `kube`/`k8s-openapi`).

## Non-goals (for this phase)

- Supporting DRA API versions other than `v1` (no `v1beta1` compatibility
  shim for pre-1.34 clusters).
- Multi-slice `ResourceSlice` pooling/splitting for driver device counts
  above the per-slice limit (~128 devices).
- `DRAResourceHealth` streaming.

These are explicitly future phases, not rejected — see [Phasing](#phasing).

## Proposed crate shape

Keep the existing `core`/`proto`/`lib` separation intact for classic
device-plugin users; don't force `kube`/`k8s-openapi` onto them. Add a new
workspace crate, `dra`, that relates to `core`/`proto` the same way `lib`
does today:

```
core   — extend with DRA-facing pure types/traits (no gRPC, no k8s client)
proto  — add a `dra` module, gated behind a `dra` cargo feature, compiling
         dra/v1/api.proto and pluginregistration/v1/api.proto
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
    pub attributes: HashMap<String, AttributeValue>, // string/int/bool/version
    pub capacity: HashMap<String, Quantity>,
    pub health: Health, // reuses core::Health
}

#[async_trait]
pub trait ResourcePool: Send + Sync {
    /// Devices this driver currently offers, keyed by pool name.
    async fn devices(&self) -> HashMap<String, Vec<PoolDevice>>;
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
    ) -> HashMap<ClaimRef, Result<Vec<PreparedDevice>, PrepareError>>;

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
  `supported_versions: ["v1.DRAPlugin"]`. The AF_UNIX path-length problem this socket
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
  reads the referenced `ResourceClaim` (via a shared `kube::Api`/reflector),
  builds a `ResolvedClaim`, and calls `ClaimPreparer::prepare`.
- **`ResourceSlicePublisher`** (single-slice, this phase) — diffs
  `ResourcePool::devices()` output against the last-published state and PUTs
  one `ResourceSlice` owned by the local `Node`. No splitting, no GC beyond
  relying on the `Node` owner-reference for cleanup.
- **`DraPlugin::run()`** — lifecycle harness analogous to today's
  `DevicePlugin::run()`: spawn both gRPC servers, recreate the registration
  socket if kubelet's pluginwatcher rescans, drive the slice publisher off
  the same discovery source used by `ResourcePool`.

## Phasing

1. **Phase 1 (this design)** — registration server + `DRAPlugin` service +
   single-slice `ResourceSlicePublisher`, DRA API `v1` only. Enough to demo
   structured-parameter allocation end-to-end for the POC.
2. **Phase 2** — `ResourceSlice` splitting/reconciliation for pools
   exceeding the per-slice device limit; retry/backoff hardening on the
   publisher and `ResourceClaim` resolver. Model this on upstream's
   `resourceslice.Controller` (workqueue keyed by pool name, a
   sync-delay so bursts of change coalesce, and a mutation cache to absorb
   informer lag around deletes) rather than inventing a reconciliation
   strategy from scratch.
3. **Phase 3** — `DRAResourceHealth` streaming, parity with the classic
   plugin's `ListAndWatch` health-diffing behavior.
4. **Phase 4** — `v1beta1` compatibility (only if a target cluster needs
   pre-1.34 support), deployable `dra-example` crate mirroring `example/`.

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

## Cluster validation

The Phase 1 fixture was validated on a single-node `kind` cluster running
Kubernetes **v1.37.0 linux/arm64**, with the default kubelet configuration
(no additional DRA feature-gate overrides). The API server exposed the stable
`resource.k8s.io/v1` `DeviceClass`, `ResourceClaim`, and `ResourceSlice`
resources.

The checked-in [`dra/hack/e2e-smoke.sh`](../dra/hack/e2e-smoke.sh) creates a
`DeviceClass`, allocates one `widget-0` device through a `ResourceClaim`, and
starts a consumer that asserts the CDI-provided `DRA_E2E_DEVICE=widget-0`
marker. The live run confirmed exactly one `ResourceSlice` for
`dra.example.com` / `widget-pool`, successful pluginwatcher registration,
`NodePrepareResources`, container startup, and `NodeUnprepareResources` after
consumer deletion.

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

Cargo patches apply only at the workspace/application root. Building this
repository uses the override automatically; an application consuming the DRA
crate from Git or crates.io must repeat the exact pinned `[patch.crates-io]`
entry from the root `Cargo.toml`. The [crate guide](../dra/README.md) is the
authoritative copy-and-paste instruction. This is a release constraint, not a
hidden implementation detail.

## Open risks / things to verify against a real cluster before Phase 1 is "done"

- RBAC scope needed for the `ResourceSlice`/`ResourceClaim` API client
  (namespaced vs. cluster-scoped, field selectors for node-local claims).
  Upstream's `kubeletplugin` docs recommend scoping `ResourceClaim`/
  `ResourceSlice` access to the local node via a Validating Admission Policy
  — see [`dra-example-driver`'s Helm templates](https://github.com/kubernetes-sigs/dra-example-driver/tree/main/deployments/helm/dra-example-driver/templates)
  for a concrete VAP example to adapt.
