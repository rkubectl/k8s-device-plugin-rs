# Releasing

This workspace uses [`cargo-release`](https://github.com/crate-ci/cargo-release)
for its version transaction. All public crates share one workspace version and
are released under one annotated `vX.Y.Z` tag.

The release source is always tagged and pushed before publication. That makes
the tag, rather than a mutable working tree, the auditable source for every
crate on crates.io.

## Prepare a release

Start from `main` with an empty `git status`; `cargo-release` intentionally
refuses both tracked and untracked changes. Keep private review notes outside
the checkout, or use a clean worktree for the release.

Run the required CI gate and inspect the proposed next patch release:

```bash
mise run release-preflight
```

For a particular version rather than the next patch, substitute that version
in the preview command:

```bash
cargo release 0.0.7 --workspace --exclude k8s-device-plugin-example --no-confirm
```

`cargo-release` is dry-run by default. Verify that the preview changes the
shared version, all internal dependency constraints, and the two facade-crate
README snippets.

## Tag and push the release sources

After the preview is correct, repeat the release with `--execute`:

```bash
cargo release 0.0.7 --workspace --exclude k8s-device-plugin-example --execute --no-confirm
```

The workspace configuration permits releases only from `main`, makes one
release commit, creates annotated tag `v0.0.7`, and pushes the commit and tag
to `origin`. It deliberately disables the tool's automatic publish stage.

Confirm the checked-out source is exactly the pushed tag before publishing:

```bash
git diff --quiet
git describe --exact-match --tags HEAD
```

## Publish from the tag

Publish only after the tag check succeeds:

```bash
cargo release publish --workspace --exclude k8s-device-plugin-example --execute --no-confirm
```

`cargo-release` processes workspace packages in dependency order and skips a
version that crates.io already has, so the command can safely resume an
interrupted publication. Do not make release-source changes after tagging; a
fix requires a new version and tag.

Finally update the corresponding Beads issue, export it, and commit that audit
record separately:

```bash
brr close <issue-id> --reason "Released v0.0.7"
brr sync --flush-only
git add .beads/issues.jsonl
git commit -m "Record v0.0.7 publication"
git push
```
