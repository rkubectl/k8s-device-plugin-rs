# k8s-device-plugin-proto

Generated Rust bindings for Kubernetes Device Plugin, DRA, and plugin
registration gRPC protocols. The `dra` Cargo feature enables the stable DRA
`v1` bindings; `dra-health` additionally enables the resource-health protocol.

The package includes the required kubelet protocol sources from the
`proto/kubelet` submodule. Building from a Git checkout requires that submodule;
crates.io packages include the protocol sources directly. The build script uses
the Rust `protox` compiler and Tonic/Prost generator, so no external `protoc`
installation is required for any feature combination.

Most driver authors should depend on `k8s-device-plugin-lib` or
`k8s-device-plugin-dra` instead of using these generated bindings directly.
