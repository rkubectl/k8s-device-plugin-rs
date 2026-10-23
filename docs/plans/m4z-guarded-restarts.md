# Guarded local DRA matrix restarts

Beads: `k8s-device-plugin-rs-m4z`.

## Contract

Run the existing native v1.36.1 and v1.37.0 validation profiles serially,
using the maintained `k8s-versioned start --name` helper for every start and
baseline restoration. The helper owns guest admin.conf recovery, kube-proxy
reconciliation, node/DNS readiness, and host kubeconfig refresh. Run it even
when a VM is already running. Keep the workspace's h2 override and
DaemonSet `maxSurge: 0` unchanged.

Preflight validates executable dependencies, distinct profile names, and
Rosetta-disabled profiles before arming restoration. Preserve the original
failure code, fail an otherwise successful run when restoration fails, and
never start the baseline if stopping v1.37 fails. Print bounded driver logs
before stopping a profile and when a profile fails.

## Implementation and checks

- Replace raw starts and duplicated endpoint repair with the guarded helper.
- Use the same canonical image reference for build/import so an older
  short-name alias cannot silently select a stale fixture.
- Exercise normal execution, already-running repair, profile context routing,
  native build arguments, smoke/start/stop/query failures, failed restoration,
  diagnostic failure, version mismatch, and preflight refusal without VMs.
- Run those checks in CI and via `mise run dra-test-matrix`.
- Build the current fixture as linux/arm64; validate both profiles after
  observed address changes, including real kubelet registration,
  ResourceSlice publication, CDI prepare/unprepare, and v1.37 generated-claim
  cleanup. Restore v1.36.1 as the sole running validation profile.

## Validation record

The installed host helper was verified as `1.6.0-k8s-versioned`, commit
`9a1b26b`, against Container CLI/server 1.6.0. All 12 orchestration tests and
Bash syntax checks passed. Live matrix results are recorded in the
[design's cluster validation evidence](../dra-design.md#cluster-validation).

The build used a temporary context containing tracked workspace/submodule
files and current edits, excluding the 18 GiB local `target/` directory.
No Cargo outputs or private review files were removed.
