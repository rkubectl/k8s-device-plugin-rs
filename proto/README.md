# k8s-device-plugin-proto

Generated Rust bindings for Kubernetes Device Plugin, DRA, and plugin
registration gRPC protocols. The `dra` Cargo feature enables the stable DRA
`v1` bindings; `dra-health` additionally enables the resource-health protocol.

The package includes the required kubelet protocol sources from the
`proto/kubelet` submodule. Building from a Git checkout requires that submodule;
crates.io packages include the protocol sources directly. The build script uses
the Rust `protox` compiler and Tonic/Prost generator, so no external `protoc`
installation is required for any feature combination.

The submodule is pinned to the upstream `kubernetes-1.36.5` tag, matching our
Kubernetes v1.36 baseline. Git records the tag's exact commit in the submodule
entry. When the `k8s-openapi`/`kube` ecosystem supports Kubernetes v1.37,
review the baseline and move to a matching stable kubelet tag in the same
upgrade. Compare protocol changes, verify generated bindings, and run the
Kubernetes compatibility matrix before advancing the baseline. The kubelet
protocol tag and Kubernetes object API versions are separate compatibility
choices; this policy keeps their provenance aligned. Validated optional v1.37
capabilities remain supported on the v1.36 baseline.

Most driver authors should depend on `k8s-device-plugin-lib` or
`k8s-device-plugin-dra` instead of using these generated bindings directly.
