# Function ownership and conversion assessment

Tracking: `k8s-device-plugin-rs-zw5`. Baseline: v0.5.0, commit `db5d2cf`.

## Scope and evidence

The assessment covers first-party Rust in core, DRA, the classic runtime,
Quantity, facade, test helpers, examples, and the protocol build entrypoint.
Generated protobuf output and the vendored kubelet tree are excluded.

Graph project `Volumes-rust-k8s-device-plugin-rs`, full generation
`2026-11-06T22:12:25Z`, supplied the function inventory, candidate snippets and
inbound/outbound traces. Coverage checks found no recorded gaps in the bounded
first-party scopes or candidate paths. This is best-effort evidence, not proof
of completeness. Some iterator callback edges were absent and fixture helpers
were inconsistently marked as tests. Exact source/call-site searches
supplemented those cases and cross-checked top-level Rust declarations,
including the public test-helper crate.

## Architecture decision

Preserve the [original design](../dra-design.md#proposed-crate-shape): core owns
pure types/traits without generated gRPC or Kubernetes API dependencies.
Classic-only applications do not need an API schema or its dependency graph;
backends can share core types across transports. API-version and schema
coupling belongs in adapters. An optional core dependency would retain a light
default graph but still couple core's implementation/conversion API to those
external versions, so it is not selected as a style workaround.

Rust orphan rules mean DRA cannot implement `From<core::PoolDevice>` for an
OpenAPI device, nor can classic runtime implement `From<core::Device>` for a
generated protocol device. Both source and target are foreign in those crates.
Keep those projections as adapter functions. Do not introduce wrapper types,
single-use extension traits or another crate solely to eliminate functions.

## Accepted changes

| Existing helper(s) | Owner and decision | Callers / proof |
|---|---|---|
| Classic `validate_device`, `validate_allocation` | Inherent `validate(&self)` methods on core's `Device` and `ContainerAllocation`, delegating path invariants to `DevicePath` and `HostMount`. No new dependencies. | Runtime filtering and Allocate trust boundaries remain; existing invalid ID/path and malformed-allocation tests plus direct public validation proof. |
| Quantity `format_decimal` | Private `Quantity::format_decimal(&self, format)`: its nano-precision precondition belongs to a validated Quantity, not arbitrary BigDecimal inputs. | Display and canonical comparison; exact upstream corpus and wire/numeric tests. |
| Metadata `store::persist`, `store::cleanup` | Private synchronous `MetadataRuntime` methods in the storage module. Async entrypoints retain blocking-task/owned-guard lifetimes. | Publish/cleanup and storage tests; journal/CDI golden bytes, retry, collision and cancellation proof. |
| Example `statuses_for_claim` | Inherent method on local `BindingState`, which owns the binding-policy decision. | Controller main and condition-transition/foreign-driver tests. |
| Example pair inventory/CDI projection | `From<&AcceleratorPair>` for `PoolDevice` and the CDI device. The source is local, so the traits are legal without a dependency change. Retain borrowing of the driver's pair model. | Inventory and CDI specification construction; existing pair tests and independent expected CDI JSON. |
| Example `cdi_device_id` for a pair | `AcceleratorPair::cdi_device_id(&self)` owns the pair identity. | Preparation uses the already resolved pair; existing literal CDI ID assertion. |
| Test-only `device_with_path` | Remove the redundant wrapper and use `Device::rdwr`. | Existing static plugin tests. |

The public core additions are validation methods only. Their ID/path checks
and error order match the previous runtime functions, do not access the
filesystem, and remain invoked at the same trust boundaries. Standard
conversions are used where existing crate ownership permits them. There are
no new core features or dependencies, and no duplicate `to_wire`/`From` API.

## Retained functions and boundary decisions

| Scope / functions | Why a free function fits |
|---|---|
| Core `validation::*` scalar validators and DNS predicates | Shared algorithms over borrowed strings/paths; no existing semantic wrapper owns those inputs. Public APIs remain compatible. Object-level validation now composes them on the owning types. |
| Core socket hashing/name sanitization and Unix-socket recovery/locking helpers | Standalone algorithms and coordinated filesystem operations. Preserve stable hashes, limits and lock filenames; no wrapper solely for syntax. |
| DRA `pool_device_to_wire`, `attribute_value_to_wire`, `claim_from_wire`, `prepared_device_to_wire`, health `report_to_wire` / `device_to_wire` | Both endpoint types are foreign in the adapter crate. Preserve core independence, typed attributes, empty-map omission, request normalization, share-ID limits and health fields. |
| Classic `device_to_proto`, `device_path_to_spec`, `host_mount_to_proto`, `container_allocation_to_response` | The same orphan/dependency boundary. Borrow retained watch snapshots and consume allocation fields. Keep independent raw-wire tests. |
| DRA `inventory_device_to_wire`, counter conversions and name/reference validation | Pool-scoped diagnostics, API limits and cross-device/set references require publisher context. A context-free `From` would hide validation; tuple-based `TryFrom` would obscure the operation. |
| ResourceSlice canonicalization, matching, generation, taint-time preservation, slice naming and retry calculation | Algorithms over foreign API objects, collections and reconciliation history; no local value type owns all inputs. Preserve ordering and numeric comparison. |
| Classic `valid_devices`, order-insensitive equality, allocation-error/status and server-exit mappings | Batch operations and runtime-specific transport policy, rather than core-wide semantics. |
| DRA component supervision, publisher/RPC error mappings, socket setup/connect, transient-read classification and watch completion | Multi-object orchestration and transport decisions with contextual inputs/foreign types. |
| Metadata scalar grammar/attribute validation, descriptor-relative storage primitives and filename calculation | Metadata-specific schemas and secure filesystem algorithms. Preserve different grammar limits, no-follow access and create-only persistence. High-level runtime persistence moves to methods. |
| Metadata `Plan::build`, journal projection and `JournalDevice::from` | Already use a useful owner or standard conversion. Driver context and borrowed retry state remain explicit. |
| Device-status and pool-status publishers, CDI models and registration services | Production behavior already belongs to local types; retain test fixtures and generic defaults. |
| Example environment/device-name parsing, taint-effect environment validation, minimal-driver CDI ID formatting, batch CDI writes and inventory fixtures | Configuration/driver identity or multi-object orchestration. Do not expand core APIs to eliminate an example helper. `BindingState` already implements `FromStr`. |
| Test-helper mock client/server startup, scripted responses, typed Kubernetes fixtures and legacy JSON fixtures | Transport orchestration or fixtures over foreign types. Preserve published mock APIs and async lock compatibility. Retain helpers naming useful scenarios. |
| Tests, `main`, private generic `default`, protocol build entrypoint | Entry points and fixtures are appropriate free functions. Generated output is excluded. |
| Quantity parsing/integer and optional OpenAPI conversions; facade re-exports | Already type-owned or standard traits. Quantity's explicitly supported optional integration stays in that standalone crate; core does not enable it. |

## Standards and prevention

AGENTS.md, contributor guidance and a PR checklist require an ownership,
fallibility, crate/dependency and borrowing decision, plus independent
behavior/schema proof. Architecture boundaries take precedence over syntax.
Explicit Clippy conversion/naming rules check mechanical conventions; they
cannot infer ownership. Do not ban free functions with a name-based check.

An independent core consumer check builds default, minimal and all core
features without a schema, and rejects generated protocol, Tonic, kube and
OpenAPI dependencies. It runs in CI and the local consumer-check gate. This
prevents an optional convenience integration from silently changing the
core boundary. DRA's existing independent schema matrix remains in place.

## Validation

Implementation and focused validation are in progress. Record final checks
before closing the ticket. No release is requested by this task.
