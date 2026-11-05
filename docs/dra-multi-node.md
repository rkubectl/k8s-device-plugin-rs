# Native multi-node DRA lifecycle validation

`dra/hack/validate-multi-node.sh` exercises the widget driver's lifecycle on
a fresh Kubernetes v1.36.1 control plane and two workers. It uses the maintained
native Apple Container `k8s-versioned` helper with worker support. The host,
images and nodes must be arm64; every node must have Rosetta disabled.

## Run

Build two distinct local images with `dra/Dockerfile`, using fully qualified
tags and clean source snapshots. For example, build a released baseline and
the candidate checkout with `container build --platform linux/arm64 -f
dra/Dockerfile -t docker.io/library/dra-lifecycle:<version> .`. Both builds
must contain the minimal driver and its RPC liveness probe. Stop the builder
before the run to leave room for the three nodes.

```sh
DRA_MULTI_NODE_BASELINE_IMAGE=docker.io/library/dra-lifecycle:baseline \
DRA_MULTI_NODE_UPGRADE_IMAGE=docker.io/library/dra-lifecycle:candidate \
DRA_MULTI_NODE_ARTIFACTS=/private/tmp/dra-lifecycle-evidence \
  bash dra/hack/validate-multi-node.sh
```

The artifact directory must not exist. If omitted, a temporary directory is
created and printed. `DRA_MULTI_NODE_CLUSTER` optionally changes the default
`dra-validation-mok-136` name; it must start with `dra-validation-mok-` and
contain at most 40 lowercase letters, digits and hyphens.

The runner refuses an existing target or worker prefix and any running
Kubernetes profile. Stop other profiles yourself before running it. It creates
and deletes only the new cluster, uses a private temporary kubeconfig, and
does not select a context in your normal configuration. Creation failures
also trigger deletion of partially created nodes. Existing stopped profiles
and the builder stay outside its ownership. Avoid concurrent host operations
that create/start profiles during acceptance.

## Gates and evidence

The runner imports both images on all nodes and compares each imported
containerd target digest with the inspected local OCI index/manifest. Local
imports initially have tags without CRI repository digests, so the runner adds
a matching digest alias inside each owned node and verifies CRI resolves it.
It requires distinct image config IDs. The rendered DaemonSet targets only
the two workers, uses immutable digests and keeps `maxSurge: 0`,
`maxUnavailable: 1`. Pool and device names
come from each node's name through the existing example configuration.

It then verifies:

1. Two ready replicas with matching runtime image IDs, passing real-RPC probes,
   and node/driver-owned ResourceSlices containing distinct devices. Two
   consumers use scheduler node affinity, receive the expected allocations and
   CDI environment markers, and read their claim-specific device metadata.
2. Consumer B is deleted, its UID appears in unprepare logs, its metadata and
   CDI artifacts disappear, and its kubelet explicitly reports zero
   `dra_resource_claims_in_use{driver_name="<any>"}` across all drivers.
   In v1.36.1 the per-driver series disappears after its last prepared claim;
   [the collector's backing calculation](https://github.com/kubernetes/kubernetes/blob/v1.36.1/pkg/kubelet/cm/dra/claiminfo.go#L252-L273)
   always retains the aggregate. A missing aggregate metric fails this gate,
   even if a per-driver zero is present. While active, both the aggregate and
   fixture-driver counts must be one. Only then is worker B cordoned, drained and
   uncordoned. Consumer A keeps the same Pod UID and container ID.
3. Both drivers roll to the second digest while consumer A stays prepared.
   A Pod watch starts from the pre-upgrade resourceVersion. For each node,
   the old container's termination event must precede the replacement's running
   event, and its recorded finish time must precede or equal the replacement's
   start time. Both the old Pod's deletion and new Pod's addition must also be
   observed; API deletion notification may arrive after the replacement is
   created. Replacement RPC probes, inventory and runtime image IDs must
   pass. Consumer A retains its UID/container ID, CDI and metadata, then
   completes unprepare and artifact cleanup after deletion.

Evidence includes the original context name, rendered deployment, node
inventories, version, inspected/imported image identities,
baseline/post-maintenance/upgraded Pods and slices, consumer and claim
snapshots, per-claim unprepare logs and zero samples, metric samples,
upgrade watch/order, final diagnostics and
`result.json`. Its `result` preserves the original operation status; `cleanup`
reports cleanup failures independently. Cleanup failure also makes an otherwise
successful run fail. Credential-bearing temporary files are removed.

Run `mise run dra-test-multi-node` for the VM-free orchestration/failure tests;
they are also part of `mise run ci`. The command doubles exercise the actual
Bash and jq predicates, including missing metrics, leftover CDI artifacts,
bad upgrade ordering, partial creation and failed cleanup.

Widget acceptance establishes framework lifecycle evidence. Hardware
discovery, attachment, release, admission policy, credentials and least-privilege
deployment still require representative qualification by each driver. This
run adds no optional DRA gate and does not qualify seamless surge upgrades.

## Acceptance: 2026-11-05

Task `k8s-device-plugin-rs-mok` passed on a fresh v1.36.1 control plane and
two workers using Container 1.6.1 and `1.6.1-k8s-versioned` at `6fc0528`.
All three nodes were native arm64 with `rosetta=false`. Both linux/arm64
images built with Rust 1.98 and schema 1.37, from clean snapshots of
`v0.4.1` (`8e975fd`) and candidate `342178d`. The unrelated local core feature
edit was excluded. The h2 compatibility snapshot remained unchanged.

The [recorded evidence](validation/mok-2026-11-05.json) contains full OCI
index/manifest digests, per-node imported config IDs, actual Pod image IDs,
claim allocations, consumer identities, unprepare logs, zero samples and
old/new Pod lifetimes. The accepted runner SHA-256 is
`70761971a71e03ed64f40667c4beba22846c784b4d68e9db075b4e68b7be24db`.
Raw diagnostics remain in `/private/tmp/mok-work.CVt5y0/evidence-9` on the
validation host; the source-controlled record preserves the relevant results.

The native run reported:

```text
Per-node concurrent allocation, CDI and metadata isolation passed
Safe maintenance and consumer continuity passed
Immutable-image upgrade, consumer continuity and final unprepare passed
```

For worker 1, termination event 4 preceded replacement-running event 9;
for worker 2, events 16 and 23 established the same ordering. The corresponding
finish/start timestamps were `15:47:27Z` and `15:48:00Z`, respectively. Kubelet
timestamps have second precision, so the watch event ordering supplies the
additional evidence when finish and start fall within the same second.
Both old Pod deletions and replacement additions were captured.

`mise run ci` passed from a clean snapshot on native arm64 Rust 1.99.1:
203 workspace tests, 123 DRA health/example tests, 28 matrix tests, five
profile tests, 14 multi-node tests, formatting and strict Clippy. Independent
consumers selected both schemas, with and without resource health.
The multi-node tests include replacement startup observed before old
termination, deferred Pod deletion notification, missing aggregate metrics,
nonzero claims, leftover CDI files and provisioning/operation/cleanup failures.

The runner and cleanup both exited zero. The new cluster was deleted, all
three pre-existing VMs remained stopped with native arm64/Rosetta-disabled
configuration, and the selected context remained `dra-validation-advanced-137`.
The local core manifest and private review files retained their original
hashes. This acceptance covers the widget framework gates described above;
hardware deployment qualification remains independent.
