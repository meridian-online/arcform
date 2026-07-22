---
description: >-
  Release arcform — roll the CHANGELOG, bump the crate version, tag, and publish a GitHub
  release. The changelog is the centre of gravity: each release moves `[Unreleased]` into a
  dated version section compiled from the conventional-commit git log. Network + git push, so
  run the pre-flight checks first and stop on any failure.
when_to_use: >-
  User says "release", "ship", "cut a release", "tag a version", or "update the changelog for
  release". A deliberate, reviewed action — never auto-fire mid-task or as a side effect of
  other work.
argument-hint: "[patch | minor | major | X.Y.Z]"
arguments: bump
allowed-tools: Bash, Read, Edit
---

# /release

Cut a release of the `arc` binary. arcform is a single crate (no workspace, no model, no
sub-packages), so a release is: **changelog → version bump → tag → GitHub release**. The
changelog is the deliverable users actually read — treat steps 1–3 as the heart of the skill
and don't shortcut them.

## Versioning policy

Pre-1.0, so the rules are looser than SemVer-proper but still deliberate:

- **patch** (`0.1.x`) — bug fixes, docs, internal changes with no new surface.
- **minor** (`0.x.0`) — new capabilities: a `feat:` commit, a new CLI command/flag, a new manifest
  field. This is the common case while the engine is growing. **Default to minor when a `feat:`
  commit landed since the last release.**
- **major** (`x.0.0`) — reserved. Don't cut a 1.0 without an explicit decision.

If the user passed an argument (`patch` / `minor` / `major` / an explicit `X.Y.Z`), use it.
Otherwise infer from what shipped since the last tag and state your choice in the pre-flight
summary.

## Usage

```
/release            # infer the bump from the commits since the last tag
/release minor      # force a minor bump
/release 0.2.0      # set an explicit version
```

## Instructions

### 1. Pre-flight checks

Run these and **stop on any failure**. A release is hard to unwind once tagged and pushed.

1. **On `main`, clean, and up to date.**
   ```bash
   git rev-parse --abbrev-ref HEAD     # must be main
   git status --porcelain              # must be empty
   git fetch -q origin && git rev-list --left-right --count main...origin/main  # must be 0  0
   ```
   If a release is being cut from feature work, that work must be merged to `main` first.

2. **Build is clean (zero warnings).**
   ```bash
   LIBRARY_PATH=/opt/homebrew/lib cargo build 2>&1 | grep -E "^warning|^error" | head
   ```
   `LIBRARY_PATH=/opt/homebrew/lib` is required on macOS — the `duckdb` crate links the Homebrew
   `libduckdb.dylib`, which isn't on the default linker path. Expect the two known dead-code
   warnings (`runner::run`, `MockStateBackend::set_step_state`) — anything else stops the release.

3. **Tests pass.**
   ```bash
   LIBRARY_PATH=/opt/homebrew/lib cargo test 2>&1 | tail -5
   ```
   Tests must link and pass here. If they cannot link on this machine, do not claim they passed —
   say so and let the user decide.

4. **`arc` runs.** Smoke-test the built binary:
   ```bash
   LIBRARY_PATH=/opt/homebrew/lib cargo run -q -- --version
   ```

Present the pre-flight summary (branch, cleanliness, warning count, test result, current version,
proposed new version) and **wait for confirmation** before proceeding.

### 2. Update CHANGELOG.md — the featured step

