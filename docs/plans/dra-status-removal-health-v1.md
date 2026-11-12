# DRA status removal and stable health protocol

Tracking: `k8s-device-plugin-rs-jd9v` (GitHub #5) and
`k8s-device-plugin-rs-mtkc` (GitHub #4).

These additive changes extend existing owners and need no new subsystem or
OpenSpec workflow. Preserve the Kubernetes v1.36 baseline, consumer-selected
schemas, health opt-in, and all existing publication contracts.

## Status removal

- Add `ClaimDeviceStatusPublisher::remove(&ClaimRef, keys)`. Match the exact
  pool/device/share ID under the publisher's fixed driver identity; preserve
  every other entry and its order. Duplicate keys are harmless.
- Empty input needs no API request. Absent keys return `Unchanged` after the
  claim identity check. Allow removal after deallocation: removing stale
  status must not require an allocation that no longer exists.
- Share the existing read/UID-check/conditional-merge-patch/conflict-retry loop
  with publication. Reread and recompute after each conflict, at most three
  patches. Keep resourceVersion and UID safeguards and return terminal errors.
- `publish_fields` retains omitted fields. Document complete `publish` as the
  existing way to clear data/network data with `None`; do not redefine JSON
  null or add a second partial-update model.

## Health versions

- Generate `dra::health::v1` from the already pinned kubelet v1.37.2 source,
  alongside v1alpha1. No source-pin or public domain-type changes.
- Serve both services on the plugin socket, advertise both exactly once when
  opted in, and decline both when unsupported.
- Share one bounded watch lifecycle and cancellation path. Use explicit typed
  conversions for each protocol; preserve final buffered reports and reconnect
  errors. Do not introduce a protocol abstraction or serialization roundtrip.

## Validation checklist

- [x] API-mock removal tests: exact keys, other drivers, shared IDs, stale
      allocations, absent/empty/duplicate keys, empty resulting list, UID and
      resourceVersion failures, conflict recomputation/bounds, authorization.
- [x] Both health versions: independent wire values, identical report bytes,
      Unix-socket streaming/reconnect, unsupported service and idle cancellation.
- [x] Registration activation is idempotent and opt-in.
- [x] Public consumer checks and no-protoc generation include both versions.
- [x] Workspace CI, supported-schema tests/lints, doctests and strict rustdoc.
- [x] Native arm64 kind smoke/health negotiation on v1.36 and v1.37, preserving
      existing profiles, selected context and `rosetta=false`.
- [x] Document evidence, close/sync Beads, commit and push reviewed changes.

## Evidence

Initial Verify graph: `Volumes-rust-k8s-device-plugin-rs`, generation
`2026-11-08T21:18:54Z`, ready. Relevant runtime/build files matched coverage
metadata with no recorded gaps; coverage is best effort. Traces confirm
`publish`/`publish_fields` share `publish_inner`, and plugin health activation
configures both the service and registration. Exact source was read before
editing, including the existing cancellation and conflict tests.

### Local validation (2026-11-12)

On native arm64, with `RUSTC_WRAPPER=` and offline Cargo resolution:

```sh
CARGO_NET_OFFLINE=true mise run ci
CARGO_NET_OFFLINE=true sh proto/hack/check-without-protoc.sh
K8S_OPENAPI_ENABLED_VERSION=1.36 cargo test --locked --offline -p k8s-device-plugin-dra --all-targets
K8S_OPENAPI_ENABLED_VERSION=1.36 cargo clippy --locked --offline -p k8s-device-plugin-dra --all-targets -- -D warnings
# Each of K8S_OPENAPI_ENABLED_VERSION=1.36 and 1.37:
cargo test --locked --offline -p k8s-device-plugin-dra --doc
RUSTDOCFLAGS='-D warnings' cargo doc --locked --offline -p k8s-device-plugin-dra --no-deps
```

All passed. CI ran 251 workspace tests, strict workspace Clippy, 30 matrix,
five profile-configuration and 14 multi-node orchestration tests, plus
schema-independent classic/core/quantity consumers and independent DRA and
protocol feature consumers. Both supported schemas passed 143 DRA library
and 14 example tests. The DRA doctest target contains zero examples; strict
rustdoc passed for both schemas. Fresh-target builds verified all protocol
feature combinations without invoking external protoc. `cargo package --list`
confirmed inclusion of both health proto files and the upstream LICENSE.

The sandbox blocks Unix-socket binds; the assembled RPC test runs passed in
the unrestricted environment. An initial independent-runtime consumer fixture
incorrectly named the runtime's private `ClaimRef` import. It now imports the
public type from `dra-core`; the full CI rerun passed without changing library
ownership. Final graph coverage generation `2026-11-12T11:36:17Z` matched all
changed runtime, test, build, consumer and documentation paths with no recorded
gaps (best-effort metadata, backed by exact source/diff review).

Local evidence logs and SHA-256:

- `/private/tmp/dra-issues-ci.log`:
  `56f586c62c3823e450d18009c24783288fcb377f110d5f00f5fe977efb5e6091`
- `/private/tmp/dra-issues-no-protoc.log`:
  `6d800b552cd8c9b6c167dd6cef00fc6e0e8c49e21c5330766d82f0ce09a46cf3`
- `/private/tmp/dra-issues-schemas.log`:
  `54f79a10feec0b1f2c10a78a30f85522617bfb06808388095ff59c563af8e532`

### Native acceptance (2026-11-12)

Tested source: status-removal commit `c15f1c5`, then health commit
`2e38f189eb98d09656b35c3eea7dae279d6b9da7`. The build context was made with
`git archive HEAD` and a separate
archive of the pinned kubelet submodule, excluding untracked private review
files. No code changed after these commits.

After the Container-maintenance session explicitly released the shared
runtime, the restored `k8s-versioned` helper (`1.7.1-k8s-versioned`, source
`4d785db`) was used. The native linux/arm64 image built on the Dockerfile's
Rust 1.98 baseline:

```sh
container build --platform linux/arm64 -f dra/Dockerfile \
  -t docker.io/library/dra-issues:2e38f18 .
```

Image identities from the completed build:

- Manifest: `sha256:a1a99059d3dfcf45df105a0d5a78774ce60e430090af1ce4bffd292303d3aa7c`
- Config: `sha256:ddb9beca495e824e5837c69830ccfd4cbf2b62e20cfc46f5e628fb92f8792c58`
- Index: `sha256:d9209fb3ed401b3c96008b8ec349445c713fdfac6a085ff496f5d1f2b0730093`

Fresh, dedicated profiles `dra-issues-136` and `dra-issues-137` ran the
committed serial matrix with a temporary `KUBECONFIG`:

```sh
DRA_E2E_V136_CLUSTER=dra-issues-136 DRA_E2E_V137_CLUSTER=dra-issues-137 \
  DRA_E2E_IMAGE=docker.io/library/dra-issues:2e38f18 DRA_E2E_SKIP_BUILD=1 \
  DRA_E2E_RESOURCE_HEALTH=1 dra/hack/validate-k8s-versions.sh
```

Both v1.36.1 and v1.37.0 passed registration/liveness, CDI, prepared claim
status, FirstAvailable and unprepare. The v1.37 extended-resource lifecycle
also passed. The health fixture checked Healthy Pod resource status and the
service actually selected by kubelet: v1alpha1 on v1.36.1, v1 on v1.37.0.
The default backend was restored after each health-enabled rollout;
`maxSurge: 0` was preserved.

A temporary independent Rust application, built against the archived public
runtime/core APIs, additionally ran against each live API server using the
isolated administrator kubeconfig. For the allocated fixture claim it checked:

1. `remove` returned `Updated` and a fresh read contained no driver entry.
2. Repeating removal returned `Unchanged`.
3. `publish` restored the complete status.
4. Complete publication with `data: None` cleared the previously populated
   data field on a fresh read; the original status was restored afterward.

The normal driver fixture exercised publication using its service account;
the temporary host probe exercises real API semantics, not production RBAC
qualification. Authorization/error and concurrent-writer behavior remain
covered by the API-mock tests. No hardware-specific deployment qualification
or release is included.

Acceptance and cleanup both exited zero. Both owned profiles were deleted,
the builder was returned to stopped, all four original profiles matched their
initial stopped/arm64/`rosetta=false` snapshot, and the shared context remained
`dra-validation-advanced-137`. The temporary kubeconfig was removed. The
builder itself was verified arm64 with `rosetta=false` while running and after
shutdown. Existing DRA profiles were never started.

Evidence in `/private/tmp/dra-issues.FnQEKv/` (SHA-256):

- `build.log`: `8e513c41fe929dcb1009dea4431c6d7e8a5a0f980ed3746fc261429cc39e50d9`
- `acceptance.log`: `3dcb0ab7efc0467500873a4fb310150632819d7ab6750d63fb00c0fb0d1b12ef`
- `status-check/src/main.rs`: `0c6dd9a7d217107c0979c985e278533e4ec90347ac37cd4345cf44a62cfa2ee4`

The local runner and probe are retained there alongside the committed-source
archive, profile snapshots and independent-client build log.
