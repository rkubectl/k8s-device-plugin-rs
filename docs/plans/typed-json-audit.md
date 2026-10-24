# Typed JSON audit

Reviewed on 2026-10-25. Tracking: `k8s-device-plugin-rs-wmh`.

The audit covers `json::json!`, `serde_json::json!`, and JSON value types in
maintained Rust source, tests, and examples across the workspace. Generated
protocol bindings and vendored kubelet sources are outside this scope. Graph
tools were unavailable; the audit used direct source and locked dependency
type definitions (`k8s-openapi` 0.28.0 and `kube` 4.2.0).

## Replacements

| Objects | Owning types | Changes |
| --- | --- | --- |
| Nodes and API errors | `core::v1::Node`, Kubernetes `Status`, `kube::core::Status` | Shared fixtures construct typed objects; normal callers serialize them directly. |
| Claims and allocation results | `ResourceClaim`, `ResourceClaimStatus`, `AllocationResult`, `DeviceAllocationResult`, `DeviceRequestAllocationResult` | Resolver, service, metadata, and binding-controller fixtures use the Kubernetes types. |
| Device requests | `DeviceRequest`, `ExactDeviceRequest`, `DeviceSubRequest` | Metadata fixtures construct and modify request fields directly. |
| Slices, devices, taints, and bindings | `ResourceSlice`, `List<ResourceSlice>`, `Device`, `DeviceTaint`, `OwnerReference` | Publication fixtures use typed fields for pool counts, owners, times, empty conditions, and default-false bindings. |
| API discovery | `APIResourceList`, `APIResource` | Pool-status discovery uses the existing Kubernetes discovery schema. |
| Pool-status requests and responses | `ResourcePoolStatusRequest`, its spec/status types, `PoolStatus`, `Condition` | Creation, completion, failure, and replacement fixtures use typed objects. |
| Allocated device status | `ClaimDeviceStatus` and `AllocatedDeviceStatus` | Complete status fixtures are constructed with the existing models before conversion to raw wire inputs where needed. |

The shared mock responder accepts serializable objects. Existing public
`*_json` fixture helpers remain available for compatibility and raw wire tests;
their complete base objects are constructed through typed helpers. Arbitrary
caller-supplied status/device fields are preserved rather than parsed through
a model that could discard unknown fields.

## Intentional JSON

- **Partial status merge patches:** `dra/src/device_status.rs` deliberately
  sends only identity/version preconditions and `status.devices`. Keep the
  explicit `json!` construction. A full ResourceClaim is the wrong shape.
  Tests assert that `spec`, `status.allocation`, and `status.reservedFor` are
  absent, alongside existing UID, resource-version, conflict/retry, and
  foreign-entry preservation checks.
- **Driver-defined data:** `ClaimDeviceStatus.data` is an open JSON payload,
  matching Kubernetes' raw extension field. The framework cannot substitute
  a fixed schema for all drivers.
- **Independent wire assertions:** ResourceSlice and metadata tests compare
  actual serialized output with literal expected objects. Building the
  expected side with the production model could hide serialization errors.
- **Boundary tests:** Device-status tests deliberately vary null, absent,
  shared, foreign, and concurrent fields and model array replacement in a
  merge patch. Journal tests inject unsupported versions or corrupt bytes.
  Raw representations are intentional at those boundaries, after typed base
  fixture construction where applicable.

## Schema gaps

`dra/src/metadata.rs`, `dra/src/metadata/store.rs`, and the minimal/accelerator
examples still use JSON for DeviceMetadata documents, CDI specs/edits, and
the version-1 journal. There is no matching complete model in the current
Kubernetes bindings or workspace crates. Existing `DeviceAttribute` values
are already typed before inclusion in the metadata document.

These sites are candidates for new protocol models, rather than replacements
with existing Kubernetes types. Follow-up `k8s-device-plugin-rs-420` tracks
that work. Its compatibility boundary includes JSON names, omission rules,
canonical key ordering, and exact persisted document/spec/journal bytes:
retry and restart recovery compare immutable artifacts byte-for-byte.

## Validation

Stable-Rust `mise run ci` passed: 176 workspace tests, 102 optional
resource-health/example tests, and 30 validation-tooling tests, plus format
and Clippy checks. This includes raw patch omission assertions, taint/binding
normalization, status conflicts and field preservation, and metadata recovery.
Runtime construction and decoding of metadata/CDI/journals were not changed;
no new live-cluster run was needed for these fixture replacements.
