# Separate classic and DRA contracts; validate identities at construction

Tracking: `k8s-device-plugin-rs-ha1` and phases `ha1.1`–`ha1.5`.
Status: **review approved; ha1.2, ha1.3, and ha1.4 complete**.
The user authorized the classic identity phase after ha1.3. ha1.5 has not
started; release and publication remain separate decisions.

Baseline: v0.5.0 plus ownership audit, commit
`51774a29a86dbbf47eb20bcbaca65481854ab9ec`, assessed 2026-11-07.

Related contracts: [DRA integration guide](../../dra/README.md),
[DRA design and acceptance](../dra-design.md),
[ownership audit](zw5-function-ownership.md), and
[Quantity design](kubernetes-quantities.md).

## Purpose

Classic plugins do not need Kubernetes REST API models. DRA does, and its
domain contracts have little substantive overlap with classic allocation.
Separate those contracts so classic stays small while DRA can use OpenAPI
where it improves ownership. Replace repeatedly validated identity strings
with semantic types whose construction establishes their lexical invariants.

Success means a classic-only application builds without OpenAPI, a schema,
Quantity or BigDecimal; a DRA application can use typed identities and fitting
OpenAPI conversions; and raw external strings are checked before entering the
typed domain. Contextual checks and transport failure behavior remain explicit.

## Evidence and current constraints

- `core/src/lib.rs` currently exports classic and DRA contracts together.
  `core/src/dra.rs`, `core/src/dra/attribute.rs` and
  `core/src/dra/inventory.rs` hold the DRA contracts, including Quantity.
  `core/Cargo.toml` therefore brings Quantity into a classic-only core consumer.
- `core/src/validation.rs` combines classic lexical/path errors, DRA name
  errors and a contextual driver mismatch in one `ValidationError`.
- The inbound graph trace of `validate_driver_name` identifies repeated
  checks in the resolver, publisher, registration, metadata, liveness and
  plugin construction. `ClaimResolver::validate_identity` checks syntax again
  before checking equality.
- `ResourcePool` returns maps keyed by `String`; `DraDriver::driver_name`
  returns `&str`. Names also recur in allocated/prepared devices and health
  reports. `PreparedDevice` derives a default containing empty identities.
- `PoolDevice::health` is explicitly informational; DRA's actual allocated
  resource health uses `ResourceHealthStatus`. Do not merge those meanings.
- The facade unconditionally re-exports current core types. Its DRA types
  must become conditional without introducing an import cycle.
- Existing independent core and DRA schema consumer scripts are useful
  acceptance gates; extend them instead of inventing a parallel framework.

Evidence tier: Verify, graph project `Volumes-rust-k8s-device-plugin-rs`,
generation `2026-11-07T13:31:35Z`. Candidate searches, exact contract snippets,
the driver-validator inbound trace and targeted source reads informed this
plan. Coverage metadata was fresh with no recorded issues for the relevant
core, facade, resolver and ResourceSlice paths or `core/src` scope. Coverage
is best effort, not proof of completeness; this is not a full caller audit.
Implementation must trace each changed signature and check exact callers.

## Proposed ownership and dependencies

| Layer | Responsibilities | Dependency contract |
|---|---|---|
| Existing `core` / `k8s-device-plugin-core` | Classic device/allocation traits and models, classic identities and errors; existing shared socket primitives and `Health` | No OpenAPI, kube, generated protocol, Tonic, Quantity or BigDecimal, including optional features |
| New `dra-core` / `k8s-device-plugin-dra-core` | DRA driver traits, inventory, claims, preparation, attributes, health contracts, DRA identities/errors, value projections to OpenAPI | May depend on pure core, Quantity and OpenAPI; no kube client, generated protocol or Tonic |
| Existing `dra` runtime | Kubernetes clients, reconciliation, registration/RPC services, metadata storage, contextual validation and error policy | Depends on both relevant cores, kube and protocol/transport crates |
| Existing classic `lib` | Classic RPC boundary, allocation/server policy | Pure core plus existing transport dependencies |
| Facade | Classic re-exports and conditional DRA/runtime exports | New DRA core dependency enabled only by `dra` |

