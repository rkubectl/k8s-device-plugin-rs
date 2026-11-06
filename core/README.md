# k8s-device-plugin-core

Shared, transport-independent types and traits for Rust Kubernetes device
plugin and Dynamic Resource Allocation (DRA) drivers.

Use `k8s-device-plugin-lib` to serve the classic Device Plugin API, or
`k8s-device-plugin-dra` to publish DRA resources and serve kubelet claim
preparation. This crate is useful when a backend implementation must be shared
between those runtimes without depending on gRPC or the Kubernetes API client.

Device and allocation `validate()` methods check protocol IDs/paths without
accessing the filesystem. Runtimes still validate at their RPC boundaries.
Wire projection stays in the transport/API adapters so core does not depend
on generated gRPC or Kubernetes API types, including through optional features.

The repository [README](../README.md) provides the framework overview, and
the [DRA crate guide](../dra/README.md) documents DRA-specific integration.

DRA backends can override `ResourcePool::inventory()` to publish one complete
snapshot of `InventoryDevice`s with explicit device taints. Its default wraps
existing `devices()` snapshots; the `PoolDevice` layout remains compatible.
See the [taint integration guide](../docs/dra-device-taints.md) for policy,
timestamp, and claim-toleration behavior.

For partitionable devices, override `ResourcePool::snapshot()` to return
coherent `PoolInventory` devices and shared `CounterSet`s. Each
`InventoryDevice::with_consumes_counters(...)` references a set through
`DeviceCounterConsumption`, using the same exact `Quantity` type as capacities.
The default snapshot delegates to existing inventory implementations. See
the [shared-counter guide](../docs/dra-shared-counters.md) for card/pair
allocation, validation limits, and publication behavior.

`Quantity` accepts exact fractions and Kubernetes units through `FromStr`,
for example `"0.5".parse::<Quantity>()?` and `"1.5Gi".parse()?`. Construct
integers with `Quantity::from(2)`. This replaces the old public `i64` tuple
field and `Copy` API; see the [migration guide](../docs/plans/kubernetes-quantities.md)
for source changes, dependency assessment and wire compatibility.

Paths carried by device allocations use `camino::Utf8PathBuf`; path-taking
helpers use `camino::Utf8Path`. This encodes kubelet/CDI's UTF-8 requirement in
`DevicePath` and `HostMount` instead of relying on transport-time conversion.
String literals and `String` inputs still work with `.into()` and the existing
constructors. Convert OS-provided `std::path::PathBuf` values explicitly with
`Utf8PathBuf::try_from(path)` or `Utf8PathBuf::from_path_buf(path)`, handling
non-UTF-8 input before constructing an allocation. `from_path_buf` returns the
original path on failure, preserving its bytes for diagnostics.

This is a source-breaking migration: path fields and `ValidationError::RelativePath`
now contain `Utf8PathBuf`, and `validate_absolute_path` accepts `&Utf8Path`.
`ValidationError::NonUtf8Path` is removed because invalid encoding cannot enter
these types. Empty/relative path validation remains. The generic
`bind_unix_listener` helper still accepts standard OS paths through `AsRef<Path>`.
