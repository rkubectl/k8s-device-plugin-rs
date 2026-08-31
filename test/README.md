# k8s-device-plugin-test

Test-only mocks and helpers for Rust Kubernetes Device Plugin and Dynamic
Resource Allocation (DRA) drivers.

The crate provides mock kubelet registration and protocol clients used by this
workspace's integration tests. Driver projects can use it to exercise their
own implementations without a live kubelet; it is not required at runtime.
