# DRA DNS subdomain compatibility

Tracking: `k8s-device-plugin-rs-ha1.6`. User authorized planning and
implementation from trial commit `71fef879a275d42f1eebfc46520c419743742121`.

Kubernetes 1.36.1 and 1.37.0 `IsDNS1123Subdomain` check the total
253-byte limit and component syntax, without a separate 63-byte component
limit. The current DRA identity and metadata validators incorrectly apply the
DNS-label limit to each component of a subdomain. Preserve the existing
63-byte total for `DriverName`, the 253-byte totals for `NodeName` and the
slash-separated `PoolName`, and the 63-byte limit for actual device and
counter DNS labels. Do not change case rules; `k8s-device-plugin-rs-rpj9`
tracks the separate driver-name case question.

1. Separate component syntax from actual DNS-label length checks in DRA core
   and metadata; do not add a public validation helper just for reuse.
2. Independently test 63/64/253/254-byte boundaries, dot and slash syntax,
   labels, serde and the metadata claim path and journal. Exercise both
   supported OpenAPI schemas and native filesystem boundaries.
3. Audit downstream paths: a claim name is an independent container path
   component, not a local journal filename. Driver, UID and request limits
   remain in place for host paths and CDI filenames; actual filesystem path
   limits and mount operations can still return I/O errors.
4. Record exact checks and limitations in Beads; commit only on the trial
   branch. Release and publication remain separate.

Upstream references: [`IsDNS1123Subdomain` in v1.36.1](https://github.com/kubernetes/kubernetes/blob/v1.36.1/staging/src/k8s.io/apimachinery/pkg/util/validation/validation.go)
and [v1.37.0](https://github.com/kubernetes/kubernetes/blob/v1.37.0/staging/src/k8s.io/apimachinery/pkg/util/validation/validation.go).
Evidence tier Verify:
graph project `split-terra-experiment`, generation `2026-11-07T19:21:57Z`;
metadata subdomain is called by `Plan::build` and `attribute_key`. The graph
does not expose identity macro call sites, so those and downstream path usage
were checked in source. Coverage for the identity and metadata files showed no
recorded gaps and matching metadata; that is not proof of completeness.

## Implementation and evidence, 2026-11-08

The component syntax check is separate from the length-limited label check in
each owning crate. Metadata validates claim names as subdomains while keeping
namespace, UID, request and attribute-prefix limits unchanged. The claim name
appears as one component in the container mount path, but not in the local
metadata directory or CDI filename. Those use the bounded driver (63), UID
(64) and request (63); the CDI filename is at most 206 bytes and its staging
name is at most 245 bytes using 10-digit process IDs and 20-digit counters.
Filesystem-specific component and total-path limits still apply; errors are
not suppressed. A native filesystem test created a 253-byte component and
persisted and cleaned the metadata journal for each boundary.

With `RUSTC_WRAPPER=` and `--locked --offline`:

- `K8S_OPENAPI_ENABLED_VERSION=1.36 cargo test -p k8s-device-plugin-dra-core identity::tests` and the same command with `1.37` passed all three identity tests, including labels, subdomains and serde.
- `K8S_OPENAPI_ENABLED_VERSION=1.36 cargo test -p k8s-device-plugin-dra metadata::tests` and the same command with `1.37` passed all 21 tests, including journal golden bytes and the long claim path/persist/cleanup regression. A sandboxed 1.37 attempt failed on an unrelated Unix socket bind; the unrestricted rerun passed.
- `K8S_OPENAPI_ENABLED_VERSION=1.37 cargo clippy -p k8s-device-plugin-dra -p k8s-device-plugin-dra-core --all-targets -- -D warnings`, `cargo fmt --all --check` and `git diff --check` passed.
- `/private/tmp/ha16-native-boundary.sh` used isolated kubeconfig and server-side dry-run create on the native arm64 `k8s-versioned-171-validation-136` and `-137` profiles. Both Kubernetes v1.36.1 and v1.37.0 accepted 64- and 253-byte single-component ResourceClaim names and ResourceSlice pool/node names and rejected 254-byte values. This proves API validation, not a live driver rollout of the changed code. Script SHA-256 `2520379517af5bee85a441f13551f37668fc897eacc2e9919ce4ecbfffb39c04`; output `/private/tmp/ha16-native-boundary.log` SHA-256 `5d6089ff973295a6f53a31718efe2a2381ec411f8cddb2e610502fbf00b210ff`. These are temporary artifacts. Both profiles and the two other listed profiles were stopped before and after; all were arm64 with `rosetta=false`, and the original kubeconfig context was unchanged.
