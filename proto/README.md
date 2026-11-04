# k8s-device-plugin-proto

Generated Rust bindings for Kubernetes Device Plugin, DRA, and plugin
registration gRPC protocols. The `dra` Cargo feature enables the stable DRA
`v1` bindings; `dra-health` additionally enables the resource-health protocol.

The package includes the required kubelet protocol sources from the
`proto/kubelet` submodule. Building from a Git checkout requires that submodule;
crates.io packages include the protocol sources directly. The build script uses
the Rust `protox` compiler and Tonic/Prost generator, so no external `protoc`
installation is required for any feature combination.

The submodule is pinned to the upstream `kubernetes-1.37.2` tag, aligned with
the Kubernetes v1.37 schemas in `k8s-openapi` 0.29 / `kube` 5. Git records the
tag's exact commit in the submodule entry. All four compiled protocol files
and LICENSE are byte-identical to the previous `kubernetes-1.36.5` pin.
The supported cluster baseline remains v1.36; the protocol source tag and
Kubernetes object API versions are separate compatibility choices. For future
ecosystem upgrades, review a matching stable kubelet tag, compare protocol
changes, verify generated bindings, and run the Kubernetes compatibility matrix
before advancing the pin or cluster baseline.

Most driver authors should depend on `k8s-device-plugin-lib` or
`k8s-device-plugin-dra` instead of using these generated bindings directly.
