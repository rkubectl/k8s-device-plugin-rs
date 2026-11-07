# Contributing

Start with [AGENTS.md](AGENTS.md) for issue tracking and repository conventions.
Track implementation and validation in Beads and commit the synchronized
`.beads/issues.jsonl` with the work. Preserve unrelated local edits.

## Rust ownership and conversions

Identify the invariant and the type responsible for it before adding a helper.
Prefer an inherent method when behavior uses that type's state or invariants.
Use the standard conversion traits when the source/target pair describes the
operation naturally: `From` for infallible projections, `TryFrom` for fallible
conversions, and `FromStr` for parsing. An equivalent `to_wire` wrapper adds no
value beside `From`; callers can use `.into()` or the target's `from` method.

Choose ownership deliberately. Consume values that are no longer needed and
move their fields. Borrow retained snapshots, retry state and shared data;
do not add a clone merely to fit a helper's signature. Preserve semantically
distinct constructors such as version attributes, and document intentional
wire projections/normalization rather than implying a reversible conversion.

Rust orphan rules apply across workspace crates. DRA cannot implement
`From<dra_core::PoolDevice>` for an OpenAPI device because both types are foreign.
Core's pure types/traits intentionally stay independent of generated gRPC and
Kubernetes API types, including optional dependencies. Keep such projections
in the adapter layer as functions when neither type has a useful local owner.
An optional dependency still couples the implementation to a versioned
transport/schema API; it is an architecture decision, not a style workaround.
Use a standard conversion where one of the types is already local, such as
the metadata journal projection or the accelerator example's pair model.
Do not move domain types or invert foundational dependencies merely to satisfy
a style preference. Applications continue to select their Kubernetes schema.

Free functions fit algorithms, orchestration, contextual transport policy,
foreign-type operations, entry points and fixtures. A conversion requiring
pool-scoped validation, retry history or explicit driver context may be clearer
as a function or method with those parameters. Do not introduce single-use
wrapper types, extension traits or tuple-based conversions solely to eliminate
free functions. A test fixture helper should name a useful scenario rather
than duplicate an existing constructor.

## Review checklist

- Does the behavior have a responsible type? Is it already provided by an
  existing method, constructor, builder or standard trait?
- Does a conversion's ownership, fallibility and meaning fit `From`,
  `TryFrom` or `FromStr`, without a redundant parallel API?
- Is the implementation legal in its owning crate? Are the architecture's
  dependency boundaries intact, including core independence and consumer
  schema selection?
- Is retained state borrowed and consumed input moved? Are semantic/wire
  distinctions and any non-reversible projections documented?
- Are trust-boundary checks, error ordering, wire values/bytes, stable names,
  and async guard/cancellation lifetimes preserved?
- Do existing independent wire/malformed-input tests still prove the contract?
  Have changed feature combinations and both supported schemas been checked?

## Validation

Run `mise run ci` for formatting, strict Clippy, tests, tooling and independent
consumer checks. Direct Cargo commands use
`K8S_OPENAPI_ENABLED_VERSION=1.36` or `1.37` when OpenAPI is enabled; classic
and schema-free core/Quantity consumers must continue to work without it.
Run schema builds sequentially or use isolated target directories.

The independent core consumer check exercises default, minimal and all core
features without a Kubernetes schema and rejects transport/API dependencies.
Clippy's explicit `from_over_into`, `infallible_try_from`,
`should_implement_trait`, `wrong_self_convention` and `new_ret_no_self` rules
enforce mechanical conventions. They cannot determine architectural ownership;
the checklist is required. Generated and vendored code is excluded from
ownership refactoring. See the [ownership assessment](docs/plans/zw5-function-ownership.md)
for concrete decisions and retained-function examples.
