# Compile protocol bindings without an external protoc

## Primary goal

Build the classic, DRA, registration, and optional resource-health protocol
bindings from a Git checkout or published crate with the Rust toolchain alone,
without installing or invoking `protoc`. Preserve existing generated APIs,
wire definitions, and Cargo feature boundaries.

Beads task: `k8s-device-plugin-rs-o0r`.

## Assessment and decision

Before this migration, `proto/build.rs` called
`tonic_prost_build::Builder::compile_protos` three times depending on features.
Its four proto3 inputs have no imports;
the DRA input includes proto3 optional fields. CI installed `protobuf-compiler`
in two jobs, all three Docker builders installed it, and both the root and proto
READMEs described it as a prerequisite.

Use `protox` 0.10 as a build dependency. The published 0.10.0 manifest requires
Rust 1.85, below this workspace's 1.98 MSRV, and uses Prost/Prost Types 0.14,
matching `tonic-prost-build` 0.14.6. No CLI feature is needed. This adds Rust
compiler/parser dependencies at build time; it does not add a runtime compiler.
Keep the existing Tonic/Prost generator, consuming the in-memory descriptor set
through `compile_fds`, so client/server code generation settings remain the same.

Allowed APIs, checked against published source and documentation on 2026-10-23:

- [`protox::Compiler`](https://docs.rs/protox/0.10.0/protox/struct.Compiler.html):
  `new`, `include_imports`, `include_source_info`, `open_files`,
  `file_descriptor_set`, and `files`; each file's `path` identifies disk inputs.
- [`tonic_prost_build::Builder::compile_fds`](https://docs.rs/tonic-prost-build/0.14.6/tonic_prost_build/struct.Builder.html#method.compile_fds)
  accepts the matching `prost_types::FileDescriptorSet` and calls Prost's
  in-memory generator without loading schemas through `protoc`.
- The [protox example](https://docs.rs/protox/0.10.0/protox/) confirms the
  descriptor-set handoff. Its older `tonic_build` spelling is not used here;
  this workspace already uses the separate `tonic_prost_build` crate.

Graph tools were unavailable in this session. The assessment instead used
direct reads of the build script, library include modules, Cargo manifests,
all four schema headers/import declarations, CI, Dockerfiles, and published
dependency source. No graph coverage or exhaustive graph claim is made.

## Phase 0: Capture the current contract

How this serves the primary goal: establish what must remain equivalent when
the external compiler is removed.

- Save all four generated Rust outputs from the current all-features build
  using installed protoc 36.2 as a temporary comparison baseline.
- Confirm the classic-only, `dra`, and `dra-health` feature selections and
  the `dra-health -> dra` implication in `proto/Cargo.toml`/`proto/src/lib.rs`.
- Verify the selected compiler's MSRV and descriptor type compatibility.

Verification: the baseline contains `v1beta1.rs`, `pluginregistration.rs`,
`k8s.io.kubelet.pkg.apis.dra.v1.rs`, and `v1alpha1.rs`.

## Phase 1: Compile descriptors in-process

How this serves the primary goal: remove the executable dependency while
retaining the established Rust generator.

- Add `protox = "0.10"` to workspace dependencies and inherit it only in
  `proto` build dependencies. Review the lockfile for unrelated changes.
- Select the same schema paths using the existing Cargo feature checks.
- Use `Compiler::new(["."])`, enable imports/source info, open the selected
  inputs, and pass the descriptor set to Tonic's `compile_fds`.
- Emit narrow Cargo rerun directives for `build.rs` and each disk input
  reported by the compiler, including future local imports.

Verification: compare generated Rust with the protoc baseline; investigate
every difference, including comments, before accepting the migration.
Do not bundle a protoc executable, spawn a subprocess, download schemas at
build time, or modify the Kubernetes submodule/schema definitions.

## Phase 2: Remove prerequisites and prevent regression

How this serves the primary goal: make developer, CI, and container builds
benefit from the migration, with a durable proof that external tools stay unused.

- Remove protobuf compiler installation from both CI jobs and all three
  Dockerfiles, retaining unrelated native build tools.
- Update both READMEs to describe Rust-based compilation and the retained
  submodule requirement for Git checkouts.
- Add an executable protocol feature-matrix check using a fresh temporary
  target directory, a failing `protoc` sentinel on PATH and in `PROTOC`, and
  an invalid `PROTOC_INCLUDE`. Run classic-only, `dra`, and `dra-health` builds.
- Run that check in CI and expose it as an opt-in local mise task.

Verification: all three feature sets succeed, the sentinel is never invoked,
and searches find no remaining installation prerequisite outside historical
assessment text. Do not uninstall or alter the user's installed protoc.

## Phase 3: Verify integration and packaging

How this serves the primary goal: prove both repository and registry consumers
can compile the same protocol definitions without the external compiler.

- Run the fresh-target feature matrix and strict full workspace CI, including
  opt-in DRA resource-health tests/examples, with a failing protoc sentinel.
- Check the generated rerun directives cover every selected on-disk input;
  verify incremental rebuilding after a source timestamp change.
- Package and verify `k8s-device-plugin-proto` with all features and the
  sentinel configured; check that schema files and the build dependency are
  present in the resulting crate.
- Review the complete diff, record results here and in Beads, then commit and
  push the implementation with the synchronized issue export.

Generated output equality establishes that kubelet-facing code is unchanged;
if that fails beyond understood comment-only differences, stop and reassess
whether live DRA smoke validation is required. A new release and a toolchain
MSRV change are outside this task.

## Execution evidence

Completed on 2026-10-23:

- All four generated Rust files are byte-for-byte identical to the protoc
  36.2 baseline. No schema, public API, wire output, or submodule revision
  changed. No live-cluster smoke run was needed for unchanged generated code.
- `RUSTC_WRAPPER= proto/hack/check-without-protoc.sh` passed from an empty
  temporary target directory for classic-only, `dra`, and `dra-health`.
  The PATH/`PROTOC` sentinel was never invoked, and `PROTOC_INCLUDE` pointed
  to a nonexistent directory. The original pre-migration build executable
  failed and tripped the sentinel as a negative control.
- Full `mise run ci` passed with the same external compiler guard: formatting,
  strict workspace Clippy, 127 workspace tests, strict resource-health Clippy,
  and 56 resource-health/example tests. Socket tests ran outside the restrictive
  sandbox. Existing manifest warnings about an unused workspace dependency and
  commented proto lint inheritance remain outside this task.
- Changing the DRA schema's timestamp triggered regeneration; emitted rerun
  directives covered `build.rs` and all four schemas. The check restored the
  original source timestamp and verified its content hash was unchanged.
- `cargo package --locked -p k8s-device-plugin-proto --all-features
  --allow-dirty` passed build verification with the guard. The resulting crate
  includes all four schema sources and the normalized `protox` build dependency;
  the repository-only `hack/` checker is excluded. Nothing was published.
- The lockfile adds nine compiler/parser/diagnostic build dependencies and
  retains every existing package version, including the exact h2 tag source.
- CI YAML parses, the checker passes shell syntax validation, and the diff
  passes whitespace checks. CI and all three Dockerfiles no longer install a
  protobuf compiler. Local container image builds were not rerun.

Generated-source SHA-256 values (identical before and after):

| Generated file | SHA-256 |
|---|---|
| `v1beta1.rs` | `29ad10c86ebfaaec143a77af5ad443e67286b49ae4293c79fd97930a68042bf9` |
| `pluginregistration.rs` | `69571949b48e33c8b0e61e9cb34894d28fa7253ce6a1487bf1873256f7c4c4eb` |
| `k8s.io.kubelet.pkg.apis.dra.v1.rs` | `8f1dca3ebd48733b28c9c8213a47aef7c14dd249ac5890c2618321873eb9a84d` |
| `v1alpha1.rs` | `70e6ee9e4b41075258cc8e7ccc1dbc9e04bfbe735f838ac9d3a9d7cb09381f68` |