The new crate depends on existing core for `Health` initially; both runtimes
can continue using core's proven socket facilities. Do not add a common crate,
duplicate socket algorithms, remove informational health, or reverse a core
dependency as part of this migration.

Move all current DRA contracts together, including `Arc` delegations, metadata
preparation contracts, resource-health contracts and Quantity re-exports.
Health contracts are always available in DRA core; reporting remains an
explicit backend choice. Preserve the exact Quantity and `AttributeValue`
domain types; OpenAPI string/struct representations do not replace their
invariants.

When OpenAPI projections land, DRA core has a direct, non-optional
`k8s-openapi` dependency with no selected version feature. This deliberately
accepts a schema/version dependency for DRA. Applications select exactly one
supported schema (`v1_36` or `v1_37` currently). Direct DRA core consumers
then require a schema too. Quantity remains independently usable without one.

Implement standard OpenAPI conversions in DRA core where one endpoint is
owned there and the projection is context-free. For example, the existing
`PoolDevice` projection can become `From` with its ignored informational
health documented. Preserve empty-map omission and attribute semantics.
Pool-level limits, reference checks, retry policy and slice assembly stay in
the runtime. Protocol conversions still have foreign endpoints in the runtime;
retain fitting free functions rather than adding a protobuf dependency to
DRA core. No reverse OpenAPI conversion is promised without an actual caller
and a complete, meaningful contract.

## Identity types and invariants

Use role-specific types, even when their private grammar checks are shared.
Names are case-sensitive, preserve input bytes and are never trimmed,
lowercased, sanitized or silently truncated.

| Owner/type | Construction invariant / migration scope |
|---|---|
| DRA `DriverName` | DNS subdomain, at most 63 bytes; runtime/backend identities, registration, metadata and probes |
| DRA `NodeName` | DNS subdomain, at most 253 bytes; publication and node-scoped operations |
| DRA `PoolName` | Slash-separated DNS subdomains, at most 253 bytes total; inventory map keys, allocated/prepared devices, health and status identities |
| DRA `DeviceName` | Kubernetes DRA device-name grammar; inventory, allocated/prepared devices, health and status identities |
| DRA `CounterSetName` | DNS label, 1–63 bytes; set identity and consumption references |
| DRA `CounterName` | DNS label, 1–63 bytes; counter map keys and references |
| Classic `ExtendedResourceName` | Existing domain/name form, domain/suffix grammar and reserved-domain rejection; plugin configuration/registration |
| Classic `DeviceId` | Existing nonempty, at-most-63-byte contract; device identity, allocation and optional hooks; **not DNS-label validation** |

Before implementing each grammar, compare its current validator and the pinned
Kubernetes source/API documentation for both supported schemas. Record exact
references in tests or type docs. In particular, establish DRA `DeviceName`'s
contract rather than copying the classic ID rule. Any newly rejected value
must be identified in the migration guide; unrelated tightening of existing
rules belongs in a separately reviewed fix.

Keep claim namespace/name/UID, request references, qualified attribute/capacity
keys, taint keys, CDI IDs, paths and arbitrary diagnostic/environment strings
outside this first identity migration. They have separate grammars and
context. UIDs are opaque, not necessarily UUIDs. Request `/subrequest`
normalization retains its current semantics. Revisit additional newtypes only
when a concrete invariant and owner justify them.

## Public API shape

