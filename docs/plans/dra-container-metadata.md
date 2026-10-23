# Container device metadata: design

## Primary goal

An application can read the identity and driver-selected scalar attributes of
its allocated devices from standard read-only JSON files inside its container,
without Kubernetes API credentials. A container sees only metadata associated
with the claim requests it consumes.

Status: proposed design, 2026-10-23. Design task `qt7.6.7`; implementation
`qt7.6.8`. The maintainer selected **device identity and attributes first**.
This document describes future APIs; none of the proposed APIs exists yet.
Implementation is a separate step after the design discussion.

## Phase 0: Protocol and source discovery

**How this serves the primary goal:** Establish the exact format applications
can consume and the integration points that can deliver it without breaking
existing drivers.

### Verified upstream contract

Use Kubernetes **v1.37.0**, KEP-5304's implementation, as the pinned reference.
The public observability guide calls the feature beta in v1.37; this is a
driver-side protocol with **no cluster feature gate**, not a new API-server
resource. A Rust implementation must implement the file/CDI contract itself;
it cannot call the Go helper library or assume a kubelet RPC metadata field.

| Contract | Pinned source |
|---|---|
| Container path categories, stream decoding and per-driver files | [metadata/doc.go](https://github.com/kubernetes/kubernetes/blob/v1.37.0/staging/src/k8s.io/dynamic-resource-allocation/api/metadata/doc.go), [types.go constants](https://github.com/kubernetes/kubernetes/blob/v1.37.0/staging/src/k8s.io/dynamic-resource-allocation/api/metadata/types.go#L27-L47), [helpers.go](https://github.com/kubernetes/kubernetes/blob/v1.37.0/staging/src/k8s.io/dynamic-resource-allocation/api/metadata/helpers.go) |
| `metadata.resource.k8s.io/v1beta1`, kind `DeviceMetadata` | [v1beta1/register.go](https://github.com/kubernetes/kubernetes/blob/v1.37.0/staging/src/k8s.io/dynamic-resource-allocation/api/metadata/v1beta1/register.go), [types.go](https://github.com/kubernetes/kubernetes/blob/v1.37.0/staging/src/k8s.io/dynamic-resource-allocation/api/metadata/v1beta1/types.go) |
| Explicit driver opt-in and required beta version | [draplugin.go:661-714](https://github.com/kubernetes/kubernetes/blob/v1.37.0/staging/src/k8s.io/dynamic-resource-allocation/kubeletplugin/draplugin.go#L661-L714) |
| Request grouping, encoding, read-only CDI edits and host artifacts | [metadata.go:147-417](https://github.com/kubernetes/kubernetes/blob/v1.37.0/staging/src/k8s.io/dynamic-resource-allocation/kubeletplugin/metadata.go#L147-L417) |
| Template path selection annotation | [metadata.go:91-100](https://github.com/kubernetes/kubernetes/blob/v1.37.0/staging/src/k8s.io/dynamic-resource-allocation/kubeletplugin/metadata.go#L91-L100), `resource.kubernetes.io/pod-claim-name` in [resource/v1/types.go](https://github.com/kubernetes/kubernetes/blob/v1.37.0/staging/src/k8s.io/api/resource/v1/types.go#L38-L42) |
| Subrequest normalization and CDI injection | [draplugin.go:1309-1380](https://github.com/kubernetes/kubernetes/blob/v1.37.0/staging/src/k8s.io/dynamic-resource-allocation/kubeletplugin/draplugin.go#L1309-L1380) |
| Container request filtering | [claiminfo.go:cdiDevicesAsList](https://github.com/kubernetes/kubernetes/blob/v1.37.0/pkg/kubelet/cm/dra/claiminfo.go#L214-L228), [manager.go:GetResources](https://github.com/kubernetes/kubernetes/blob/v1.37.0/pkg/kubelet/cm/dra/manager.go#L519-L602) |

The older KEP text and some Go option comments contain earlier/simplified
paths. The pinned path helpers and metadata package documentation take
precedence. There is no `DRADeviceMetadata` cluster feature gate to enable.

### Existing repository integration points

Graph tools were unavailable during this design. The bounded source fallback
read these paths directly; no graph completeness claim is made:

- `core/src/dra.rs:92-171`: `ClaimRef`, `AllocatedDevice`, `ResolvedClaim`,
  `PreparedDevice`, `ClaimPreparer::prepare` and `unprepare`.
- `core/src/dra.rs:189-201`: `Arc<T>` preparation delegation; adding a default
  method also requires forwarding overrides through this implementation.
- `dra/src/claim.rs:55-139`: one UID-checked ResourceClaim read, bounded retry
  and batch resolution. The current public projection discards annotations,
  request definitions, allocation driver identity and share IDs.
- `dra/src/service.rs`: per-claim RPC errors, one batch backend prepare call,
  existing wire conversion and concurrent unprepare; metadata integrates here.
- `proto/kubelet/pkg/apis/dra/v1/api.proto:54-79`: request-scoped device entries
  and CDI IDs, with no metadata payload. No protobuf change is required.
- `dra/examples/minimal_driver.rs:49-64,135-195`: current CDI environment
  marker, idempotent static device preparation and device-status publication.
- `dra/k8s/daemonset.yaml`: existing driver plugin-data and `/var/run/cdi`
  host mounts; `maxSurge: 0` stays unchanged.
- `dra/src/resourceslice.rs:29-48`: existing scalar attribute conversion;
  extract a shared internal conversion rather than maintaining two encoders.
- `dra/src/service/tests.rs`, `dra/src/claim/tests.rs`: copy the current
  mock-client and real Unix-socket RPC testing patterns.

Allowed existing building blocks: `ClaimPreparer`, `PreparedDevice`,
`AttributeValue`, `ClaimResolver`, `DraPluginService`, `DraPlugin`,
`serde_json`, Tokio filesystem/blocking facilities, existing kube mocks and
CDI runtime mounts. Proposed names below are design proposals, not existing APIs.

## Phase 1: Compatible backend contract and request model

**How this serves the primary goal:** Let drivers supply metadata from the
same device-preparation result while preserving their existing integration.

### Proposed additive API

Keep the public fields of `PreparedDevice`, `ResolvedClaim`, `AllocatedDevice`
and `PoolDevice` unchanged. Do not add required fields to existing literals.
Keep `core` free of Kubernetes/serialization dependencies.

Add a pure-core, non-exhaustive `PreparedDeviceWithMetadata` wrapper with
constructors/builders around:

- `device: PreparedDevice`;
- `attributes: BTreeMap<String, AttributeValue>`.

Add `ClaimPreparer::prepare_with_metadata(&[ResolvedClaim])`, returning the
same per-claim result-map shape as `prepare`, with wrapped device values. Its
default calls the existing `prepare` **once for the batch** and wraps the
results with empty attributes. `Arc<T>` forwards the new method so overrides
remain effective through trait objects. A backend overriding the hook should
factor its preparation once and return device artifacts plus attributes from
one coherent result; the runtime does not call both preparation methods.

Add a fallible `DraPlugin::with_device_metadata(options)` builder and matching
service configuration. Metadata is disabled by default. When enabled, the
service invokes only `prepare_with_metadata`; otherwise it follows today's
`prepare` path. Options explicitly select `V1Beta1` and configure the CDI
output directory, default `/var/run/cdi`. Unsupported/empty version selection
fails before socket registration or backend side effects. Initial support is
**beta only**, not an implicit latest-version alias or alpha compatibility.
The wrapper retains current resource-health configuration and re-exports
through the unified crate.

The node plugin's existing plugin-data directory is the default metadata root.
For non-default container/host mount layouts, require an explicit pair of
local and host plugin-data paths; never infer the node host path from an
arbitrary container path. Validate all roots as absolute and driver-owned.
This option does not imply general kubelet socket-root support beyond the
existing runtime's capabilities.

### Resolver context and ownership

Refactor the UID-checked ResourceClaim read into an internal context retained
through preparation. Existing public `resolve`/`resolve_all` remain compatible
projections. Metadata context includes claim identity, top-level request
names, the pod-claim-name annotation, and allocation tuples including driver
and full request/subrequest references. No extra Pod watch, ResourceSlice
lookup, broad RBAC or second racy metadata lookup is needed.

Validate every returned metadata-bearing device against allocations for this
runtime's driver and the exact pool/device/request. Deduplicate identical
memberships; conflicting attributes for the same request/device are errors.
Reject unallocated/foreign devices and malformed references before publishing
artifacts. An empty `request_names` retains its existing all-requests meaning,
expanded using the claim's actual request definitions; never infer request
scope from just the devices returned by the backend. A backend returning an
explicit request must match an allocated reference for that device.

For `gpu/fast`, the file path and wire request selector use the base `gpu`,
while the JSON request name retains `gpu/fast`. In this scope one selected
subrequest per base request is expected; inconsistent allocations fail clearly.
Initial support excludes local nonempty share IDs/consumable-capacity
allocations because the current preparation types do not represent those
identities. This restriction does **not** exclude multiple Pods sharing one
ordinary PodGroup claim UID.

Verification: legacy struct-literal compilation; default/override/Arc dispatch;
one backend batch call; direct/template context from the same UID-checked
read; mixed drivers; duplicate/foreign/empty/all-request and FirstAvailable
mapping; resource-health composition. No metadata artifacts or extra API reads
when the feature is disabled.

## Phase 2: Versioned read-only files and request-isolated CDI

**How this serves the primary goal:** Deliver a predictable application-facing
format, with each container receiving only the requests it consumes.

### Consumer contract

Direct claim path:

```text
/var/run/kubernetes.io/dra-device-attributes/resourceclaims/<claimName>/<baseRequest>/<driverName>-metadata.json
```

Template-generated claim path, selected by the standard claim annotation:

```text
/var/run/kubernetes.io/dra-device-attributes/resourceclaimtemplates/<podClaimName>/<baseRequest>/<driverName>-metadata.json
```

Files are concatenated JSON-object streams. The first implementation writes
one beta object plus a newline; consumers select an understood `apiVersion`
from the stream. Do not encode an array or an extra envelope.

```json
{"apiVersion":"metadata.resource.k8s.io/v1beta1","kind":"DeviceMetadata","metadata":{"name":"my-claim","namespace":"default","uid":"claim-uid","generation":1},"requests":[{"name":"gpu","devices":[{"driver":"gpu.example.com","pool":"pool-a","name":"gpu-0","attributes":{"model":{"string":"example"},"resource.kubernetes.io/numaNode":{"int":1}}}]}]}
```

For template claims also include `podClaimName`. Identity is framework-owned;
attributes are backend-selected, scalar, typed exactly like DeviceAttribute
(`string`, `int`, `bool`, `version`), at most 32 per device. Reuse the existing
attribute converter and validate qualified keys/types/limits. Deterministic
ordering avoids changes caused solely by iteration order.

Do not serialize arbitrary ResourceClaim annotations, status, ResourceSlices,
secrets or the backend's entire inventory. Backends should supply the public
attributes they want their applications to receive. NUMA is optional payload:
include known `resource.kubernetes.io/numaNode` if selected by the backend;
omit unknown/-1 locality. It is not a dependency of the metadata mechanism.

### Host artifacts and CDI isolation

Use a **UID-qualified** private host layout such as
`<pluginData>/dra-device-metadata/<claimUID>/<baseRequest>/metadata.json`.
The upstream namespace/name host layout is an implementation detail; the
consumer paths above are the interoperability contract. UID-qualified storage
prevents a reused claim name from overwriting an old mount's source.

Use one owned CDI spec per request, kind `<driver>/metadata`, deterministic
ID `<driver>/metadata=<claimUID>_<baseRequest>`, and a single file bind mount
with `ro,bind`. Metadata directories are 0755 and files 0644 for non-root
readers, matching upstream. The local and host path mapping must identify the
same file. CDI output must be in the runtime's existing watched host mount.

**Never append several requests' metadata IDs to one broadly scoped wire
device.** Kubelet filters the device entry before consuming all of its CDI IDs.
The proposed representation retains physical device entries and their CDI
IDs, base-normalizing explicit subrequest names. It adds one metadata-only
wire entry per base request, using a real allocated pool/device identity as
its representative, exactly one request selector and only that request's
metadata CDI ID. These are response artifacts, not extra hardware preparation
calls or allocations. This avoids duplicating a physical CDI ID when an
original device is associated with multiple requests. Do not add the metadata
IDs to the original broad entry.

An implementation spike must first verify that repeated physical identity in
these artifact entries is accepted by both kubelets/container runtimes and
preserves resource-health behavior. Test single-request, all-request and
multi-request containers, physical CDI delivery once, no cross-request files,
and unchanged hardware preparation/health subscription counts. If this wire
representation is rejected or changes required semantics, stop and revise
this design before adding persistence. Do not fall back to attaching all
metadata IDs to a broad entry. The pinned filtering code supports the proposed
routing, but this response shape has not yet been tested live.

A container consuming all requests sees all relevant files. A container using
request A must have neither request B's file nor its metadata CDI edits. A
container consuming no claim sees neither. Multiple drivers use distinct file
names/IDs. The physical device IDs retain their normal semantics.

Verification: beta-schema golden objects checked against pinned upstream types;
stream framing; annotation-based template paths; subrequest base paths;
positive/negative request isolation at the wire/CDI layer; original physical
CDI IDs retained; deterministic bytes; path traversal/symlink refusal. Backend
strings must not escape the owned roots. Application code opens the exact
known path; it does not rely on directory enumeration across file mounts.

## Phase 3: UID-owned persistence, retries and cleanup

**How this serves the primary goal:** Metadata remains consistent through
repeated prepare calls and driver restarts, and one claim cannot damage
another claim's files.

The initial contract is an **immutable preparation snapshot**, generation 1.
Repeated preparation of the same UID/request with identical canonical data
reuses existing files/CDI IDs without replacement. If a prepared claim's
payload changes, return a clear per-claim error and preserve the committed
snapshot; changing live metadata requires a future update design. Do not
replace the inode of a bind-mounted file and claim existing consumers see
an atomic update. Network/post-CNI and live updates remain out of scope.

Serialize prepare/unprepare for a UID. Preserve one backend batch preparation
call: acquire distinct claim locks in stable order for batch preparation;
unprepare uses the same locks. Avoid a per-claim backend call rewrite or
creation of two active mutexes for a UID during lock-registry pruning.

Persist an owned manifest with claim identity, canonical snapshot and planned
artifact paths before publishing CDI references. Use create-new private
staging and atomic publication for initial unmounted files; publish CDI last,
then mark the manifest ready. Never replace a ready metadata source just to
make retries appear atomic. A pending manifest supports replay after a crash
between metadata and CDI publication. Filesystem work that needs no-follow
or durable create/rename semantics may use a bounded `spawn_blocking` helper;
plain async `write` must not be treated as a transaction.

Metadata-enabled preparation fails the affected claim if validation or any
required artifact operation fails. Return no successful device entries for
that claim; other claims in the RPC keep their outcomes. This is deliberately
stricter than upstream's current best-effort metadata logging: enabled means
the application can rely on its metadata being present. Retry the idempotent
backend and owned artifact journal; do not silently unprepare successful
hardware while kubelet may retry preparation. Partial artifacts stay recorded
for replay/explicit unprepare, not an unbounded retry loop.

On unprepare, first run the backend's idempotent unprepare. If that fails,
preserve metadata for the retry. After backend success remove owned CDI
references, then files and the manifest; cleanup failure is a per-claim error
and a subsequent unprepare retries it. Cleanup uses the supplied UID and
owned manifest, without requiring the ResourceClaim still to exist. A stale
unprepare for an old UID cannot remove a new claim's artifacts.

On restart inspect only manifests under the configured private root. Validate
record identity/paths and reconstruct or finish that UID's transaction when
its operation is retried. Do not globally glob-delete CDI files or discard
unknown active claims. Ambiguous/corrupt records produce actionable errors;
they do not authorize cleanup outside the owned UID directory. Disabling the
feature must not silently delete previous metadata while consumers may still
use it; drain claims under the enabled configuration before disabling/removing
the driver. Re-enabling can recover persisted owned state.

PodGroup-shared claims use the same UID-owned snapshot. The driver does not
watch or count member Pods; kubelet's existing final-consumer unprepare owns
the lifetime. Metadata failures do not alter allocation, taints, readiness
conditions or `ResourceClaim.status.devices` ownership.

Verification: identical retries/no inode replacement, changed-payload refusal,
crash windows before/after each publication step, restart recovery, backend
and filesystem errors, concurrent prepare/unprepare, stale UID/name reuse,
missing API object on unprepare, idempotent cleanup and foreign-file retention.

## Phase 4: Native consumer acceptance and final verification

**How this serves the primary goal:** Demonstrate the promised read-only,
request-scoped files in actual containers and preserve the default baseline.

Before completing the writer, verify the metadata-only wire-entry proposal
from Phase 2 against both pinned kubelets; this is a mandatory implementation
feasibility gate, not a result established by this design-only work.

Use the existing harmless widget consumer as the reproducible first contract.
It is not evidence of a production hardware integration. Extend the example
with an explicit `DRA_EXAMPLE_DEVICE_METADATA=1` control and selected model/
identity attributes matching its published inventory. No invented NUMA node,
PCI address or hardware UUID. Default behavior and the physical CDI marker
remain unchanged. Build a fresh native arm64 image for runtime changes.

Add an opt-in matrix fixture for the standard v1.36.1 and v1.37.0 profiles:

1. Direct claim: read the exact standard path, decode beta identity/attributes,
   check UID and physical CDI delivery; non-root reading succeeds and writing
   fails even for a root consumer.
2. One claim with two requests and two containers selecting different requests:
   each sees only its own file; a third container with no claim sees neither.
   Include a mocked/shared device across requests, plus native multiple-device
   inventory where needed to exercise selector isolation.
3. ResourceClaimTemplate: use the pod-local name path and verify generated
   claim identity and `podClaimName`, without API credentials in the consumer.
4. A FirstAvailable subrequest: base request path, full reference in JSON,
   base-normalized wire filtering. Multiple selected devices retain identity.
5. Driver restart with a live consumer: preserve metadata bytes/inode/source,
   existing mount and CDI IDs; repeat prepare converges. Inject a writer
   failure and prove per-claim error/retry without harming another claim.
6. Final unprepare removes only that UID's artifacts; recreate the same claim
   name with a new UID and prove stale cleanup cannot affect it. Verify no
   host or CDI residue and restore the example's default controls.
7. On the isolated advanced v1.37 profile, regress PodGroup sharing: first-Pod
   deletion preserves remaining metadata; final-consumer cleanup removes it.
8. Run full workspace/health/example CI, strict Clippy/format, public API
   compatibility checks and VM-free matrix failure/restoration tests. Restore
   v1.36 as the sole Ready/RPC-healthy VM; all Rosetta false, `maxSurge: 0`.

Copy the existing e2e/matrix patterns rather than add another cluster manager.
This feature requires neither advanced-profile provisioning nor RBAC changes;
the advanced profile is used only for the shared-claim regression. Record
image provenance and actual live evidence in a dedicated metadata guide.
Close implementation task `qt7.6.8` only after these checks pass.

## Decisions and boundaries

| Decision | Rationale |
|---|---|
| Identity + selected scalar attributes first | Maintainer's explicit initial consumer scope. |
| Standard beta JSON and paths, runtime opt-in | Interoperable consumers, unchanged default driver behavior. |
| Wrapper plus default preparation hook | Preserve existing public struct literals and one batch preparation call. |
| Metadata from backend preparation | Explicit exposure contract; no speculative ResourceSlice scraping. |
| UID-qualified private artifacts and request-specific wire entries | Prevent name-reuse cleanup errors and cross-request metadata leakage. |
| Static snapshot initially, no live-update API | Avoid promising inode-safe updates without a designed update protocol. |
| Required metadata fails its own claim | A consumer can rely on an explicitly enabled contract. |

Deferred: network data/post-CNI/NRI hooks; live mutation/generation increments;
v1alpha1 encoding; arbitrary JSON data; list/derived attributes; consumable
capacity/share IDs; production hardware discovery; a library for decoding
metadata inside applications. NUMA support remains the existing known-scalar
attribute mechanism and is not a prerequisite.

The selected scope is sufficient to implement and validate the generic adapter.
A real application's attribute keys remain a backend contract; the framework
does not standardize vendor-specific attributes beyond existing DRA rules.
