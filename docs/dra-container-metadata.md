# Container device metadata

The DRA runtime can expose device identity and backend-selected scalar
attributes as standard read-only JSON files inside consuming containers.
Enable it explicitly; the default driver lifecycle creates no metadata files
or CDI entries. Applications need no Kubernetes API credentials to read them.

This implements the Kubernetes v1.37.0 beta metadata protocol, independently
of API-server feature gates. The transport is ordinary CDI on the existing
DRA prepare/unprepare path, so the native acceptance targets both v1.36.1
and v1.37.0. It adds no protobuf service or cluster permission.

## Driver integration

Keep implementing `ClaimPreparer::prepare` and `unprepare` as before.
`PreparedDevice`, `PoolDevice`, `AllocatedDevice` and `ResolvedClaim` retain
their public field layouts. Existing struct literals remain valid.

Attach metadata options to the runtime:

```rust,no_run
use k8s_device_plugin_dra::{DeviceMetadataOptions, DraPlugin};
# async fn start<D: k8s_device_plugin_core::DraDriver + 'static>(client: kube::Client, driver: D) -> std::io::Result<()> {
let plugin = DraPlugin::new(client, "gpu.example.com", "worker-1", driver)
    .with_device_metadata(DeviceMetadataOptions::v1beta1())?;
plugin.run().await
# }
```

`v1beta1()` explicitly selects the implemented protocol. It cannot express an
empty or unsupported version list. The same builder composes with
`DraPlugin::with_resource_health`. The unified crate re-exports
`DeviceMetadataOptions` and the new pure-core wrapper.

When enabled, the service calls `ClaimPreparer::prepare_with_metadata` once
for the whole batch. Its default calls `prepare` once and wraps the existing
results with empty attributes, exposing identity only. `Arc<T>` and trait
objects forward overrides. The runtime never calls both preparation methods
for one batch.

Override `prepare_with_metadata` to return `PreparedDeviceWithMetadata`
values from the backend's coherent preparation result:

```rust
use k8s_device_plugin_core::{AttributeValue, PreparedDevice, PreparedDeviceWithMetadata};

let device = PreparedDeviceWithMetadata::new(PreparedDevice {
    request_names: vec!["gpu".into()],
    pool_name: "pool-a".into(),
    device_name: "gpu-0".into(),
    cdi_device_ids: vec!["gpu.example.com/device=gpu-0".into()],
})
.with_attribute("model", AttributeValue::String("example".into()))
.with_attribute("firmware", AttributeValue::Version("1.2.3".into()));
```

Select attributes intentionally, normally using the same public values as the
backend's ResourceSlice inventory. Runtime-only scalar attributes are also
possible. The framework does not scrape ResourceSlices or serialize arbitrary
claim annotations/status. It retains the original UID-checked claim context
for request/annotation routing; no second claim lookup or Pod watch is needed.

Attribute keys follow DRA qualified-name rules: optional DNS domain of at most
63 bytes, then a C identifier of at most 32 bytes. At most 32 attributes are
allowed per device. String/version values are at most 64 bytes; versions must
be SemVer 2.0.0. The existing ResourceSlice attribute conversion is reused.
List attributes and arbitrary JSON payloads are outside this API.

NUMA is optional data, not a prerequisite. A backend can select a known
`resource.kubernetes.io/numaNode` integer; it must omit unknown/-1 locality.
The validation widget deliberately publishes no fabricated NUMA/PCI identity.

## Application contract

For a directly referenced claim, open:

```text
/var/run/kubernetes.io/dra-device-attributes/resourceclaims/<claimName>/<baseRequest>/<driverName>-metadata.json
```

For a claim carrying the standard `resource.kubernetes.io/pod-claim-name`
annotation, open:

```text
/var/run/kubernetes.io/dra-device-attributes/resourceclaimtemplates/<podClaimName>/<baseRequest>/<driverName>-metadata.json
```

The template path uses the Pod's local claim name rather than an unpredictable
generated ResourceClaim name. Path selection follows the pinned upstream
annotation rule, including for controller-generated/shared claims; it does not
invent an extra path category for PodGroups.

A file is a stream of versioned JSON objects. This implementation writes one
`metadata.resource.k8s.io/v1beta1` object plus a newline. Consumers should decode
objects until they find a version they understand, rather than expect an array.
The object contains `kind: DeviceMetadata`, claim name/namespace/UID/generation,
optional `podClaimName`, and one request with its device identities/attributes.
For FirstAvailable, the path and wire selector use the base request name;
the JSON request name retains the selected `request/subrequest` reference.

