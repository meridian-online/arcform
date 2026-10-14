# Releasing arcform

A release is a pushed version tag. `.github/workflows/release.yml` does the rest: it builds `arc` for four targets, publishes a GitHub release whose notes are the tag's section of `CHANGELOG.md`, publishes the crates to crates.io and sends the install script to the install site. Nothing is published from a laptop.

The agent-facing walkthrough is the release skill, `.claude/skills/release/SKILL.md`. This page says what the machinery does and what it requires.

## Before the first release: two repository secrets

Hugh adds these under the arcform repository's Settings → Secrets and variables → Actions. An agent cannot.

- **`INSTALL_SITE_TOKEN`**: a GitHub token that can send a repository dispatch to `meridian-online/install` (a fine-grained token with Contents read and write on that repository, the same kind finetype uses). The `update-install-site` job sends `scripts/install.sh` with it. The install repository must also list arcform in its `update.yml` matrix and serve `/arc`, or the dispatch is accepted and ignored.
- **`CARGO_REGISTRY_TOKEN`**: a crates.io API token with the `publish-new` and `publish-update` scopes. The `publish-crates` job publishes `arcform-sqlparser` and `arcform` with it. Without it the job fails with a message naming the secret, after the GitHub release is already out; add it and re-run that job.

## What a release tag must be

A pushed tag starts a release only when it is `v` followed by three dot-separated numbers, such as `v0.2.0`. `release.yml` filters its tag trigger on `v[0-9]+.[0-9]+.[0-9]+`, so a tag such as `v0.3.0-rc1` or `v19-train-state` starts nothing.

A version tag then has to pass `release.yml`'s first job, `tag-guard`, before anything else runs. Every other job waits on it, directly or through another job, none carries an `if:` that would run it after a failure, and `publish-crates` and `update-install-site` also wait on the `release` job, so a refusal stops the run before a binary is built, a release is created or a crate is published. `scripts/check_release_tag.py` refuses when:

- the tag is not a version (exit 3), which is the second line of defence behind the trigger filter;
- the tag's number differs from `version` under `[package]` in `Cargo.toml` (exit 4);
- `CHANGELOG.md` has no `## [X.Y.Z]` heading for that number (exit 5);
- the heading has nothing under it, or its section is longer than 120,000 characters (exit 6). GitHub cuts a release body at 125,000 characters, so a longer section would be published cut off mid-sentence; shorten it instead.

The release body is that heading's section of `CHANGELOG.md`, passed from `tag-guard` to the `release` job as a job output. GitHub's generated notes are off. So the changelog section is what readers of the release page see: write it before tagging, not after.

Pull requests check this without a tag. `ci.yml`'s `release-guard` job runs `check_release_tag.py --wiring`, which reads the tag filter, the guard step, the job graph and the release body out of `release.yml`, and `--release-rehearsal`, which executes the `tag-guard` job's own `run:` line against a copy of the tree with this tree's version and with each refusal. Both run on every pull request, so a version bump without its changelog heading fails before it can be tagged. `--self-test` mutates each of those and requires the check to refuse. What no check here can run is GitHub's own tag matching: `--wiring` reads the filter's text, and the guard's exit 3 sits behind it.

The script reads `release.yml` without a YAML library, so it understands only the block style the file is written in (jobs at two spaces, job keys at four, `needs:` written inline, steps as `- ` items with keys at eight). A shape it does not recognise fails with exit 2 rather than being guessed at; rewrite the line in the file's style.

## What the workflow publishes

| Job | Needs | What it does |
| --- | --- | --- |
| `tag-guard` | | Runs the check above and outputs `notes`. |
| `build` | `tag-guard` | Builds `cargo build --release --locked --features mcp,bundled-duckdb` for `aarch64-apple-darwin` and `x86_64-apple-darwin` (on `macos-latest`), `x86_64-unknown-linux-gnu` (`ubuntu-22.04`) and `aarch64-unknown-linux-gnu` (`ubuntu-22.04-arm`), all native. Checks that `arc --version` prints `arc X.Y.Z` and that `arc mcp` answers an `initialize` with protocol version `2025-11-25`. Packages `arc-<tag>-<target>.tar.gz`, with `arc` at the archive's root, and `arc-<tag>-<target>.tar.gz.sha256` in `hash  filename` form. |
| `release` | `tag-guard`, `build` | Requires all eight files and checks each checksum, then creates the GitHub release with `body: ${{ needs.tag-guard.outputs.notes }}` and `fail_on_unmatched_files: true`. |
| `publish-crates` | `release` | Publishes `arcform-sqlparser` (from `vendor/sqlparser-0.55.0`) when crates.io's API answers 404 for its version, then `arcform` (`cargo publish --locked`) when its version is not there either. Any answer other than 200 or 404 stops the job. |
| `update-install-site` | `release` | Sends `scripts/install.sh` to `meridian-online/install` as an `update-install-script` dispatch carrying the tag and repository. |

The Linux binaries are built on Ubuntu 22.04, so they need glibc 2.35 or later. Older systems build from source with `cargo install arcform --locked --features mcp,bundled-duckdb`.

## The release, step by step

1. **On a branch**, bump `version` in `Cargo.toml` (and the `**Version:**` line in `CLAUDE.md`), then refresh `Cargo.lock` with `cargo metadata --format-version 1 > /dev/null` and check it with `cargo metadata --locked --format-version 1 > /dev/null`: every build leg uses `--locked`, so a stale lock fails the tag. Move `CHANGELOG.md`'s `[Unreleased]` items under `## [X.Y.Z] - YYYY-MM-DD`, leaving an empty `[Unreleased]` above it, and update the link definitions at the foot of the file: `[Unreleased]: https://github.com/meridian-online/arcform/compare/vX.Y.Z...HEAD`, and a new `[X.Y.Z]: https://github.com/meridian-online/arcform/releases/tag/vX.Y.Z` above the previous version's. Run `python3 scripts/check_release_tag.py --tag vX.Y.Z` to see the guard accept it.
2. **Open a pull request** and wait for CI. `release-guard` rehearses the tag against the bumped tree; the `bundled` and `package` jobs build the release binary and package both crates.
3. **Hugh merges it and pushes the tag** from `main`: `git tag vX.Y.Z && git push origin vX.Y.Z`.
4. **Watch the release workflow** (`gh run watch`) until all five jobs are green, then check the release page lists eight assets.
5. **Open a pull request in the web repository** adding the `releases.json` row dated to the release's publish date, `content/changelog/arcform-vX-Y-Z.mdx`, and any change to the docs page's install section.

## When a release fails partway

- **`tag-guard` refused:** nothing was published. Fix the tree on a branch and merge it, then Hugh moves the tag to the fixed `main` (below).
- **A `build` leg failed:** nothing was published, because `release` needs every leg. Re-run the failed jobs if the failure was the runner's; otherwise fix and merge, then Hugh moves the tag.
- **`publish-crates` or `update-install-site` failed:** the GitHub release is out. Fix the cause (usually a missing secret) and re-run the failed job. `publish-crates` skips a crate whose version is already on crates.io, so a re-run publishes only what is missing. A crate version, once published, cannot be replaced; a fix to it is a new patch release.

Moving the tag is Hugh's step, as pushing it was. Delete it on GitHub and locally, then tag the fixed `main` and push again:

```bash
git push --delete origin vX.Y.Z && git tag -d vX.Y.Z
git switch main && git pull --ff-only
git tag vX.Y.Z && git push origin vX.Y.Z
```

Move a tag only while nothing has been published from it. Once the GitHub release or a crate is out, fix forward with a new patch release.
