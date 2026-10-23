# Compile protocol bindings without an external protoc

## Primary goal

Build the classic, DRA, registration, and optional resource-health protocol
bindings from a Git checkout or published crate with the Rust toolchain alone,
without installing or invoking `protoc`. Preserve existing generated APIs,
wire definitions, and Cargo feature boundaries.

Beads task: `k8s-device-plugin-rs-o0r`.

## Assessment and decision

The current `proto/build.rs` calls `tonic_prost_build::Builder::compile_protos`
three times depending on features. Its four proto3 inputs have no imports;
the DRA input includes proto3 optional fields. CI installs `protobuf-compiler`
in two jobs, all three Docker builders install it, and both the root and proto
READMEs describe it as a prerequisite.

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

Assessment and baseline captured before implementation. Validation results
will be recorded after execution.
