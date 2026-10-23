# Device binding conditions

A backend can require asynchronous setup to finish before Kubernetes binds a
Pod. Override `ResourcePool::inventory()` and attach a `DeviceBinding` policy
to each affected `InventoryDevice`. Existing backends retain their default
inventory and declare no binding policy.

```rust
use k8s_device_plugin_core::{DeviceBinding, InventoryDevice, PoolDevice};

let device = InventoryDevice::new(PoolDevice::new("widget-0")).with_binding(
    DeviceBinding::new()
        .with_condition("example.com/Attached")
        .with_failure_condition("example.com/AttachFailed")
        .with_node_binding(),
);
```

Configure at least one readiness condition to make the scheduler wait.
Required conditions must all become `True`. A declared failure condition
becoming `True` aborts binding and lets the scheduler recover. Conditions are
read from the allocated device's `ResourceClaim.status.devices[].conditions`.
The [v1.37 scheduler implementation](https://github.com/kubernetes/kubernetes/blob/v1.37.0/pkg/scheduler/framework/plugins/dynamicresources/dynamicresources.go#L1996)
is the version-pinned reference for this behavior. The allocation copies its
binding-condition names from the published ResourceSlice.

The optional node-binding flag records the selected node in
`ResourceClaim.status.allocation.nodeSelector`. An external attachment
controller can use that selector to perform node-specific setup. This path
does not require NUMA attributes. If a hardware backend separately needs
NUMA co-placement, publish known scalar `resource.kubernetes.io/numaNode`
attributes through the existing integer-attribute API; omit unknown locality.

The publisher supports at most four readiness and four failure conditions per
device, rejects duplicates within each list, and normalizes condition order,
empty lists, and default false node binding for comparison. Policy changes
advance the ResourcePool generation; unchanged snapshots converge without
writes. Kubernetes validates condition-type syntax.

This feature is beta/default-on in v1.36 and v1.37 through
`DRADeviceBindingConditions`. Device-status publication must also be available:
`DRAResourceClaimDeviceStatus` is beta/default-on in v1.36 and stable in v1.37.
An operator that disables a prerequisite must leave this inventory policy off.
The framework does not change cluster feature gates.

## Controller and node responsibilities

An external controller observes an allocated claim, performs the actual
attachment/setup, and reports readiness. This must happen before
`NodePrepareResources`: kubelet reaches preparation after Pod binding.
Report failure when setup cannot complete, and use standard condition
transition-time semantics. The Kubernetes scheduler owns allocation and
binding; the controller must not write `status.allocation` itself.

`ClaimDeviceStatusPublisher::publish_fields()` updates fields supplied as
`Some`, preserving omitted conditions, data, and network data. A controller
can update conditions while a node preparer reports data. Each field still
needs one coordinated owner; the conditions value is a complete list. An
explicit empty condition list clears it. `publish()` retains complete-entry
replacement behavior for backends that own the whole device-status record.

Both operations validate claim UID and the exact allocated driver/pool/device/
share key. They use a resourceVersion precondition and recheck UID/allocation
on conflicts. Field-preserving publication remerges current fields after a
conflict, so a node's newer data survives a controller retry. Other drivers'
entries remain untouched. The example node preparer uses `publish_fields()`
for its `data.phase` update, preserving the controller's readiness conditions.

A controller needs scoped ResourceClaim read and status-patch permissions plus
its driver-specific `resourceclaims/driver` authorization. A controller that
can report across nodes uses the appropriate `arbitrary-node:patch` verb;
node-local reporting uses `associated-node:patch`. See the
[Kubernetes DRA hardening guide](https://kubernetes.io/docs/concepts/security/hardening-guide/dynamic-resource-allocation/).
The checked-in validation DaemonSet's RBAC remains unchanged.

## Validation controller and fixture

[`binding_controller.rs`](../dra/examples/binding_controller.rs) performs one
explicit simulated attachment action for an already allocated claim. It
requires the namespace, name, expected UID, and `pending`, `ready`, or `failed`
state. It selects the supplied kubeconfig context, uses bounded transport
requests, preserves transition times when status stays the same, and reports
through the safe field-preserving publisher.

```sh
KUBE_CONTEXT=<context> \
CLAIM_NAMESPACE=k8s-device-plugin-dra-example \
CLAIM_NAME=dra-example-binding CLAIM_UID=<expected-uid> \
BINDING_STATE=ready \
  cargo run --locked -p k8s-device-plugin-dra --example binding_controller
```

Replace the simulation with the backend's real setup/readiness check before
using this pattern for hardware. The validation controller uses the caller's
Kubernetes credentials; it is separate from the node preparer.

Build/deploy the current validation image, then run from this Cargo workspace:

```sh
KUBE_CONTEXT=<context> dra/hack/e2e-binding.sh

# Or include binding checks in the guarded serial local matrix:
DRA_E2E_BINDING=1 mise run dra-validate-k8s
```

The fixture temporarily enables `DRA_EXAMPLE_BINDING_CONDITIONS=1` on the fake
widget driver. It waits for inventory policy publication, proves Pending
keeps an allocated Pod unbound, rejects a stale UID, and then reports Ready.
It verifies CDI injection, retained readiness after node preparation, an
idempotent repeated report, and unpreparation after deletion. A fresh claim
then reports Failed and must produce a scheduler binding-failure event
without reaching kubelet preparation. Cleanup deletes fixture objects,
removes the example control, and waits for inventory policy removal. The
outer matrix restores the v1.36 baseline. NUMA topology is not fabricated for
this fake device.

## Validation evidence (2026-10-23)

Beads `k8s-device-plugin-rs-qt7.6.4` passed full local CI: 147 workspace
tests, strict Clippy/format checks, 78 resource-health/example tests, and 18
VM-free orchestration/preflight/fixture checks. Focused coverage includes
binding metadata/default normalization/limits/generation changes, retained
taint timestamps, partial-field cooperation and conflict remerging, stale-UID
retry refusal, complete-entry behavior, explicit condition clearing, and
controller state/transition-time semantics.

The corrected fixed-snapshot serial matrix exited 0 on native v1.36.1 and
v1.37.0, with `rosetta=false`. Both passed ordinary CDI smoke plus the full
Pending, stale-UID refusal, Ready/CDI, retained readiness after preparation,
idempotent `Unchanged` report, unprepare, and Failed/scheduler-recovery path.
v1.37 also passed the existing extended-resource fixture. The native image
config was
`sha256:bd1c6614eaa9e3e6b7e9b192d4fba4a99b8493e2b69e650237e7dd2cb5e19e60`.
The first attempt's relocated script used a nonexistent manifest path; Cargo
workspace discovery now runs before mutation, with a regression test, and
the corrected full matrix passed. Final checks found the v1.36 baseline
Ready at `192.168.64.32`, with successful RPC liveness and no taints/binding
policy; all other VMs, including the builder, were stopped. No NUMA attribute
was required or invented.
