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
k8s-device-plugin = { version = "0.0.3", default-features = false, features = ["dra"] }
```

The common types are available at the crate root:

```rust
use k8s_device_plugin::{DevicePlugin, DevicePluginService, StaticDevicePlugin};
```

The full focused APIs remain available as modules: `core`, `device_plugin`,
`dra`, and `proto`. The individual `k8s-device-plugin-*` crates remain
supported for applications that prefer explicit, minimal dependencies.
