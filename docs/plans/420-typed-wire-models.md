# Typed metadata, CDI and journal models

Tracking: `k8s-device-plugin-rs-420`.

The runtime previously constructed fixed-schema protocol objects through
`serde_json::Value`. The replacement models are private to their owning
protocols. Metadata reuses the existing Kubernetes object metadata and device
attributes; the journal adapts core claim/prepared-device values. Examples
reuse the private CDI projection without expanding the public library API.

## Compatibility and safety

- Preserve wire names, omitted optional fields, canonical key order, compact
  runtime output, pretty example output and document/spec final newlines.
- Keep journal version 1 and exact immutable artifact bytes. Golden files were
  captured before migration from `197a277`, with stable host paths.
- Decode the entire journal before cleanup. Reject malformed values, extra
  identity fields and unknown schema extensions; retain the journal/artifacts
  when interpretation is unsafe.
- Preserve driver/claim identity checks, request/path validation, CDI content
  comparisons, no-follow descriptor-relative filesystem operations, size
  limits, publication order and per-claim cancellation-safe locking.
- Retain arbitrary driver data and partial status merge patches as JSON.

## Acceptance

Golden compatibility tests cover direct/template metadata, all four scalar
attribute kinds, empty attributes, exact journal/document/spec bytes, retries
over old artifacts, inode preservation and restart cleanup. Independent CDI
wire assertions cover pretty environment specs and mount omission. Malformed
journal tests verify refusal before either request's artifacts are deleted.

## Validation results (2026-11-05)

- Stable Rust 1.99.1 passed all-target/all-feature workspace tests under both
  consumer-selected schemas: 220 tests each for 1.36 and 1.37. Strict Clippy
  passed for both schemas.
- `mise run ci` passed formatting, default and resource-health Clippy, 202
  workspace tests, 123 DRA health/example tests, 28 matrix tests, five profile
  tests and independent consumer schema checks. The isolated source snapshot
  also passed all 202 default-feature workspace tests, excluding the unrelated
  local `core/Cargo.toml` default-feature edit.
- A native linux/arm64 image built with Rust 1.98, using the same isolated
  snapshot. Unique reference:
  `docker.io/library/k8s-device-plugin-dra-example:typed-wire-420`; config:
  `sha256:c3cf7d0cb765872516dcd3961a774c2ae9929bc1bab9f0482a49d965a3f65672`.
  The v1.36 live Pod digest matched; the same fresh image reference was imported
  and deployed to both profiles.
- `validate-k8s-versions.sh` passed with `DRA_E2E_METADATA=1` on Kubernetes
  v1.36.1 and v1.37.0: registration/liveness, ordinary CDI, FirstAvailable,
  direct/reused/template metadata, request isolation, read-only mounts,
  restart/inode preservation, cleanup, writer-failure isolation and retry.
  The v1.37 extended-resource lifecycle passed too.
- Matrix and outer cleanup exited 0. Both validation profiles and the builder
  were restored to stopped; all remained arm64 with `rosetta=false`.
  Validation used a temporary kubeconfig and retained the user's selected
  `dra-validation-advanced-137` context and `maxSurge: 0`.