Each identity is an owned struct with a private `String`, with checked
`FromStr`, `TryFrom<String>` and `TryFrom<&str>`. Validate an owned string by
reference and retain its allocation. Provide `as_str()`, `into_string()`,
`AsRef<str>`, `Display`, `Debug`, `Clone`, `Eq`, `Ord` and `Hash`.
Use `Borrow<str>` for map lookup with string-equivalent equality and hashing.
No public tuple field, mutable string access, unchecked public constructor,
infallible string-to-identity conversion or empty `Default` is permitted.
Start with `&DriverName` etc.; no borrowed unsized string types or generic
public validation framework are needed.

Domain constructors take typed identities and remain infallible when they
have no other failure condition. Parsing raw strings is an explicit boundary:

```rust
let driver: DriverName = "example.com".parse()?;
let pool: PoolName = "rack-a/pool-1".parse()?;
let device: DeviceName = "card-0".parse()?;
let offered = PoolDevice::new(device);
```

After migration `DraDriver::driver_name` returns `&DriverName`, and
`ResourcePool` maps use `PoolName`. Store typed fields in domain objects and
runtime configuration; use `DeviceName`, `CounterName` and `CounterSetName`
consistently at every matching definition/reference. Classic allocator,
pre-start and preferred-allocation signatures use typed `DeviceId` slices
and results. Remove `PreparedDevice`'s invalid empty `Default`, and provide a
constructor requiring pool/device identity. Keep valid collection defaults.

Runtime constructors accept typed identities. Retain `Result` only for
remaining contextual failures; do not keep a redundant lexical-validation
error path. Avoid a second parallel family of string-taking constructors.

Where serialization is needed, preserve plain string values/map keys. Serde
deserialization must call the checked constructor (for example via
`try_from = "String"`); deriving transparent `Deserialize` over the inner
string alone bypasses the invariant. Use the existing Serde dependency where
needed; do not add serialization support to pure core without a consumer.
Keep the journal's explicit wire DTOs and bytes; validate when recovering typed
domain state and preserve existing corrupt-journal handling.

## Error ownership and trust boundaries

Each identity exposes a focused construction error, such as
`DriverNameError`, with useful rejected-input and reason diagnostics. Share
private grammar code as appropriate. Do not replace all errors with an opaque
`InvalidString` or a generic public validator registry.

Pure core keeps classic/path validation errors. DRA core owns identity parse
errors. Runtime errors own driver mismatches, missing references and API/IO
failure policy, retaining typed causes where possible. During extraction,
separate the mixed error enum first; then remove redundant lexical branches
as typed APIs replace their callers. Do not preserve misleading old error
variants solely for source compatibility in this breaking migration.

Parse CLI/config input, incoming API/protobuf strings, backend construction
and recovered persistent records before typed domain use. Keep invalid RPC
claim reporting correlated with the original claim and preserve per-claim
failure isolation. Do not turn one failed parse into a whole-batch failure or
make malformed unrelated-driver allocations fail this driver's resolution.
Foreign wire/API objects remain untrusted even when internal values are typed.

Newtypes do not establish driver equality, reference existence, uniqueness,
collection limits, current allocation/UID identity or filesystem safety.
`PoolName` permits `/`, so it must not become a raw filename component.
Preserve stable socket/slice hashes, coordination paths, descriptor-relative
no-follow operations, request normalization, share-ID rejection, async guard
lifetimes and cancellation behavior. Keep path validation at existing
boundaries; this plan does not assert that current UTF-8/absolute-path checks
prove NUL freedom, existence or symlink safety.

## Compatibility, features and release integration

This is one coordinated source-breaking migration, proposed for the next
breaking minor release (normally v0.6.0 under the present pre-1.0 policy).
Do not tag or publish as part of implementation without a release instruction.
There is no old-core re-export bridge: core depending on DRA core would form
a cycle and reintroduce the schema dependency.

The facade retains top-level DRA names under its `dra` feature and exposes a
`dra_core` module alias. Its `core` alias stays the pure classic crate. Direct
users migrate `k8s_device_plugin_core::PoolDevice` to the new crate; facade
users enabling DRA keep the familiar name but adopt typed constructors and
fields. Cover default, no-default, classic-only, DRA-only and combined modes.

