# DRA runtime policy configuration

Tracking: `k8s-device-plugin-rs-97oi`. The maintainer approved implementation
of the three highest-priority configuration gaps and use of the existing Terra
session. Baseline: `3dcff72` on main. No release is included.

## API and ownership

Add consuming, must-use `DraPlugin` builders:

- `with_max_concurrent_resolves(usize)` forwards the resolver's per-kubelet-RPC
  API-read limit. Default 16; zero clamps to one. This is not a global limit.
- `with_claim_retry_policy(usize, Duration)` forwards maximum attempts and
  delay for transient claim reads. Default 3 attempts and 50 ms; zero attempts
  clamps to one, zero delay retains its existing meaning. Keep the existing
  HTTP/transport retry classification and terminal-error behavior.
- `with_publisher_retry_backoff(Duration, Duration)` forwards failed-publish
  base and maximum delay. Defaults 1 and 30 seconds; preserve the publisher's
  one-millisecond minimum and maximum-at-least-base normalization. Keep the
  successful poll interval independent; success still resets failure backoff.

Reuse the responsible types' existing builders and validation. Resolver
configuration belongs in the owning service; use narrow crate-private service
helpers as needed rather than exposing resolver fields or duplicating its
settings on the plugin. Do not introduce a general configuration bag or a
second implementation of retry/concurrency policy.

Builders must compose with polling, metadata and health activation without
resetting those components. Both ordinary and health-enabled constructors
must permit configuration, and repeated calls replace the relevant setting.
Preserve identity validation, wire/journal behavior, cancellation and guards.
Classic and liveness timeout changes are separately deferred as `spyb` and
`vt9x`; protocol-imposed limits remain fixed.

## Evidence and execution

Parent Verify evidence: graph `Volumes-rust-k8s-device-plugin-rs`, generation
`2026-11-08T20:20:21Z`, ready. Exact symbols include
`dra.src.lib.DraPlugin.from_driver`, `dra.src.claim.ClaimResolver`'s
`with_max_concurrent_resolves`/`with_retry_policy`/`resolve_context`,
`dra.src.service.DraPluginService.new`, and
`dra.src.resourceslice.ResourceSlicePublisher`'s `with_retry_backoff`/`spawn`
(prefix qualified names with the graph project). Complete bounded search
results and direct source confirmed current defaults and clamps.
Outbound `from_driver` reaches the resolver, service and publisher constructors;
inbound `resolve_context` reaches ordinary and metadata resolution paths.
Coverage for lib, claim, service, publisher, their tests, Cargo manifest and
public docs matched source with no recorded gaps; coverage remains best effort.
The trial worktree index may be stale and must use exact source fallback.

Reuse the existing Terra worktree, fast-forwarded to this plan commit. Terra
owns this task's source/tests/docs and its explicit trial Beads database;
other agents may edit the repository, so preserve unrelated edits. Parent owns
review, main integration and closure. No new agents, routine polling, main
merge, push, tag, version bump or registry publication from the worker.

## Validation and completion

Use existing API mocks and paused Tokio time to prove actual configured-plugin
behavior, not only field assignment: bounded concurrent claim requests,
transient retries/attempt bounds and configured publisher retry timing. Cover
default/zero normalization, last-call behavior and composition where material;
retain existing lower-level classification and cancellation regressions.
Update the DRA README/design guide with defaults, scope and a copyable example.

Run focused checks while implementing, then one assembled DRA all-target test,
strict Clippy, doctest and strict rustdoc pass under schemas 1.36 and 1.37;
verify direct and facade consumers can call the new API. Reuse existing checks
and add the smallest necessary consumer coverage. No cluster reprovisioning is
needed for this forwarding-only change; record any actual validation limits.
Record exact commands/source/results here and in the ticket; leave the main
ticket in progress for parent review. Parent integrates, closes and synchronizes
the accepted work. Preserve all private review files.

## Implementation and validation record

Tested source: `1d8688d3f7de8435c7e18cac5b22b060fb86c01d`, including API/test
commit `ff6f607` and documentation/consumer commit `1d8688d`. The plugin
forwards settings through crate-private service methods and the existing
publisher builder; it stores no duplicate policy. Configured-plugin tests
exercise the default, custom, zero and repeated settings; per-RPC concurrency
and independence; retry attempt/delay behavior; backoff capping, reset and
poll independence; and composition with metadata and health services.

With `RUSTC_WRAPPER=` and `CARGO_NET_OFFLINE=true`, the following passed for
each `K8S_OPENAPI_ENABLED_VERSION=1.36` and `1.37`:

```sh
cargo test --locked --offline -p k8s-device-plugin-dra --all-targets
cargo clippy --locked --offline -p k8s-device-plugin-dra --all-targets -- -D warnings
cargo test --locked --offline -p k8s-device-plugin-dra --doc
RUSTDOCFLAGS='-D warnings' cargo doc --locked --offline -p k8s-device-plugin-dra --no-deps
```

Each all-target run passed 134 library tests and 14 example tests. The test
and documentation runs used an unrestricted environment for Unix socket
bindings. `RUSTC_WRAPPER= CARGO_NET_OFFLINE=true sh dra/hack/check-consumer-schemas.sh`
passed direct DRA core/runtime, DRA-only facade and combined consumers on both
schemas, including calls to the new builders. The temporary consumer log
`/private/tmp/97oi-consumers.log` has SHA-256
`3158f86ae275de3fffc3972233a9240120efdb9b6ef0a47347197764c161c7d7`.
`cargo fmt --all --check` and `git diff --check` passed.

Trial graph `split-terra-experiment` identified this worktree and HEAD, but
coverage generation `2026-11-07T19:21:57Z` was stale for modified runtime
files and excluded docs and consumer scripts. Exact current source and those
excluded files were inspected directly. No live cluster was changed or
reprovisioned for this forwarding-only work. `spyb` and `vt9x` remain deferred;
the ticket stays in progress for parent review.
