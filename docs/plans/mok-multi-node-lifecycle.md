# Multi-node DRA lifecycle validation

Tracking: `k8s-device-plugin-rs-mok`.

## Scope and ownership

Use the installed native `container k8s-versioned` helper's worker support
to create a fresh, disposable Kubernetes v1.36.1 cluster with two workers.
Refuse existing cluster/node names and concurrently running Kubernetes
profiles. The validation owns only that new cluster and a temporary
kubeconfig; delete its nodes on success or failure and retain diagnostics.
Existing profiles, the builder and the user's selected context stay outside
the orchestrator's ownership. Require native arm64 and `rosetta=false`.

Reuse the minimal widget driver, deployment and RPC liveness probe. Resolve
two supplied local images to distinct immutable identities, import them on
every node, deploy by digest and verify actual runtime image IDs. Select
only the two workers for the driver. Supply each node's name as its pool and
device identity through the existing environment configuration. Consumer
node affinity must go through the scheduler and DRA claim allocation.

## Acceptance path

1. Probe both driver replicas and assert node/driver/owner-specific slices.
   Prepare one consumer/claim per worker and verify distinct allocation and
   CDI identity.
2. Keep the first consumer running. Release the maintenance worker's claim,
   observe unprepare, artifact/claim cleanup and zero kubelet claims in use,
   then cordon/drain/uncordon that worker and verify driver readiness and
   inventory. Check the first consumer throughout.
3. Upgrade to the second immutable image with the first claim still active.
   Record old/new driver Pod lifetimes per node and prove termination precedes
   replacement start with `maxSurge: 0`. Verify replacement RPC readiness,
   inventory, runtime image IDs and continued consumer identity. Final consumer
   deletion must produce unprepare and zero claims in use.

Bound API and rollout waits. Install cleanup before cluster creation so
partial provisioning also cleans owned resources. Capture diagnostics before
deletion, preserve the original failure code and report cleanup failures.
Keep credential-bearing kubeconfig files out of retained/public evidence.

## Proof

Add VM-free tests for preflight rejection, successful ordering, mid-run
failures and cleanup failure reporting. Run the existing full CI gate, then
the native two-worker acceptance using clean source snapshots that exclude
the unrelated local core manifest edit. Record image/source identities,
Kubernetes version, node architecture, logs and final environment state.

Widget acceptance covers the framework lifecycle. Real device attachment,
hardware cleanup, production admission policy and least-privilege deployment
still need representative qualification by each hardware driver.

## Completion: 2026-11-05

Implemented `dra/hack/validate-multi-node.sh`, its 14 stateful VM-free tests
and the CI task. The final native run and cleanup exited zero on a fresh
v1.36.1 control plane plus two native arm64/Rosetta-disabled workers. Concurrent
allocations, CDI/metadata isolation, safe maintenance, active-consumer upgrade,
replacement readiness and final unprepare/cleanup passed. Full CI passed.

Two live API details refine the evidence contract: kubelet v1.36.1 drops the
per-driver metric after its final prepared claim, so require an explicit
`driver_name="<any>"` aggregate zero; Pod deletion notification can follow
replacement creation, so compare observed old container termination with
replacement running events and their timestamps, while retaining both Pod
addition/deletion events. Missing metrics or termination evidence still fail.

The [run guide](../dra-multi-node.md#acceptance-2026-11-05) and
[source-controlled evidence](../validation/mok-2026-11-05.json) record image
and source identities, per-node lifecycle order, unprepare logs and final
environment state. Existing profiles, the builder, selected context, local
core feature edit and private reviews were preserved. The published v0.4.1
tag and h2 pin were unchanged.
