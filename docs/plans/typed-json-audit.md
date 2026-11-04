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

## Protocol models (2026-11-04)

Task `k8s-device-plugin-rs-420` replaces the remaining fixed-schema runtime
JSON with private models. `dra/src/metadata/wire.rs` owns DeviceMetadata
v1beta1 and journal v1. It reuses Kubernetes `ObjectMeta` and
`DeviceAttribute`, and adapts the existing core `ClaimRef` and
`PreparedDevice` vocabulary without adding public serialization contracts.
`dra/src/cdi.rs` owns the CDI 0.8 environment/mount projection, shared as a
private source module with the minimal and accelerator examples. It is not
an incomplete public general-purpose CDI API.

Struct field order and ordered maps preserve canonical JSON key ordering;
optional fields retain their omission rules. Pre-migration golden journals
embed exact document/spec strings, including their newlines. Tests seed the
store with those artifacts, retry without replacing metadata inodes, and
clean them through a fresh runtime. Journal decoding rejects unknown fields
and malformed fixed-schema values before deleting artifacts; exact claim
identity, request/path validation and CDI byte checks remain enforced.
Valid version-1 journals remain compatible. Corrupt or extended journals are
retained for inspection rather than partially interpreted for deletion.

The intentional JSON boundaries above remain unchanged.
See [the implementation plan](420-typed-wire-models.md) for migration proof.

## Original audit validation (2026-10-25)

Stable-Rust `mise run ci` passed: 176 workspace tests, 102 optional
resource-health/example tests, and 30 validation-tooling tests, plus format
and Clippy checks. This includes raw patch omission assertions, taint/binding
normalization, status conflicts and field preservation, and metadata recovery.
Runtime construction and decoding of metadata/CDI/journals were not changed;
no new live-cluster run was needed for these fixture replacements.
