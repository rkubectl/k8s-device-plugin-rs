# Next DRA adapters

Parent: `k8s-device-plugin-rs-qt7.6`.

## Selected scope

On 2026-10-23 the maintainer selected this sequence:

1. `qt7.6.3`: Device taints, including administrator DeviceTaintRules.
2. `qt7.6.4`: Device binding conditions.
3. `qt7.6.5`: Workload ResourceClaims.
4. `qt7.6.6`: Resource-pool status.

Reassess after those four. Keep the other advanced capabilities deferred.
The existing validation-only widget driver supplies reproducible consumers;
no hardware backend or production deployment is implied by those fixtures.

## Compatibility and evidence

Keep the default v1.36 baseline, existing ResourcePool implementations and
PoolDevice struct literals working. Add an optional complete inventory
snapshot to ResourcePool, with a default conversion from the existing
`devices()` method and correct Arc delegation. An inventory-device wrapper
carries opt-in metadata and leaves the v0.1.0 PoolDevice layout intact.
New extensible types use constructors/builders. The publisher reads one
complete snapshot per reconciliation.

Graph tools are unavailable in this session. Use exact source reads for the
core trait, Arc delegation, ResourceSlice publication/generation comparison,
claim resolution, status publication, and relevant tests. Verify live API
discovery on the target profile as well as published Kubernetes documentation.
Taints and administrator rules are stable in v1.37; binding conditions remain
beta/default-on. Workload ResourceClaims are beta/default-off in v1.37, and
resource-pool status is alpha/default-off.
The installed k8s-openapi 0.28 schema targets v1.36; use explicit discovery or
dynamic API access for v1.37-only API versions rather than assuming generated
types cover them.

## NUMA dependency review

The maintainer authorized standard NUMA attributes wherever a selected
capability depends on them. The selected generic paths use taint policies,
claim readiness conditions, PodGroup claim references, and pool-status
filters; none of the current widget fixtures requires NUMA co-placement.
The core already publishes qualified integer attributes, so a backend with
known locality can supply `resource.kubernetes.io/numaNode` through
`PoolDevice::attribute(..., AttributeValue::Int(node))`. Unknown locality
must remain absent, including a Linux sysfs value of -1; do not invent NUMA
node 0 for the fake widget. Scalar NUMA attributes need no gate. List-form
NUMA values require DRAListTypeAttributes and remain outside this cycle
unless an actual selected adapter needs them. Recheck this decision when
the binding controller and workload fixture are implemented.

Reference: [Kubernetes standard DRA device attributes](https://kubernetes.io/docs/reference/node/dra-standard-device-attributes/).

## 1. Device taints

Add pure core taint/effect types and inventory builders; publish them as
ResourceSlice devices. Preserve server-defaulted timeAdded values for
unchanged taints, normalize empty values/order, respect the 16-taint limit,
and split tainted slices at 64 devices. Taint addition/removal must advance
the pool generation exactly when required; repeated snapshots must converge.
Do not infer taints from health or implement a second eviction controller.

Extend the fake widget backend with explicit validation-only taint controls.
Test NoSchedule rejection, matching claim tolerations, NoExecute eviction,
cleanup, and unpreparation. Validate driver taints on v1.36 and both driver
and administrator taints on v1.37, where taints/rules are stable. Administrator
rules remain operator-owned and must not widen the driver's RBAC.

## 2. Device binding conditions

Publish opt-in binding and failure-condition names with inventory metadata.
Add an opt-in field-preserving operation to the existing UID/allocation-checked
ClaimDeviceStatusPublisher so a node preparer reporting data cannot erase
readiness conditions reported by a controller. Keep the existing complete-entry
publication behavior available. Report conditions from a separate
controller/monitor before Pod binding; reporting
only from NodePrepareResources would deadlock scheduling. Supply a small
validation controller and fixture proving allocation remains unbound until
readiness, successful binding/CDI afterward, and failure-condition handling.
Keep allocation/scheduling ownership in Kubernetes.

## 3. Workload ResourceClaims

Discover the served PodGroup/Workload API and required feature gates on an
isolated v1.37 validation profile. Add a workload/PodGroup shared-claim
example and live fixture. Verify multiple member Pods can use one claim,
UID-safe/idempotent preparation, and that removing one consumer does not
release devices while another still uses them. Confirm final cleanup and
unpreparation. Change the runtime only if this exercise reveals a real gap.

## 4. Resource-pool status

Provide an opt-in client/example for ResourcePoolStatusRequest, using API
discovery, bounded waiting, and explicit request cleanup. Kubernetes'
controller owns the result calculation; do not duplicate its accounting in
the driver. On a gated validation profile prove total/allocated/available
counts before, during, and after a widget claim, and report unsupported APIs
clearly. Keep alpha APIs out of default driver behavior.

## Validation and completion

Each child records focused unit/integration tests, strict Clippy/format
checks, live evidence where required, docs, and a synchronized Beads export
in its implementation commit. Preserve maxSurge: 0 and rosetta=false. Run
profiles serially and restore the default v1.36 profile after validation.
Gate-dependent tests use an isolated profile rather than changing the
standard matrix's feature configuration. Commit the selected plan before
implementation; close only children whose acceptance checks actually pass.

References verified on 2026-10-23:

- https://kubernetes.io/docs/reference/command-line-tools-reference/feature-gates/
- https://kubernetes.io/docs/concepts/resource-management/dynamic-resource-allocation/device-taints/

## Progress

- `qt7.6.3` completed on 2026-10-23. The [taint guide](../dra-device-taints.md#validation-evidence-2026-10-23)
  records passing native v1.36.1/v1.37.0 acceptance, administrator rules,
  full CI, and baseline restoration. The remaining children retain the
  selected order; no NUMA dependency has been introduced by the taint path.

- `qt7.6.4` completed on 2026-10-23. The [binding guide](../dra-binding-conditions.md#validation-evidence-2026-10-23)
  records successful native readiness/failure lifecycles on both profiles,
  cooperative status publication, full CI, and baseline restoration.
  Node pinning uses the allocation selector and does not require NUMA.

- `qt7.6.5` shared lifecycle acceptance passed on 2026-10-23. The
  [Workload guide](../dra-workload-claims.md) records the isolated gated
  profile and same-UID two-Pod lifecycle. No runtime change or NUMA
  dependency was needed for Basic scheduling.
