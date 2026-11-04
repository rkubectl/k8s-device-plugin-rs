# k8s-device-plugin-test

Test-only mocks and helpers for Rust Kubernetes Device Plugin and Dynamic
Resource Allocation (DRA) drivers.

The crate provides mock kubelet registration and protocol clients used by this
workspace's integration tests. Driver projects can use it to exercise their
own implementations without a live kubelet; it is not required at runtime.

The `dra` feature leaves Kubernetes schema selection to the consuming test
project. Add `k8s-openapi = { version = "0.29", features = ["v1_36"] }` to that
project's dev-dependencies, or choose `v1_37`; all dependencies must agree on
one schema. See the [DRA guide](../dra/README.md#kubernetes-schema-selection).

Mock connection APIs accept `AsRef<camino::Utf8Path>`. Registration-server
`socket_path()` accessors return a borrowed `&Utf8Path`; use `.as_str()` for a
protocol string or `.to_owned()` when an independently owned path is needed.
Fixtures use `camino_tempfile::Utf8TempDir`/`NamedUtf8TempFile`, preserving the
same cleanup ownership as `tempfile` while making temporary paths UTF-8.
