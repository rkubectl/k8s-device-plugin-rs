# Fifth-review hardening and v0.5.0 release

Tracking: `k8s-device-plugin-rs-y6j` and fixes `a0f`, `isp`, `z2h`, `9b2`.
The maintainer selected CODE_REVIEW-5 findings 4, 3, 7 and 9, followed by
comprehensive validation and a release. The private review file is not part
of the release sources.

## Changes and invariants

- Empty `NodePrepareResources` requests return an empty successful response
  before resolution or backend hooks, including the metadata path. Nonempty
  batches retain their existing per-claim behavior.
- Conditional stale-slice deletion accepts API 404 as an already achieved
  desired state. UID/resourceVersion preconditions remain intact; 409, 403
  and 500 errors propagate.
- The classic runtime aborts and awaits its server after permanent
  registration failure. The original registration error is returned after
  the listener has closed.
- ResourceSlice comparison treats empty optional collections as absent,
  including device maps, shared-counter/device lists, capacity-policy values,
  the schema-specific node-resource map, and v1.37 compatibility groups and
  skipped node operations. Nonempty values remain significant. Existing
  canonicalization of binding conditions, consumption lists and taints stays
  in place. This does not expose new inventory features.

All four reported failure modes were reproduced by new tests before their
fixes. Five focused regressions passed afterward. The shutdown test uses a
paused clock and checks the listener synchronously at return; the publisher
test feeds empty collections through the mocked API across repeated polls.

## Release boundary

v0.5.0 is a minor release because the exact Quantity migration removes the
public integer tuple field and `Copy`. Use `Quantity::from(2)` for integers,
`"0.5".parse()` for fractions, and borrowing or explicit cloning. See the
[Quantity plan](kubernetes-quantities.md) for migration, compatibility limits
and rollback. The new `k8s-quantity` crate publishes before core, followed by
the other dependents. No persisted metadata or CDI migration is required.

Validation and publication use a clean `main` checkout. The maintainer's
existing `core/Cargo.toml` default-resource-health edit and private review
notes are excluded and preserved in the original checkout. The published
resource-health feature therefore remains opt-in.

## Validation record

The clean checkout at fix commit `033dffd` passed `mise run --jobs 1
release-preflight`: formatting, strict Clippy, 226 workspace tests,
resource-health tests/examples, 29 matrix-orchestration tests, 14 multi-node
tests, five profile tests, and independent schema 1.36/1.37 consumers with and
without resource health. Its automatic patch preview was only a dry run;
v0.5.0 is the selected release boundary.

An additional sequential matrix passed under each schema, 1.36 and 1.37:

- 239 default-feature and 244 all-feature workspace tests with all targets;
- strict all-target/all-feature Clippy;
- all-feature workspace doctests and strict rustdoc.

Core also compiled with no default features; standalone Quantity passed its
five tests and doctest without a selected Kubernetes schema. Host validation
used native arm64 Rust 1.101.0-nightly. The native linux/arm64 validation image
built with the declared Rust 1.98 minimum.

GitHub [CI run 42288544063](https://github.com/rkubectl/k8s-device-plugin-rs/actions/runs/42288544063)
passed on the pushed fix commit, including Ubuntu tests/lints under both
schemas, independent consumers, protocol generation, validation tooling and
all three Docker build configurations.

Native Kubernetes v1.36.1 and v1.37.0 both passed baseline and FirstAvailable
CDI, fractional shared counters (stable polls, capacity exclusion,
release/retry and CDI), and metadata direct/reused/template/FirstAvailable
isolation and cleanup, writer failure isolation, an independent peer claim
and retry. The v1.37 extended-resource generated-claim lifecycle also passed.

The image was
`docker.io/library/k8s-device-plugin-dra-example:review5-y6j`, built from fix
commit `033dffd` before the release version transaction. Its config digest
`sha256:106d0e5aad7c1dc157e0cec0cf154d8867c7634815eaa6ce11f9961a3ee8aeff`
was verified on the running driver Pod on each version. The matrix and outer
restoration exited zero. Both `dra-upgrade-348-136` / `dra-upgrade-348-137`
profiles and `buildkit` were restored to stopped, all arm64 with
`rosetta=false`. Validation retained `maxSurge: 0` and used a temporary
kubeconfig without changing the user's selected context.

## Remaining assessment

The function-ownership assessment remains open as
`k8s-device-plugin-rs-zw5`. Other unselected fifth-review items are explicit
Tokio feature declarations, health-builder idempotence, Docker build-argument
cleanup, slice ordering and v1.37 compatibility-group support/documentation.
Resolve the maintainer's local default-feature edit as a separate feature
contract decision. These items are outside this release's four-fix scope.

## Publication — 2026-11-07

The release version transaction is commit
`dbc2ba3cb3d294aa3e28ea687d35c318209fb646`. Annotated tag `v0.5.0` has tag
object `4abb4592f6ed66f1f9d7865d7f2d5ab930887f8e`; its peeled commit and
remote `main` matched the release commit before publication. The clean
checkout matched the tag exactly. No release-source changes were made
after tagging.

All seven public packages were verified, published with
`release-publish.toml`, and independently retrieved from crates.io with
`cargo info <name>@0.5.0 --registry crates-io` outside the workspace:
`k8s-quantity`, `k8s-device-plugin-core`, `k8s-device-plugin-proto`,
`k8s-device-plugin-test`, `k8s-device-plugin-dra`, `k8s-device-plugin-lib`,
and `k8s-device-plugin`. The example remains unpublished. Registry metadata
confirmed that core's resource-health feature remains opt-in.

The original checkout was fast-forwarded to the release, preserving the
maintainer's local feature edit and all private review files. This publication
record and the final ticket closure are a separate audit commit after the
release tag, as required by the release workflow.
