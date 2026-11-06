# k8s-quantity

Exact Kubernetes resource quantities for applications that need fractions,
SI units, exponent notation and numeric equality. This crate is independent
of the device-plugin runtime and does not depend on `kube`, `k8s-openapi`, a
Kubernetes schema or an async runtime by default. It uses no floating-point
arithmetic.

```rust
use k8s_quantity::Quantity;

let half: Quantity = "0.5".parse()?;
assert_eq!(half, "500m".parse()?);
assert_eq!(half.to_string(), "500m");

let memory: Quantity = "1.5Gi".parse()?;
assert_eq!(memory.to_string(), "1536Mi");
assert_eq!(Quantity::from(2).to_string(), "2");
# Ok::<(), k8s_quantity::QuantityParseError>(())
```

The crate uses BigDecimal for exact storage and rounding. The Kubernetes
adapter supplies syntax, unit suffixes and wire formatting. It supports
signed integers/fractions, decimal SI (`n`, `u`, `m`, `k`, `M`, `G`, `T`, `P`,
`E`), binary SI (`Ki` through `Ei`) and decimal exponents. Input is validated
with `FromStr`; integer conversions use `From`. There is no floating-point
conversion or general arithmetic API.

## k8s-openapi conversions

Enable the optional `k8s-openapi` feature to convert directly at API boundaries.
Select exactly one schema on `k8s-openapi` 0.29 in the application, such as
`v1_36` or `v1_37`; this library selects no schema for you.

```rust
use k8s_openapi::apimachinery::pkg::api::resource as apiresource;
use k8s_quantity::Quantity;

let exact: Quantity = "0.5".parse()?;
let wire: apiresource::Quantity = exact.into();
assert_eq!(wire.0, "500m");
let exact = Quantity::try_from(wire)?;
# Ok::<(), k8s_quantity::QuantityParseError>(())
```

`From<Quantity>` and `From<&Quantity>` produce API quantities. The reverse
`TryFrom<apiresource::Quantity>` and `TryFrom<&apiresource::Quantity>` validate
the API type's unchecked string. Core users need no schema unless another
dependency explicitly enables this feature. The DRA runtime enables it and
uses the same conversions.

## Semantics and compatibility

Values finer than nano precision round away from zero, following Kubernetes
v1.36's implementation. BinarySI values saturate at signed-64-bit magnitude.
Equality compares values independently of units or spelling. Display emits a
canonical wire form; `canonical_decimal()` supplies a common notation for
comparing independently serialized Kubernetes objects.

Compatibility is checked against 282 signed cases evaluated by
`k8s.io/apimachinery` v0.36.4. The checked-in fixture runs in ordinary Rust
tests; the optional pinned Go reference generator lives under
[`hack/quantity-oracle`](hack/quantity-oracle). Go is not required to build,
use or test this crate.

This is bounded conformance coverage. Input spelling is not retained;
empty numeric components accepted as zero by Go parser quirks are rejected;
explicit decimal exponents must fit signed 32-bit scale without wrapping.
Decimal values beyond the Exa suffix use exponent notation to preserve their
value instead of reproducing older Go canonicalization's missing suffix.
See the [design, candidate review and migration guide](../docs/plans/kubernetes-quantities.md)
for the exact boundary and validation evidence.

`k8s-device-plugin-core` and the facade re-export `Quantity` and
`QuantityParseError`, so driver code can keep its existing import path while
migrating integer construction from `Quantity(2)` to `Quantity::from(2)`.
The crate participates in the workspace's shared-version, tag-first release
process. This change does not publish a release.
