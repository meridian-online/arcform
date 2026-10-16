---
description: >-
  Release arcform: on a branch, bump the crate version and roll CHANGELOG.md's [Unreleased]
  section into a dated version; open a pull request and wait for CI; once Hugh has merged it and
  pushed the tag, watch the release workflow publish; then open the web-repo pull request for the
  release. Agents never push to main, never tag and never merge.
when_to_use: >-
  User says "release", "ship", "cut a release", "prepare a release", or "roll the changelog for
  release". A deliberate, reviewed action: never auto-fire mid-task or as a side effect of other
  work.
argument-hint: "[X.Y.Z | patch | minor]"
arguments: version
allowed-tools: Bash, Read, Edit, Write
---

# /release

A release of arcform is a pushed `vX.Y.Z` tag. `.github/workflows/release.yml` builds `arc` for four targets, publishes the GitHub release with the tag's `CHANGELOG.md` section as its notes, publishes `arcform-sqlparser` and `arcform` to crates.io, and sends `scripts/install.sh` to the install site. `docs/RELEASE.md` describes that machinery; read it first.

Your part is everything either side of the tag: the release pull request before it, and watching the run and the web pull request after it. **You never push to `main`, never create or push a tag, and never merge a pull request.** Hugh merges the release pull request and pushes the tag himself.

The notes come from `CHANGELOG.md` and nowhere else. Do not compile them from commit subjects (the history does not use Conventional Commits) and do not use GitHub's generated notes. Every pull request with a user-visible change already adds its entry under `[Unreleased]`; the release edits and rolls that section.

## Choosing the version

Pre-1.0, so:

- **patch** (`0.X.Y` → `0.X.Y+1`): fixes and documentation, with no new surface.
- **minor** (`0.X.0` → `0.X+1.0`): new commands, flags, manifest fields, MCP tools, or a change in behaviour.
- **major:** reserved. Never cut 1.0 without Hugh's explicit decision.

Use the argument if one was given. Otherwise read `[Unreleased]`, propose a version with a one-line reason, and wait for Hugh to confirm it.

## 1. On a branch, bump the version and roll the changelog

Start from an up-to-date `main` with a clean tree, on a new branch:

```bash
git switch main && git pull --ff-only
git status --porcelain            # must print nothing
git switch -c release-vX.Y.Z
```

**Roll the changelog.** In `CHANGELOG.md`:

1. Merge any duplicate subsection headings under `[Unreleased]` (one `### Added`, one `### Changed`, one `### Fixed`, one `### Removed`, in that order).
2. Add a short summary at the head of the section, about ten lines, written for someone who has never used arc: what this release lets them do. It sits above the first `###` heading.
3. Rename `## [Unreleased]` to `## [X.Y.Z] - YYYY-MM-DD` with today's date (`date +%F`), and put a fresh, empty `## [Unreleased]` above it.
4. Update the link definitions at the foot of the file. `[Unreleased]` now compares from the new tag, and the new version gets its own line above the previous one:

   ```markdown
   [Unreleased]: https://github.com/meridian-online/arcform/compare/vX.Y.Z...HEAD
   [X.Y.Z]: https://github.com/meridian-online/arcform/releases/tag/vX.Y.Z
   ```
5. Strip anything that names private planning material: this repository is public. `./scripts/check-public-hygiene.sh` must be clean.

Keep the section under 120,000 characters, or the tag guard refuses it (GitHub cuts a release body at 125,000).

**Bump the version** in four places:

1. `version` under `[package]` in `Cargo.toml`.
2. The `**Version:**` line in `CLAUDE.md`.
3. `Cargo.lock`'s `arcform` entry. Run `cargo metadata --format-version 1 > /dev/null` to rewrite it, then check with `cargo metadata --locked --format-version 1 > /dev/null`. The release builds with `--locked`, so a stale lock fails every build leg of the tag.
4. The library snippet `arcform = { version = "=X.Y.Z", default-features = false }` in `README.md`, `src/lib.rs` and `src/spec.rs`. It pins exactly, so a stale one keeps a reader of the published docs on the previous release.

