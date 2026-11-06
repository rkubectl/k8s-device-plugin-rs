# Exact Kubernetes quantities

Tracking: `k8s-device-plugin-rs-56b`. The maintainer approved the public
Quantity API migration on 2026-11-06.

## Problem and ownership

The previous `core::Quantity(pub i64)` represented only integers. Both
`PoolDevice::capacity` and the shared-counter inventory used it, and the DRA
publisher constructed Kubernetes quantities from that integer. This excluded
valid values such as `0.5`, `500m`, and `1.5Gi`. The counter publisher also
contained its own integer-only normalization.

The standalone `k8s-quantity` workspace crate now owns a validated exact
Quantity type backed by BigDecimal. Core and the facade re-export it. Capacities,
counter sets, and device consumption use the same type. The DRA transport
converts owned quantities to wire strings and compares API values numerically,
including when the server uses another notation family. There is no change to
claim identities, preparation RPCs, CDI files, persisted journals, or the
ResourceSlice API version.

## Existing Rust implementation review

The bounded search covered crates.io, GitHub code search, published source,
and several unpublished ports. It did not establish a suitable fully
conformant standalone Rust Quantity implementation.

| Candidate | Source-checked finding |
|---|---|
| [`kube_quantity` 0.9.0](https://github.com/ThomasK33/kube-quantity-rs) | Parses through `nom::double`/`f64`, then constructs a Decimal. A native probe changes `9007199254740993` to `9007199254740992`, accepts invalid `++1` and `1e3Ki`, and rejects valid `1E` and `1Ei`. Formatting retains fractional mantissas. |
| [`kube_quantity_2` 0.6.1](https://github.com/fluktuid/kube-quantity-rs) | The fork retains the same floating-point parsing path. |
| [`k8s_quantity_parser` 0.1.0](https://github.com/sombralibre/k8s-quantity-parser) | CPU/memory conversion helpers parse integer mantissas; fractional quantities are not supported. |
| [`k8s-openapi` 0.29 Quantity](https://docs.rs/k8s-openapi/0.29.0/k8s_openapi/apimachinery/pkg/api/resource/struct.Quantity.html) | Correct wire string wrapper, without parsing, validation, or numeric equality; also requires a consumer-selected Kubernetes schema. |
| [`Oxikube` Quantity](https://github.com/karan-vk/Oxikube/blob/main/crates/domain/oxikube_domain/src/quantity.rs) | Exact implementation with a Go-derived corpus, but explicitly changes BinarySI saturation and imposes its own range/spelling bounds. It is part of an unpublished application domain crate. |
| [`vibecode-kubernetes-rust` k8s-quantity](https://github.com/mkowalski/vibecode-kubernetes-rust/tree/main/crates/k8s-quantity) | Unpublished port; inspected code omits nano rounding and BinarySI saturation, and no reusable license was declared in the inspected crate/repository metadata. |

BigDecimal 0.4.11 supplies arbitrary-precision decimal storage, parsing of the
numeric component, comparison, and rounding. A small standalone adapter supplies
Kubernetes syntax, suffixes and wire formatting. This adds BigDecimal and its
numeric dependencies to core, while keeping Kubernetes clients and generated
API schemas out of classic-only dependency graphs. No general quantity
arithmetic API or floating-point conversions are introduced.

The optional `k8s-openapi` feature implements owned and borrowed `From`
conversions to the API Quantity and `TryFrom` conversions back. The reverse
path validates the API wrapper's unchecked string. It selects no Kubernetes
schema itself; applications select a schema on k8s-openapi 0.29. DRA enables
the feature and uses the conversions at the wire boundary, while the
standalone crate's default dependency graph remains schema-free.

## Numeric and wire contract

- Accept signed decimal numbers with decimal SI suffixes `n/u/m/k/M/G/T/P/E`,
  binary SI suffixes `Ki/Mi/Gi/Ti/Pi/Ei`, or signed decimal exponents.
- Preserve exact values beyond the precision of `f64`.
- Round finer fractions away from zero to nano precision, following the
  actual Kubernetes v1.36 parser. The generated Quantity documentation's
  older three-decimal-place description is not the current implementation.
- Match BinarySI saturation at signed-64-bit magnitude; decimal quantities
  retain their larger exact values, as the upstream parser does.
- Reject malformed strings before they can enter an inventory. Empty numeric
  components such as `+`, `.`, and `Gi` are rejected according to the documented
  grammar, rather than reproducing Go's permissive zero-valued parser quirks.
  Decimal exponents must fit the upstream signed-32-bit scale, without wrapping.
- Emit canonical forms such as `0.5 -> 500m` and `1.5Gi -> 1536Mi`. Original
  spelling is not retained. Decimal values beyond the Exa suffix use exponent
  notation to preserve their value instead of reproducing older Go
  canonicalization's missing-suffix behavior.
- Equivalent quantities compare equal in inventories and reconciliation.
  The publisher normalizes both desired and existing comparison copies,
  including capacities, counter definitions, and consumption. A spelling
  change does not advance a generation; a real value change does.

The independent reference fixture contains 282 inputs from Kubernetes
v1.36.4's `TestQuantityParse` table, expanded with both signs and evaluated by
`k8s.io/apimachinery` v0.36.4 on native arm64 Go. Rust tests compare numeric
values against the Go-emitted reference and verify Rust wire round trips.
An independent native Go check also parsed all 282 Rust-emitted strings and
compared them with the original inputs using Kubernetes `Quantity.Cmp`;
all values matched.
This is bounded conformance evidence, not a claim that every Go parser quirk
or arbitrary extreme input is reproduced.

The optional reference generator is pinned in `quantity/hack/quantity-oracle`;
Go is not required by Rust builds or CI. To reproduce the checked-in data:

```sh
jq -s 'map(.input)' quantity/src/tests/apimachinery-v1.36.4.jsonl > /tmp/quantity-inputs.json
(cd quantity/hack/quantity-oracle && go run . < /tmp/quantity-inputs.json) |
  jq -c '.[]' > /tmp/quantity-reference.jsonl
diff -u quantity/src/tests/apimachinery-v1.36.4.jsonl /tmp/quantity-reference.jsonl
```

## Migration and rollback

This is a source-breaking library API change and belongs in the next
compatible release boundary; no release is performed by this task.

| Previous code | Replacement |
|---|---|
| `Quantity(2)` | `Quantity::from(2)` |
| Integer-only input | `"0.5".parse::<Quantity>()?` or `"1.5Gi".parse()?` |
| `.0` field access | `to_string()`, `into_string()`, or numeric equality |
| Implicit `Copy` | Borrow the value or use `clone()` for an independently owned value |

The publisher continues to use Kubernetes wire strings, so old integer
inventories and new quantities can coexist across nodes and driver versions.
An update may canonicalize an old integer spelling once; subsequent polls
must converge. Keep `maxSurge: 0` and the existing normal plugin lifecycle.
There is no stored-data migration or destructive contraction.

Rollback is a normal driver-image rollback after changing backend inventory
to integers if the older backend cannot represent fractions. Existing
integer-only clients can remain on the preceding crate version. No metadata
journal or CDI format needs to be rewritten or removed.

## Acceptance — 2026-11-06

`mise run ci` passed after extraction and OpenAPI integration: 221 workspace
tests, strict Clippy, resource-health targets/examples, 29 matrix tests,
14 multi-node tests, five profile tests, and independent schema 1.36/1.37
consumers with and without resource health. Sequential checks also passed
all-target/all-feature workspace tests and Clippy on schema 1.36, Quantity
conversion tests and doctests on both schemas, a schema-free standalone
default test run, and core compilation without default features.

Strict all-feature rustdoc passed. `cargo package -p k8s-quantity` verified
the package independently, including its optional conversions; the optional
Go fixture generator is excluded from the package. The reference generator
reproduced all 282 fixture rows byte-for-byte, and the direct Go numeric
comparison of Rust-emitted values reported no failures.

The final image
`docker.io/library/k8s-device-plugin-dra-example:quantity-56b-final` built
with Rust 1.98 for linux/arm64, config digest
`sha256:347616bdfe295c86a38af86a7a371748b567348b21084283b61de2a0ef6c892f`.
The running v1.37 driver Pod reported that exact digest. Native Kubernetes
v1.36.1 and v1.37.0 passed the baseline and FirstAvailable CDI paths plus the
fractional fixture: one unit per pair, `0.5` consumed per card, `0.5` device
capacity, canonical `500m` API values, unchanged polls, scheduling exclusion,
release/retry and CDI. Both slice kinds shared generation/count. The v1.37
extended-resource generated-claim lifecycle passed after normal widget
inventory was restored.

The matrix and outer cleanup exited zero. Both profiles and the builder
returned to their original stopped state, all arm64 with `rosetta=false`.
The run retained `maxSurge: 0` and used a temporary kubeconfig.
No release was published. The separate ownership/standards audit is tracked
by `k8s-device-plugin-rs-zw5`.
