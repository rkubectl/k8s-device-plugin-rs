# k8s-device-plugin-dra-core

DRA backend contracts, inventory, quantities and health reporting types.
Direct consumers select one supported `k8s-openapi` schema (`v1_36` or
`v1_37`); the crate selects neither. Classic core and the standalone Quantity
crate remain usable without a Kubernetes schema. Implement `ResourcePool` and `ClaimPreparer` to provide a
`DraDriver`; use the DRA runtime to publish inventory and serve kubelet claims.

Override `ResourcePool::inventory()` to publish one coherent snapshot with
explicit device taints. Its default wraps `devices()`. For shared counters,
override `ResourcePool::snapshot()` to return `PoolInventory` with `CounterSet`s
and references from `DeviceCounterConsumption`. See the
[taint guide](../docs/dra-device-taints.md) and
[shared-counter guide](../docs/dra-shared-counters.md).

`Quantity` represents exact fractions and Kubernetes units. Parse values such
as `"0.5"` or `"1.5Gi"`, or construct integers with `Quantity::from(2)`.

Health reporting contracts are always available. A runtime backend explicitly
activates reporting with `DraPlugin::with_resource_health`; inventory's
`PoolDevice::health` remains informational and does not replace those reports.
See the [DRA runtime guide](../dra/README.md) for the integration contract.