### Resource health: accepted plan revision, 2026-11-07

The maintainer approved removing the public `resource-health` Cargo feature
after the split. Compile support unconditionally within DRA, while retaining
explicit backend activation. This decision is settled for the plan; the full
implementation review gate remains open.

| Crate | Health contract after the split |
|---|---|
| Pure core | No DRA health contracts, health feature or health-only Tokio dependency |
| DRA core | Health types and `ResourceHealthReporter` always available; Tokio sync dependency unconditional |
| DRA runtime | Health service and APIs always compiled; always enable the protocol crate's `dra-health` feature |
| Facade | Health APIs exposed whenever `dra` is enabled; no separate health feature |
| Protocol | Retain the low-level `dra-health` feature for direct consumers and classic isolation |

`DraPlugin::new(...)` must not advertise resource-health capability or start a
health monitor. The compiled health endpoint explicitly returns `Unimplemented`
without a reporter. `DraPlugin::with_resource_health(...)` accepts a backend
implementing `ResourceHealthReporter` and advertises the capability; monitors
start for kubelet watches and retain existing cancellation/reaping behavior.
Clusters with health reporting disabled need no special Cargo configuration.
The version-sensitive `v1alpha1` protocol still requires compatibility review.

Remove obsolete public feature forwarding, `cfg` gates, health example
`required-features`, health-only build flags and health-off/on Cargo matrix
cells. Migrate examples, scripts, Docker inputs, docs.rs settings and CI as
needed. Keep schema/facade combinations and low-level protocol feature tests;
replace removed health Cargo combinations with ordinary-plugin and
health-enabled-plugin runtime tests. Do not keep an empty compatibility feature
or add a default-enabled feature that preserves the same branching.

The checkout contains a maintainer-owned addition of
`default = ["resource-health"]` in `core/Cargo.toml`; leave it untouched during
planning. During the approved implementation, remove the obsolete health
feature/default entry as part of this intentional migration, preserving any
other maintainer edits. Do not transfer it to DRA core or retain a dummy feature.

### Workspace and release integration

Add the new public crate to workspace dependencies, package/readme/docs.rs
metadata, lockfile, consumer checks, examples, test helpers, Docker/build
inputs and package/release automation as applicable. Verify release ordering
from pure core and Quantity to DRA core, then runtime and facade. Preserve the
existing shared-version, tag-first publication workflow.

Update AGENTS.md, CONTRIBUTING.md and current architecture guides to scope
schema independence to pure core and permit DRA core's deliberate OpenAPI
coupling. Preserve historical audit/release evidence; annotate superseded
decisions with a link rather than rewriting their original findings.

## Execution phases and review gates

| Ticket | Work | Completion evidence |
|---|---|---|
| `ha1.1` | Maintainer reviews this plan and explicitly approves implementation; unconditional DRA health support is already agreed | Recorded approval and remaining choices below; no Rust changes before approval |
| `ha1.2` | Extract DRA contracts, split mixed error ownership, remove public health feature gates, migrate imports/facade/docs and consumer/release plumbing; retain current domain string representations | Pure core/Quantity schema-free checks, both-schema DRA consumer builds, always-available health APIs and ordinary/health-enabled plugin tests; existing activation and wire semantics preserved |
| `ha1.3` | Introduce DRA identities, typed fields/maps/traits, construction errors and fitting type-owned OpenAPI projections | Invariant/boundary tests, both-schema conversions, per-claim failures and journal/metadata regressions |
| `ha1.4` | Introduce classic identities and migrate configuration/device/allocation/hook APIs and examples | Schema-free classic consumers, malformed RPC IDs, allocation/pre-start/preferred-allocation and path regressions |
| `ha1.5` | Complete migration documentation and comprehensive validation on the combined result | Matrix and acceptance record below, local package-content listings and reviewed API/dependency diff |

