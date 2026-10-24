<!-- br-agent-instructions-v1 -->

---

## Beads Workflow Integration

This project uses [beads_rust](https://github.com/Dicklesworthstone/beads_rust)
(`brr`) for issue tracking. The shared issue export is
`.beads/issues.jsonl`; it is tracked in git and must be synchronized and
committed with the work it describes.

### Essential Commands

```bash
# View ready issues (open, unblocked, not deferred)
brr ready

# List and search
brr list --status open # All open issues
brr show <id>          # Full issue details with dependencies
brr search "keyword"   # Full-text search

# Create and update
brr create --title="..." --description="..." --type task --priority P2
brr update <id> --status in_progress
brr close <id> --reason "Completed"
brr close <id1> <id2>  # Close multiple issues at once

# Sync with git
brr sync --flush-only  # Export DB to JSONL
brr sync --status      # Check sync status
```

### Workflow Pattern

1. **Start**: Run `brr ready` to find actionable work
2. **Claim**: Use `brr update <id> --status in_progress`
3. **Work**: Implement the task
4. **Complete**: Use `brr close <id>`
5. **Sync**: Always run `brr sync --flush-only` at session end

### Key Concepts

- **Dependencies**: Issues can block other issues. `brr ready` shows only open, unblocked work.
- **Priority**: P0=critical, P1=high, P2=medium, P3=low, P4=backlog (use numbers 0-4, not words)
- **Types**: task, bug, feature, epic, chore, docs, question
- **Blocking**: `brr dep add <issue> <depends-on>` to add dependencies

### Session Protocol

**Before ending any session, run this checklist:**

```bash
brr sync --flush-only   # Export the shared issue state to JSONL
git status              # Check code and .beads/issues.jsonl changes
git add <files> .beads/issues.jsonl
git commit -m "..."
git push
```

### Best Practices

- Check `brr ready` at session start to find available work
- Update status as you work (`in_progress` → `closed`)
- Create new issues with `brr create` when you discover tasks
- Use descriptive titles and set appropriate priority/type
- Always run `brr sync --flush-only` before committing issue-state changes

---

## DRA Runtime Documentation

Before changing the DRA runtime or its deployment artifacts, read
[`dra/README.md`](dra/README.md) as the concise integration contract and
[`docs/dra-design.md`](docs/dra-design.md) for scope, validation evidence, and
the future phases.

- Phase 1 supports the stable `resource.k8s.io/v1` API, one `ResourceSlice`
  per pool, pluginwatcher registration, and claim preparation/unpreparation.
- The validation manifests are intentionally not production RBAC guidance.
  Preserve `maxSurge: 0` unless the target kubelet's seamless-upgrade support
  has been explicitly verified.
- The root Cargo manifest pins a temporary `h2` compatibility fork for real
  kubelet registration. It applies only to this workspace; downstream
  applications must repeat the root `[patch.crates-io]` entry until the
  documented upstream cleanup condition is met.
- Use the DRA guide's validation checklist, including the kind smoke test,
  after changing kubelet-facing behavior.

<!-- end-br-agent-instructions -->

## Rust cleanup conventions

- Keep defaults and invariants on the responsible type. Reuse existing
  constructors and builders; use lossless `From`/`Into` conversions for ordinary
  values. Keep semantically distinct values explicit, such as
  `AttributeValue::version("1.2.3")` versus a plain string attribute. Add helpers
  for repeated behavior or type semantics, rather than hiding useful literals.
- Check the locked dependency's constructors and builders before writing a
  `kube` object literal. Prefer matching helpers such as
  `api::DeleteParams::default().preconditions(...)`,
  `api::ListParams::default().fields(...)`, and `api::PatchParams::apply(...)`.
  Keep literals where no matching helper exists. Choose constructors for their
  behavior: deletion propagation helpers set a policy and are not equivalent
  to an unspecified policy.
- Use one import per item. Separate standard-library, third-party, workspace,
  and local imports. Prefer module-qualified generic names such as `json::Value`,
  `fs::File`, and `task::JoinHandle`, with `use serde_json as json`. Reuse an
  existing parent scope via `use super::*` in child modules where that is the
  established pattern; avoid redundant imports.
- Keep type hints next to the operation that needs them when clear, for example
  `.collect::<BTreeMap<_, _>>()` and `json::from_slice::<json::Value>(bytes)`.
  Retain annotations that clarify complex types or trait-object boundaries.
- When a transformation owns an input that is no longer needed, consume it:
  use `.into_iter()` and move fields rather than borrowing and cloning them.
  Let inference supply iterator item types, such as `.collect::<Vec<_>>()`.
  Extend this ownership through private conversion helpers and their callers;
  keep borrowed public APIs, shared snapshots, retry state, and independently
  owned outputs intact. Do not add clones just to fit a borrowed helper or
  force moves that complicate an otherwise clear borrow. Clippy's
  `redundant_clone` rule catches mechanically detectable cases.
- Prefer standard-library helpers and direct expressions over manual glue.
  Preserve behavior, wire values, coordination filenames, and the declared
  minimum Rust version when replacing an implementation.
- Let tracing record native scalar and string values. Use explicit Display
  formatting for errors, path displays, or types that need it.
- Expand complex JSON objects for readability. Keep raw expected wire values
  and explicit enum variants in tests when they independently verify semantics.
- Prefer existing `k8s_openapi`, `kube`, or workspace types for complete objects
  and ordinary fixtures. Serialize typed objects at the transport boundary;
  do not build a JSON tree just to deserialize it into a known type.
  Explicit partial merge patches are a good use of `json::json!`: preserve
  absent fields instead of serializing a full resource with defaults or nulls.
  Raw JSON also fits open extension payloads, independent wire assertions, and
  deliberate malformed/defaulting tests. Document other schema gaps rather
  than substituting an unrelated Kubernetes type.

## Synchronization primitives

- Prefer `parking_lot` over equivalent `std::sync` locks (`Mutex`, `RwLock`,
  `Condvar`, and `Once`) for synchronous coordination, including examples and
  tests. Clippy's `disallowed_types` rule enforces this preference.
- Use `tokio::sync` when tasks must await acquisition, a guard must span async
  work, or asynchronous channels, notifications, or initialization fit the
  lifecycle. An async function alone is not a reason to use an async mutex:
  keep short data-only critical sections synchronous and release their guards
  before `.await`. Avoid blocking runtime threads on long waits or I/O.
- The metadata registry uses `parking_lot::Mutex` for short lookups; its
  per-claim Tokio owned guards serialize async operations and remain owned by
  blocking filesystem work through RPC cancellation. Preserve that boundary.
- Keep published async mock lock APIs compatible. `std::sync::Arc`, `Weak`,
  atomics, and primitives without a `parking_lot` equivalent remain valid.
  Filesystem locks provide cross-process exclusion and cannot be replaced by
  in-process mutexes. Any necessary exception to the lint needs a local,
  documented reason.
