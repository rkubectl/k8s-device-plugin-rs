# k8s-device-plugin-core

Shared, transport-independent types and traits for Rust Kubernetes device
plugin and Dynamic Resource Allocation (DRA) drivers.

Use `k8s-device-plugin-lib` to serve the classic Device Plugin API, or
`k8s-device-plugin-dra` to publish DRA resources and serve kubelet claim
preparation. This crate is useful when a backend implementation must be shared
between those runtimes without depending on gRPC or the Kubernetes API client.

The repository [README](../README.md) provides the framework overview, and
the [DRA crate guide](../dra/README.md) documents DRA-specific integration.
