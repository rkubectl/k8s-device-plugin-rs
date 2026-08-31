# k8s-device-plugin-dra

`k8s-device-plugin-dra` is the Dynamic Resource Allocation (DRA) runtime for
this workspace. A DRA driver publishes the devices present on each node as
Kubernetes `ResourceSlice` objects, then serves kubelet's
`NodePrepareResources` and `NodeUnprepareResources` RPCs for the claims
allocated to that node.

This is deliberately Phase 1 infrastructure, not a production-ready example
driver. It supports the Kubernetes `resource.k8s.io/v1` API, a single
`ResourceSlice` per pool, and claim preparation. Health streaming, multiple
API versions, and a polished deployable example are outside this phase; see
the [DRA design](../docs/dra-design.md) for the complete boundary and roadmap.

## Quickstart

Implement `ResourcePool` to report the pool/device snapshot and
`ClaimPreparer` to prepare or release allocated devices. `DraDriver` ties the
two traits together. Then create `DraPlugin` with a Kubernetes client, the
driver name, and the local node name, and call `.run()`:

```rust
let client = kube::Client::try_default().await?;
let plugin = DraPlugin::new(client, "dra.example.com", node_name, driver);
plugin.run().await
```

[`examples/minimal_driver.rs`](examples/minimal_driver.rs) is the complete,
copyable version of that pattern. It implements a static `widget-0` device
without CDI hardware backing, so it is suitable for repeatable cluster
validation. Keep it as the source of truth for the quickstart instead of
copying a second implementation into this README; Cargo compiles and tests it
directly:

```bash
cargo test -p k8s-device-plugin-dra --example minimal_driver
```

The example reads these environment variables:

| Variable | Default | Meaning |
|---|---|---|
| `DRIVER_NAME` | `dra.example.com` | DRA driver name and kubelet plugin-directory component. |
| `POOL_NAME` | `widget-pool` | Resource pool advertised by the `ResourceSlice`. |
| `DEVICE_NAMES` | `widget-0` | Comma-separated static device names. |
| `NODE_NAME` | *(required)* | Kubernetes node that owns the published `ResourceSlice`. |
| `RUST_LOG` | *(unset)* | Standard `tracing-subscriber` filter, for example `info`. |

`DraPlugin::run()` creates and serves sockets below
`/var/lib/kubelet/plugins_registry/` and
`/var/lib/kubelet/plugins/<driver-name>/`. It also needs Kubernetes API access
to look up the local `Node`, publish its `ResourceSlice`, and resolve claims;
unlike a classic device plugin, it cannot run meaningfully without a
Kubernetes client configuration and `NODE_NAME`.

## Cluster validation deployment

[`k8s/`](k8s/README.md) provides a small validation-only DaemonSet, RBAC, and
Kustomization for `minimal_driver`. Build the image from the repository root,
make it available to the target cluster, then apply it:

```bash
docker build -f dra/Dockerfile -t k8s-device-plugin-dra-example .
kubectl apply -k dra/k8s
```

The target cluster must expose `resource.k8s.io/v1` with DRA enabled. The
DaemonSet explicitly uses `maxSurge: 0`: Phase 1 cannot assume kubelet support
for seamless DRA-plugin upgrades, so an update may leave a brief gap while the
old node plugin terminates before the new one starts. The RBAC is intentionally
broad for this validation fixture and is not production guidance; see the
[manifest README](k8s/README.md) for that limitation.
