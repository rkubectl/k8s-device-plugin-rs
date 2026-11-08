# Sixth-review validation and hardening

Tracking: `k8s-device-plugin-rs-4xq0` and children `.1` through `.7`.
Status: approved for planning and implementation by the maintainer.
Baseline: `9551921e4d0b564e900efcbdfc1ef4ab89a6c152` (v0.6.0 publication audit).
Private `CODE_REVIEW-6.md` is evidence, not a source-controlled deliverable.
No release, tag or publication is included in this task.

## Decisions and implementation

| Ticket | Finding and implementation |
| --- | --- |
| `4xq0.1` | Match classic `ExtendedResourceName` to Kubernetes `IsExtendedResourceName`. Reject native resources and the `requests.` prefix, including any occurrence of `kubernetes.io/`. Validate the quota-prefixed domain as a DNS1123 subdomain (253 bytes including `requests.`, therefore 244 for the original domain), without a separate component-length limit. Use qualified-name suffix syntax: ASCII alphanumeric ends, ASCII alphanumeric, dot, underscore and hyphen inside, at most 63 bytes. Preserve original bytes and case; keep `DeviceId` semantics and pure-core dependency independence. |
| `4xq0.2` | Reject NUL centrally in classic path validation with a precise `ValidationError::NulByte(Utf8PathBuf)`. Preserve existing empty/relative failure precedence and filesystem-independent checks. Exercise both host and container paths for device specifications and mounts, allocation validation, ListAndWatch rejection and Allocate's internal-error response. |
| `4xq0.3` | Use a fixed, architecture-independent numeric width sufficient for supported usize indices (20 decimal digits covers all 64-bit values). Keep names bounded to 63 bytes, DNS safe, deterministic and hashed from the complete driver/node/pool identity. A name change is a migration: publish the complete newer generation before deleting old unpadded names with UID/resourceVersion preconditions, then converge without writes. |
| `4xq0.4` | Declare Tokio `net`, `sync` and `rt` directly in classic runtime dependencies. Verify an isolated runtime consumer; do not depend on incidental feature unification. |
| `4xq0.5` | Keep the public registration health builder and make repeated calls idempotent. Ordinary construction still omits health advertisement; explicit activation advertises one entry and serves the existing handler. |
| `4xq0.6` | Document that v1.37 compatibility groups cannot be expressed by the current shared-counter adapter. Link deferred feature assessment `qt7.6.10`; do not add a schema-dependent backend field in this batch. |
| `4xq0.7` | Correct the Quantity README's historical publication wording without changing its documented conformance scope. |

`ValidationError` is a public exhaustive enum. Adding its NUL variant can
break exhaustive downstream matches. Any new resource-name error variants have
the same consideration. Record migration guidance and use the next breaking
minor release boundary for these API changes; do not silently claim patch
source compatibility or bump versions as part of this implementation.

Keep defaults and invariants with their owning types. Preserve runtime error
precedence, wire/journal bytes, cancellation and lock lifetimes, the h2 pin,
schema selection, native ARM64/Rosetta-disabled operation and `maxSurge: 0`.
Existing standalone lexical helpers may remain private implementation details;
do not introduce a public validation utility solely to share a small parser.

## Evidence and ownership

Parent evidence tier: Verify. Graph project
`Volumes-rust-k8s-device-plugin-rs`, generation `2026-11-08T11:53:21Z`, ready.
Targeted searches found `core.src.identity.valid_label`/`valid_subdomain`,
`core.src.validation.validate_absolute_path`,
`dra.src.registration.DraRegistrationServer.with_resource_health`,
`dra.src.resourceslice.slice_name` and
`dra.src.resourceslice.counters.consumptions_to_wire` (qualified names have
the graph project prefix). Exact source was read for these and the identity
macro/validator, runtime allocation boundary, inventory field, manifest and
public documentation. Searches were fully paginated in the bounded scopes.

Inbound `validate_absolute_path` traces reach `DevicePath::validate` and
`HostMount::validate`, then `Device::validate` and
`ContainerAllocation::validate`; the Allocate wire boundary validates the
allocation before conversion. Inbound `slice_name` reaches
`ResourceSlicePublisher::reconcile_pool`. The exact source and existing
legacy-migration regression establish the publication/deletion boundary.
Coverage checks of all source, test, manifest and documentation paths used in
this plan reported matching metadata and no recorded gaps. Coverage is best
effort; macro-generated relationships require direct source checks.

Upstream reference: Kubernetes v1.37.0
[`IsExtendedResourceName`](https://github.com/kubernetes/kubernetes/blob/v1.37.0/pkg/apis/core/v1/helper/helpers.go#L39)
and apimachinery qualified-name/subdomain validation; check the v1.36 contract
as well before implementing. A matching Rust Kubernetes model is not a lexical
validator and must not couple classic core to OpenAPI.

Implementation uses the existing Terra session/worktree after fast-forwarding
it to this plan commit. It owns the task's source/tests/docs and synchronized
trial Beads state; it must not revert other edits. Parent owns final review,
integration and tracking disposition. Do not spawn more agents or poll
workers routinely. Keep the original private review files untouched.

## Validation and completion

1. Add independent lexical fixtures for requests/native names, suffix grammar,
   63/64-byte suffixes, 244/245-byte domains, long components, malformed input,
   checked serde and exact string preservation. Compare supported upstream
   contracts without adding a Go/OpenAPI runtime dependency.
2. Add path regressions at the domain and RPC boundaries; cover both path
   fields and mounts. Add health-builder duplicate-call coverage.
3. Verify slice-name numeric ordering for at least 12 indices and across digit
   boundaries, DNS/hash invariants including usize::MAX, old-name migration,
   conditional deletion and idempotent reconciliation. Reuse existing mock
   infrastructure and keep independent expected wire values.
4. Run focused checks while implementing, then one comprehensive assembled
   CI pass, affected all-target tests/Clippy and doctests under both 1.36 and
   1.37 schemas. Verify independent classic and DRA consumers.
5. Naming changes affect Kubernetes-facing publication: build the native ARM64
   fixture and run the existing guarded 1.36.1/1.37.0 DRA smoke path on the
   dedicated 1.7.1 validation profiles. Record image identities and naming
   migration evidence. Preserve original profile states/context and avoid
   pre-1.7 user profiles. The standalone kind environment remains unavailable;
   record actual checks and limits rather than asserting a pass.
6. Record exact source revisions/commands/results in this plan and tickets.
   Keep issue exports committed with their corresponding focused changes.
   Parent reviews the final result, integrates locally, closes completed
   findings and synchronizes tracking. Publishing remains a separate task.
