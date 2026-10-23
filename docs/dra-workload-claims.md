# Workload ResourceClaims

The DRA runtime can prepare a claim shared by multiple Pods in a PodGroup.
Kubernetes owns claim generation, allocation, reservations and member-Pod
projection; the node driver receives the same claim UID through the ordinary
prepare/unprepare protocol. No driver-side scheduler or PodGroup controller
is needed.

This adapter supplies an opt-in native validation profile and a reproducible
consumer fixture. It does not enable cluster feature gates automatically in
the deployed driver. Basic shared-claim scheduling has no NUMA dependency.
Known hardware locality can still use the existing qualified integer attribute
API; unknown locality remains absent.

## Requirements and profile

The pinned validation target is Kubernetes v1.37.0 with `GenericWorkload` and
`DRAWorkloadResourceClaims` enabled in the API server, controller manager,
scheduler and kubelet. The profile serves `scheduling.k8s.io/v1beta1` PodGroup
and Workload APIs. The standard v1.36/v1.37 profiles keep their existing gates.

On native arm64 macOS with Container 1.6 and the maintained `k8s-versioned`
helper, stop other Kubernetes profiles before creating the isolated profile:

```sh
container stop dra-validation
./dra/hack/create-advanced-profile.sh
```

Provisioning requires `container`, `kubectl`, `jq`, Mike Farah's `yq` and
Python 3. It refuses an existing profile name, a concurrent Kubernetes VM,
non-arm64 macOS, or Rosetta. It merges only the selected gates/runtime APIs
into a newly created profile, keeps unrelated configuration, regenerates
static control-plane manifests, and proves API discovery after a second
guarded VM restart. Temporary credential-bearing configuration is private
and removed on exit. On failure the newly created VM is stopped and retained
for inspection. The script leaves the new profile running on success; stop
it before starting another profile.

The same profile also enables `DRAResourcePoolStatus` and
`resource.k8s.io/v1alpha3` for the next selected adapter. Neither advanced API
is a requirement of the default driver. Profile overrides must use a new
`dra-validation-advanced-*` name through `DRA_E2E_ADVANCED_CLUSTER`.

## Shared claim fixture

[`e2e-workload.json`](../dra/hack/e2e-workload.json) explicitly creates a
Workload template, its corresponding PodGroup, a ResourceClaimTemplate,
a DeviceClass and two member Pods. A real application controller owns the
Workload-to-PodGroup lifecycle; this fixture creates both explicitly. The
Basic scheduling policy deliberately avoids gang/topology constraints.

PodGroup and Pods reference the same claim name and ResourceClaimTemplate.
Kubernetes generates one Group-owned ResourceClaim and projects its name
into both Pods' statuses. The fixture checks the exact Group and claim UIDs,
the Group reservation, CDI delivery to both Pods, continued operation after
one Pod is deleted, final kubelet unpreparation, and generated-claim garbage
collection after the Group is deleted. Cleanup preserves failures.

With a current canonical fixture image available locally, run the serial
matrix from the repository root:

```sh
DRA_E2E_SKIP_BUILD=1 DRA_E2E_WORKLOAD=1 \
  DRA_E2E_V137_CLUSTER=dra-validation-advanced-137 \
  ./dra/hack/validate-k8s-versions.sh
```

The ordinary v1.36 smoke runs first; core and extended-resource smokes then
run on the advanced v1.37 profile before the shared-claim test. The matrix
restores `dra-validation` as the sole running Kubernetes profile. Omit
`DRA_E2E_SKIP_BUILD=1` to build a fresh native image. The Workload test can
also run directly against an already deployed, untainted driver without
binding-condition controls:

```sh
KUBE_CONTEXT=dra-validation-advanced-137 ./dra/hack/e2e-workload.sh
```

## API references

The fixture follows the pinned upstream [v1.37 scheduling API types](https://github.com/kubernetes/kubernetes/blob/v1.37.0/staging/src/k8s.io/api/scheduling/v1beta1/types.go)
and [claim-name and ownership resolution](https://github.com/kubernetes/kubernetes/blob/v1.37.0/staging/src/k8s.io/dynamic-resource-allocation/resourceclaim/resourceclaim.go).
The installed Rust schema targets v1.36, so the v1.37-only consumer objects
are explicit Kubernetes JSON, checked against live API discovery.

## Validation evidence — 2026-10-23

- The isolated v1.37.0 profile served PodGroup, Workload and pool-status APIs
  after guarded restarts at `.33 -> .34 -> .36`. All profiles had
  `rosetta=false`; existing profiles were not reconfigured.
- The fixed-snapshot matrix passed the v1.36 core smoke and advanced v1.37
  core/extended-resource/shared-claim smokes with the existing native fixture
  image configuration `bd1c6614eaa9e3e6b7e9b192d4fba4a99b8493e2b69e650237e7dd2cb5e19e60`.
  No runtime Rust changes or image rebuild were needed for this capability.
- Both members used generated claim `dra-example-workload-widget-r684w`,
  UID `a57d17b3-6545-4b50-be0b-c80e44dd4f99`. Both prepare calls used that
  UID. No unprepare occurred during ten seconds with the second member
  still active; the final deletion produced matching-UID unpreparation.
  Group deletion garbage-collected the claim and all fixture objects were removed.
- 21 VM-free matrix/fixture checks and five configuration-transform tests
  passed. The transforms also passed a JSON/YAML round trip through native
  yq 4.54.1. The unchanged runtime passed full CI in binding commit `793de21`
  (147 workspace tests, 78 health/example tests, strict Clippy and formatting),
  and GitHub run `39966720579` was successful.
- The complete matrix exited 0 and restored v1.36.1 at `192.168.64.37`,
  Ready with a successful real-RPC liveness probe. It was the sole running
  VM, with the advanced profile and builder stopped.