Keep these as separate focused commits. Run the smallest useful checks during
each change phase, then comprehensive validation in `ha1.5` after the changes
are assembled. If the final checks expose a defect, fix it and
rerun affected checks; do not repeatedly run broad matrices without a reason.

## Acceptance and validation

1. Identity tests independently cover empty values, exact/over byte limits,
   invalid grammar, reserved resource domains, slash pools, and non-DNS classic
   IDs. Reject malformed deserialization wherever supported; preserve plain
   string/map-key output. Include compile-fail doctests proving identities
   cannot be interchanged, constructed unchecked or mutated invalidly.
2. Keep independent API/protobuf assertions for empty collections, attributes,
   exact Quantity values, request normalization and health. Retain existing
   journal/CDI golden bytes and cancellation/retry/alias regressions. Check
   missing counters/sets and duplicate/oversized inventory still fail in the
   proper context despite lexically valid names.
3. Extend `core/hack/check-consumer-features.sh`: isolated default/minimal/all
   pure-core consumers and classic-only facade consumers build with no schema
   environment; normal dependency graphs exclude OpenAPI, kube, protocol,
   Tonic, DRA core/runtime, Quantity and BigDecimal as appropriate. Classic
   runtime naturally retains its own protocol/Tonic dependencies.
4. Extend `dra/hack/check-consumer-schemas.sh` with direct DRA core, direct
   runtime and facade consumers. The application explicitly selects each of
   v1.36/v1.37; libraries choose neither `latest` nor a version feature. Health
   contracts and runtime APIs must be available without a health Cargo feature.
   Check DRA-only and combined facade modes, retain independent schema-free
   Quantity proof, and retain the protocol crate's direct-consumer feature tests.
   Test that ordinary plugin construction omits health advertisement, declines
   health RPCs with `Unimplemented` and starts no reporter; explicit health
   construction advertises and streams reports, reconnects and cancels/reaps
   monitors correctly. Verify both runtime paths under each supported schema.
5. Run `mise run --jobs 1 ci`, then any matrix cells not already covered:
   locked workspace all-target tests, strict Clippy, doctests and strict
   rustdoc under both schemas and default/all features, sequentially or with
   isolated target directories. Run focused pure-core minimal tests and verify
   package contents and manifest/dependency ordering by non-mutating inspection.
   No release/preview, publication, registry writes, version, tag, push or merge commands.
6. Because identity parsing changes kubelet-facing entry paths, build the
   native ARM64 fixture and use the existing guarded Kubernetes version
   matrix for registration/liveness, prepare/unprepare/CDI, FirstAvailable,
   metadata recovery/cleanup and resource health; retain v1.37's existing
   extended-resource check. Follow the DRA guide's kind smoke path where
   applicable. Preserve `maxSurge: 0`, h2 compatibility, `rosetta=false`, image
   identity checks and the user's original cluster/profile/context state.
   Multi-node production qualification need not be rerun for an identity-only
   refactor unless lifecycle behavior changes or evidence reveals a concern.
7. Record exact source revision, commands, schemas/features, focused test
   results, image identities and any unavailable acceptance environment.
   Evidence gaps remain explicit; do not describe planned checks as passed.

## Items for plan review

- Approve the `dra-core` ownership and direct OpenAPI dependency once useful
  conversions land, including the direct-consumer schema requirement.
- Approve the eight initial identity types and typed constructor/trait API,
  with claim/request/CDI/path types deferred.
- Confirm focused construction errors and runtime-owned contextual errors.
- Confirm the coordinated breaking-release migration and conditional facade
  exports; release execution remains a separate instruction.

Agreed during review: always compile DRA health support, retain explicit
backend activation and only the low-level protocol feature, and remove the
obsolete core health default/feature during implementation as described above.

Planning checks: source/graph and dependency/feature review completed; tracking
dependencies and document links checked at commit time. No implementation,
compilation or live-cluster validation has been performed for this proposal.
