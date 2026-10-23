# k8s-device-plugin-core

Shared, transport-independent types and traits for Rust Kubernetes device
plugin and Dynamic Resource Allocation (DRA) drivers.

Use `k8s-device-plugin-lib` to serve the classic Device Plugin API, or
`k8s-device-plugin-dra` to publish DRA resources and serve kubelet claim
preparation. This crate is useful when a backend implementation must be shared
between those runtimes without depending on gRPC or the Kubernetes API client.

The repository [README](../README.md) provides the framework overview, and
the [DRA crate guide](../dra/README.md) documents DRA-specific integration.

DRA backends can override `ResourcePool::inventory()` to publish one complete
snapshot of `InventoryDevice`s with explicit device taints. Its default wraps
existing `devices()` snapshots; the `PoolDevice` layout remains compatible.
See the [taint integration guide](../docs/dra-device-taints.md) for policy,
timestamp, and claim-toleration behavior.
