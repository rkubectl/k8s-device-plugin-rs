# Releasing

This workspace uses [`cargo-release`](https://github.com/crate-ci/cargo-release)
for its version transaction. All public crates share one workspace version and
are released under one annotated `vX.Y.Z` tag.

The standalone `k8s-quantity` crate participates in that shared version and
must be published before `k8s-device-plugin-dra-core`, which depends on it.
Classic core is independent of Quantity. The DRA core crate must precede the
DRA runtime and facade.
`cargo-release` derives that order from the dependency graph. The v0.5.0
Quantity API migration replaces the public integer tuple field and `Copy`;
use a minor release for such breaking changes instead of using the default
patch preview as the release decision.

The release source is always tagged and pushed before publication. That makes
the tag, rather than a mutable working tree, the auditable source for every
crate on crates.io.

## Prepare a release

Start from `main` with an empty `git status`; `cargo-release` intentionally
refuses both tracked and untracked changes. Keep private review notes outside
the checkout, or use a clean worktree for the release.

Select the maintainer's Kubernetes schema for direct Cargo commands, including
package verification during publication. This environment setting is not a
schema feature in the published library dependencies:

```bash
export K8S_OPENAPI_ENABLED_VERSION=1.37
```

The DRA, test-helper and facade manifests also select 1.37 through docs.rs
`cargo-args` for hosted documentation builds. Keep that metadata separate from
normal dependency features so consumers can still choose their own schema.

Run the required CI gate and inspect the proposed next patch release:

```bash
mise run release-preflight
```

For a particular version rather than the next patch, substitute that version
in the preview command:

```bash
cargo release 0.1.0 --workspace --exclude k8s-device-plugin-example --no-confirm
```

`cargo-release` is dry-run by default. Verify that the preview changes the
shared version, all internal dependency constraints, the two facade-crate
README snippets, and the two getting-started dependency examples.

## Tag and push the release sources

After the preview is correct, repeat the release with `--execute`:

```bash
cargo release 0.1.0 --workspace --exclude k8s-device-plugin-example --execute --no-confirm
```

The workspace configuration permits releases only from `main`, makes one
release commit, creates annotated tag `v0.1.0`, and pushes the commit and tag
to `origin`. It deliberately disables the tool's automatic publish stage.

Confirm the checked-out source is exactly the pushed tag before publishing:

```bash
git diff --quiet
git describe --exact-match --tags HEAD
```

## Publish from the tag

Publish only after the tag check succeeds:

```bash
cargo release publish --workspace --exclude k8s-device-plugin-example \
    --config release-publish.toml --execute --no-confirm
```

The publication-only config explicitly sets `publish = true`. Without that
override, the workspace's `publish = false` also disables the separate publish
step, which can exit successfully without uploading any crates. Keep the
override limited to this step so the source tag is pushed before publication.
For an older tag that predates `release-publish.toml`, pass an absolute path to
that config in the maintenance checkout; do not modify the tagged sources.

`cargo-release` processes workspace packages in dependency order and skips a
version that crates.io already has, so the command can safely resume an
interrupted publication. Do not make release-source changes after tagging; a
fix requires a new version and tag.

Finally update the corresponding Beads issue, export it, and commit that audit
record separately:

```bash
brr close <issue-id> --reason "Released v0.1.0"
brr sync --flush-only
git add .beads/issues.jsonl
git commit -m "Record v0.1.0 publication"
git push
```