Leave `vendor/sqlparser-0.55.0` alone unless the fork itself changed. If it did, its patch version moves in both its own `Cargo.toml` and arcform's `=` pin, and CI's `package` job fails if they differ.

## 2. Check locally, then open the pull request and wait for CI

```bash
python3 scripts/check_release_tag.py --tag vX.Y.Z        # the guard accepts this tree
python3 scripts/check_release_tag.py --release-rehearsal
python3 scripts/check_release_tag.py --wiring
cargo fmt --all --check
./scripts/check-public-hygiene.sh
```

Then commit the seven files by name and open the pull request:

```bash
git add CHANGELOG.md Cargo.toml Cargo.lock CLAUDE.md README.md src/lib.rs src/spec.rs
git commit -m "Release X.Y.Z: <one line on what it brings>"
git push -u origin release-vX.Y.Z
gh pr create --title "Release X.Y.Z" --body "<the summary from the changelog section, then: merging this and pushing vX.Y.Z publishes the release>"
gh pr checks --watch
```

A check still pending is pending, not green. If CI fails, fix it on the branch and wait again.

## 3. Hugh merges and pushes the tag

When every check is green, tell Hugh the release pull request is ready, with its link, and stop. He merges it and pushes the tag from the merged `main`:

```bash
git switch main && git pull --ff-only
git tag vX.Y.Z && git push origin vX.Y.Z
```

That block is his to run, not yours.

## 4. Watch the release workflow

Once Hugh says the tag is pushed:

```bash
gh run list --workflow release.yml --limit 3
gh run watch <run-id> --exit-status
```

The five jobs are `tag-guard`, `build` (four legs), `release`, `publish-crates` and `update-install-site`. When the run is green, check what it published:

```bash
gh release view vX.Y.Z --json assets,body,publishedAt   # eight assets; the body is the changelog section
cargo search arcform --limit 3                           # arcform = "X.Y.Z"
curl -fsSL https://install.meridian.online/arc | head -n 5
```

If a job fails, `docs/RELEASE.md` ("When a release fails partway") says what has and has not been published and how to recover. Report the failure to Hugh with the job's log; do not delete or move the tag yourself. Moving it is Hugh's step, and `docs/RELEASE.md` gives the commands.

## 5. Open the web-repository pull request

In `meridian-online/web`, on a new branch:

1. **`src/data/releases.json`**: add `{ "package": "arcform", "version": "X.Y.Z", "date": "YYYY-MM-DD" }`, dated to the release's publish date (`publishedAt` above, not the changelog heading's date), keeping the rows grouped by package and in version order.
2. **`content/changelog/arcform-vX-Y-Z.mdx`**: a short entry in the form of the existing ones (frontmatter `title`, `date`, `package: arcform`, `version`; a bold lead sentence on what shipped; a link to the arcform docs and to the GitHub release).
3. **The arcform docs page's install section** (`content/docs/projects/arcform/`): the install line `curl -fsSL https://install.meridian.online/arc | bash`, the `cargo install arcform --locked --features mcp,bundled-duckdb` alternative, and the `duckdb` prerequisite. Change only what this release makes untrue.

Open the pull request with `gh pr create`, wait for its checks, and tell Hugh it is ready with its link. Do not merge it.

## Report

```
Released arcform vX.Y.Z.

  Release PR:   <link> (merged by Hugh)
  Tag:          vX.Y.Z (pushed by Hugh)
  Workflow:     <run link>, all jobs green
  Release:      <release link>, 8 assets
  crates.io:    arcform X.Y.Z, arcform-sqlparser <version or "unchanged">
  Install site: install.meridian.online/arc updated
  Web PR:       <link>, checks green, ready for Hugh
```