Open the exact path from known claim/request names. Individual files are
mounted, so directory enumeration is not the discovery contract. A container
using request A sees A's file; one using B sees B's file; a container using all
requests sees both. A container with no claim sees neither. Files are readable
by non-root users and their bind mounts reject writes, including root writes.

The service retains physical device CDI IDs and adds request-scoped metadata
CDI entries using an actual allocated pool/device identity. It never attaches
all metadata IDs to a broadly scoped physical entry. These extra wire entries
are mount artifacts; they do not allocate or prepare additional hardware or
create extra health monitors. Device/CDI listings can include the artifact
entries, so consumers must not equate wire-entry count with physical devices.

## Storage, retries and failures

The default host source is under the driver's existing plugin-data mount:

```text
/var/lib/kubelet/plugins/<driver>/dra-device-metadata/<claimUID>/<baseRequest>/metadata.json
```

CDI specs use `/var/run/cdi` by default and encode CDI 0.8.0, matching the
existing validation driver; the runtime must support that spec version. For other layouts,
`with_plugin_directories(local, host)` explicitly maps the driver-visible path
to the node-host source path, and `with_cdi_directory(path)` selects a directory
the container runtime already watches. These options require absolute UTF-8
paths without parent traversal. They do not configure kubelet socket roots or
create a new host mount. Roots are an administrator-controlled trust boundary;
normal platform aliases such as `/var/run` are resolved once per operation.
All descendants use descriptor-relative, no-follow filesystem operations.

Each UID owns an immutable version-1 journal describing its snapshot and planned files.
Unknown journal versions are refused without deleting their artifacts.
The journal is published first, complete data sources next, and CDI references
last. Initial publication uses a staged complete inode and an atomic
create-only hard link. Repeated identical preparation reuses files/CDI IDs
without replacing mounted inodes. Both attributes and preparation results
must be deterministic. Changing a committed payload produces an error and
leaves the existing snapshot intact. Generation remains 1 for this static
contract. Live metadata mutation requires a separate future API.

An immutable journal is sufficient to replay a partially published operation;
there is no separate mutable ready flag. Claim-level locks serialize prepare
and unprepare, acquired in UID order for a batch. A blocking filesystem
operation retains its lock ownership even if the calling RPC is cancelled.
The store bounds an individual artifact/journal to 16 MiB and staging-name
collision retries to 16 attempts.

Required metadata errors fail only the affected claim. Other claims retain
their individual results. The runtime does not silently return successful
hardware preparation with missing requested metadata. Hardware preparation
itself must remain idempotent across retries, including a crash before the
metadata journal is published.

Unprepare first calls the backend. On backend failure it preserves metadata.
After success it removes owned CDI references, then data and the journal.
Errors are returned per claim for retry. Cleanup needs no live ResourceClaim
object. A stale UID cannot delete a replacement claim's files, and a conflicting
or changed spec in the shared CDI directory is retained rather than deleted.
Unknown files/corrupt journals produce errors; cleanup does not glob-delete
unrelated content. Empty UID directories left after final journal removal
can be safely removed on retry.

Drain prepared claims while metadata is still enabled before disabling or
removing the driver. Disabling the feature intentionally does not sweep files
that running consumers may still use. On restart, manifests are recovered when
prepare/unprepare is retried; the framework does not assume kubelet will replay
preparation merely because the driver restarted.

PodGroup-shared ordinary claims use one UID-owned snapshot. The driver does
not count Pods: kubelet's final-consumer unprepare controls cleanup.

## Native validation

The example's `DRA_EXAMPLE_DEVICE_METADATA=1` control publishes a harmless
`model: validation-widget` attribute and uses the new preparation hook.
The normal image and deployment keep this control unset. No RBAC or feature
gate changes are required.

The local fixture requires Container and a running native profile whose name
matches `KUBE_CONTEXT`; it changes only its owned test objects and example
controls, then drains metadata before restoring defaults. It refuses existing
fixture names or pre-existing taint/binding/metadata controls.

From the repository root, build a fresh image and include acceptance in the
serial matrix:

```sh
DRA_E2E_METADATA=1 ./dra/hack/validate-k8s-versions.sh
```

To include the already-provisioned advanced profile's shared-claim regression:

```sh
DRA_E2E_METADATA=1 DRA_E2E_METADATA_SHARED=1 \
  DRA_E2E_V137_CLUSTER=dra-validation-advanced-137 \
  ./dra/hack/validate-k8s-versions.sh
```

