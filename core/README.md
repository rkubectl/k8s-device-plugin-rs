# k8s-device-plugin-core

Transport-independent classic device plugin contracts and shared socket and health primitives. DRA backend contracts live in `k8s-device-plugin-dra-core`.

Classic backends construct `DeviceId` and `ExtendedResourceName` with checked
parsing before passing them to `Device` or `DevicePlugin`. Device IDs preserve
bytes exactly, permit non-DNS names, and must be nonempty and at most 63 UTF-8
bytes. Extended resource names use checked domain/name parsing and reserved
namespace rules. Allocator and optional hook APIs now take `DeviceId` values;
`DevicePlugin::new` takes an `ExtendedResourceName` and is infallible.

Extended resource names follow Kubernetes qualified-name rules, including the
quota `requests.` prefix restriction and a 244-byte domain budget. Suffixes
may contain uppercase ASCII, dots and underscores. Path validation rejects
NUL bytes via `ValidationError::NulByte`; this new enum variant and the
`ExtendedResourceNameError::ReservedPrefix` variant require downstream
exhaustive matches to be updated when upgrading to v0.7.
`ExtendedResourceName` supports string serialization and checked
deserialization; classic core now directly depends on `serde` for this API.

Device and allocation `validate()` methods check paths without
accessing the filesystem. Runtimes still validate at their RPC boundaries.
Wire projection stays in the transport/API adapters so core does not depend
on generated gRPC or Kubernetes API types, including through optional features.

The repository [README](../README.md) provides the framework overview, and
the [DRA crate guide](../dra/README.md) documents DRA-specific integration.

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
