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
