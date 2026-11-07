# k8s-device-plugin

`k8s-device-plugin` is the unified entrypoint for this workspace's Kubernetes
device-plugin and Dynamic Resource Allocation (DRA) runtimes. It has no
runtime logic of its own: it re-exports the focused crates so applications can
start with one dependency and later opt into a narrower dependency surface.

The default feature set includes the classic device-plugin runtime, DRA, and
the generated protocol bindings. Disable defaults and select only what an
application needs:

```toml
[dependencies]
k8s-device-plugin = { version = "0.5.0", default-features = false, features = ["dra"] }
k8s-openapi = { version = "0.29", features = ["v1_37"] }
```

The common types are available at the crate root:

```rust
use k8s_device_plugin::{DevicePlugin, DevicePluginService, StaticDevicePlugin};
```

The common DRA runtime types are also available at the crate root, including
`DraPlugin`, `ClaimDeviceStatusPublisher`, and `DraPluginLivenessProbe`. The
full focused APIs remain available as modules: `core`, `dra_core`, `device_plugin`, `dra`,
and `proto`. The individual `k8s-device-plugin-*` crates remain supported for
applications that prefer explicit, minimal dependencies.

DRA health support is compiled with the runtime. Backends explicitly activate reporting with `DraPlugin::with_resource_health`:


```toml
[dependencies]
k8s-device-plugin = { version = "0.5.0", default-features = false, features = ["dra"] }
k8s-openapi = { version = "0.29", features = ["v1_37"] }
```

For DRA, the application selects exactly one `k8s-openapi` schema feature:
`v1_36` or `v1_37`. The library leaves this choice to the application.
Classic-only consumers do not need `k8s-openapi`. See the getting-started
guide for the v0.4 migration requirements and dependency override.

Start with the workspace [getting-started guide](../docs/getting-started.md)
to choose between the classic and DRA runtimes and find runnable examples.

## DRA contract migration

DRA backend contracts moved from `k8s-device-plugin-core` to
`k8s-device-plugin-dra-core`. With the facade's `dra` feature, they remain
available at its root and through `dra_core`; DRA validation errors are
exposed as `DraValidationError`, while `ValidationError` remains classic.
The former `resource-health` Cargo feature has been removed. Construct with
`DraPlugin::with_resource_health` to activate reporting. Domain strings and
wire values are unchanged.