arcform follows [Keep a Changelog](https://keepachangelog.com/). The top of the file always has
an `[Unreleased]` section; a release converts it into a dated version heading and starts a fresh
`[Unreleased]`.

**Compile the entries** for everything since the last release from the git log. arcform follows
[Conventional Commits](https://www.conventionalcommits.org/), so the commit history is the single
source of truth — the skill needs no external substrate to read.

1. **List the commits since the last release** (subject lines only):
   ```bash
   git log $(git describe --tags --abbrev=0 2>/dev/null || git rev-list --max-parents=0 HEAD)..HEAD \
     --no-merges --pretty=format:'%s'
   ```
   No tags yet on a fresh repo → the `git rev-list --max-parents=0 HEAD` fallback walks from the
   root commit, so the first release covers the whole history.

2. **Sort each commit by its conventional-commit type** into Keep a Changelog categories:

   | Commit prefix                          | Changelog section        | Notes                                          |
   |----------------------------------------|--------------------------|------------------------------------------------|
   | `feat:`                                | **Added**                | New capability, CLI command/flag, or manifest field. |
   | `fix:`                                 | **Fixed**                | A bug fix the user can feel.                    |
   | `feat!:` / `BREAKING CHANGE:` footer   | **Changed** or **Removed** | Behaviour change or a removed/renamed surface. |
   | `refactor:` / `perf:` / `docs:` / `chore:` / `test:` / `build:` / `ci:` | *usually omit* | Internal — fold into one line only if user-visible. |

   Commits may cite a private planning id in parentheses (e.g. a `(card NNNN)` suffix). **This
   repo is public — never carry that citation into the changelog.** Strip it and describe the
   user-visible change in plain English; the fuller rationale lives in the private planning repo.

**Write for users, not for the commit log.** Each entry is a user-visible change — a new
command/flag, a manifest field, a behaviour change, a fix. Fold internal refactors into one line
or omit them. Lead with the capability; do not cite private card or decision ids.

Use these categories (omit any that are empty):

```markdown
## [0.X.0] - YYYY-MM-DD

### Added
- **Lifecycle hooks** — `on_init` / `on_success` / `on_failure` / `on_exit` handlers in the
  manifest.

### Changed
- ...

### Fixed
- ...

### Removed
- ...
```

Get today's date from `date +%F`. If a prior release left a placeholder, leave it — don't backfill
history you can't verify.

### 3. Bump the version

Single crate, so two files:

1. **`Cargo.toml`** — `version = "0.X.0"`.
2. **`CLAUDE.md`** — the `**Version:**` field on line ~5.

Then confirm it still compiles:
```bash
LIBRARY_PATH=/opt/homebrew/lib cargo check 2>&1 | tail -3
```
(`cargo` rewrites `Cargo.lock`'s `arc` entry on the next build — stage that too.)

### 4. Commit, tag, push

```bash
git add Cargo.toml Cargo.lock CLAUDE.md CHANGELOG.md
git commit -m "Release v0.X.0"
git tag v0.X.0
git push && git push --tags
```

### 5. Publish the GitHub release

arcform has **no release CI workflow yet** (see the note below), so publish directly with the
changelog section as the body:

```bash
# Extract the just-released section from CHANGELOG.md for the release body.
awk '/^## \[0\.X\.0\]/{f=1;next} /^## \[/{f=0} f' CHANGELOG.md > /tmp/relnotes.md
gh release create v0.X.0 --title "v0.X.0" --notes-file /tmp/relnotes.md
```

If you can produce a binary on this platform, attach it:
```bash
LIBRARY_PATH=/opt/homebrew/lib cargo build --release
gh release upload v0.X.0 target/release/arc
```
Name single-platform assets honestly (e.g. `arc-aarch64-apple-darwin`) so it's clear it isn't a
cross-platform build.

### 6. Verify & report

```bash
gh release view v0.X.0
```

Report:
```
Released arc v0.X.0.

  Changelog:  CHANGELOG.md → [0.X.0]
  Tag:        v0.X.0 (pushed)
  Release:    <github url>
  Assets:     <none | arc-aarch64-apple-darwin>
```

## Future: cross-platform release CI

This skill publishes from the local machine. To produce Linux/macOS/Windows binaries and a
Homebrew tap update on every `v*` tag, add `.github/workflows/release.yml` — finetype's
(`meridian-online/finetype/.github/workflows/release.yml`) is the reference: it builds the matrix,
creates the release with auto-notes, and dispatches the tap + install-script updates. Until that
exists, keep this skill's step 5 (direct `gh release create`) and flag the single-platform asset.

## Rollback

A tag/release is reversible only before others pull it:

1. **Not yet pulled** — delete the release and tag, fix, re-cut:
   ```bash
   gh release delete v0.X.0 --yes
   git push --delete origin v0.X.0 && git tag -d v0.X.0
   ```
2. **Already public** — don't rewrite history. Cut a new patch (`0.X.1`) with the fix and a
   changelog entry noting the correction.