Use `DRA_E2E_SKIP_BUILD=1` only with a current native image. The matrix runs the
shared regression only on v1.37. Metadata's ordinary cases run on both profiles:
request A/B/all/no-claim containers, non-root reads/root write rejection,
direct and template claims, FirstAvailable, driver restart with inode
preservation, claim-name reuse with a new UID, a blocked CDI write plus an
independent claim, retry recovery and final owned-artifact cleanup. The outer
matrix restores the sole v1.36 baseline and keeps Rosetta disabled.

Focused tests also cover legacy API/Arc dispatch, per-claim RPC failure and
backend cleanup ordering, symlinks/foreign files, partial publication/restart
replay, changed snapshots, and metadata/health RPC composition.

## Boundaries and references

Network data, NRI/post-CNI hooks, live mutation, alpha encodings, list/derived
attributes and consumable-capacity share IDs remain deferred. Ordinary shared
PodGroup claims are supported. The widget tests prove the generic contract;
they do not establish a production hardware backend.

The new direct dependencies are `rustix` for safe descriptor-relative filesystem
operations and `semver` for the required attribute validation. Both reuse
already-locked versions; no existing dependency version was upgraded.

See the [design and implementation plan](plans/dra-container-metadata.md), the
[pinned protocol package](https://github.com/kubernetes/kubernetes/blob/v1.37.0/staging/src/k8s.io/dynamic-resource-allocation/api/metadata/doc.go),
[beta schema](https://github.com/kubernetes/kubernetes/blob/v1.37.0/staging/src/k8s.io/dynamic-resource-allocation/api/metadata/v1beta1/types.go)
and [upstream file/CDI implementation](https://github.com/kubernetes/kubernetes/blob/v1.37.0/staging/src/k8s.io/dynamic-resource-allocation/kubeletplugin/metadata.go).

## Validation evidence — 2026-10-24

- Installed native helper: `1.6.1-k8s-versioned`, commit `6fc0528`. All profiles
  had `rosetta=false`; no cluster feature configuration was changed.
- The disposable wire-entry probe passed request A/B/all/no-claim routing,
  non-root reads, root write rejection and physical CDI delivery once on both
  standard v1.36.1/v1.37.0 profiles. Its image configuration was
  `d604165ef06c3fec8ec1d16caf541f12c9eb3e7e64313202fe89b33982203b97`.
- The complete metadata acceptance matrix exited 0 on v1.36.1 and the existing
  advanced v1.37.0 profile with image configuration
  `12cd3a50203d5d66ec0447cc9294b72ca3a473da943f7573269fedacb291c2d3`.
  Direct claims, request isolation/negative controls, templates, FirstAvailable,
  restart/inode preservation, name reuse, writer collision/recovery with an
  independent usable claim, and exact owned-artifact cleanup all passed.
- Direct/reused claim UIDs were
  `7043a175-37ee-47dd-86d2-2de3ffe34a39` /
  `d54901d5-a3d2-465c-83ee-6ab82805a86b` on v1.36, and
  `56eb5ce9-6d10-4c65-b650-98e855defede` /
  `a55ed242-fc32-48fa-a1a7-9bd6c618cfac` on v1.37.
- Shared claim UID `54bc27eb-b3d7-4de6-9026-5843d3c2b4b4` retained readable
  metadata after the first Pod was deleted; final-consumer unprepare removed
  its metadata before Group/claim cleanup. The workload smoke passed.
- Full final local CI passed: 168 workspace tests, 99 health/example tests,
  strict Clippy and formatting, 25 matrix checks and five profile-config checks.
  The DRA-only unified facade build also passed. This includes explicit
  broad-physical-entry request isolation, corrupt/future journal refusal,
  fault/restart replay, legacy/Arc dispatch and real socket health composition.
- The full matrix restored `dra-validation` v1.36.1 at `192.168.65.25`, Ready
  with real-RPC liveness and `maxSurge: 0`. It was the sole running VM; the
  builder and advanced profile were stopped. Default metadata/taint/binding
  controls were absent, and baseline metadata data/CDI files were absent.
- Final source adds the internal journal-version guard. Rebuilt image config
  `fee6bc2b504bb208b8eb6306b2499507bcf503087dbbd337fca4b1fc93d867b7`
  passed an additional v1.36 native smoke: journal version 1 was read from the
  node, A/B/all/no-claim visibility and read-only behavior passed, a driver
  restart retained the inode, and unprepare removed the data and CDI files.
  Claim UID was `cf21feaa-7991-4043-819f-fa35ad16048a`. Baseline real-RPC
  liveness/default controls/sole-VM status were reverified on that exact image.
