#!/bin/sh
# make-forge.sh PROTOCOLS FORGE: for each PROTOCOLS/<name>/, commit its numbered
# versions (1/, 2/, ... up to 9/) in order to a repository, publish it bare as
# FORGE/test/<name>.git, and write each name's commits, oldest first, to
# FORGE/commits.json.
set -eu
protocols=$1
forge=$2
export GIT_AUTHOR_NAME=stub GIT_AUTHOR_EMAIL=stub@example.invalid
export GIT_COMMITTER_NAME=stub GIT_COMMITTER_EMAIL=stub@example.invalid
mkdir -p "$forge/test"
json="{"
sep=""
for dir in "$protocols"/*/; do
  name=$(basename "$dir")
  work=$(mktemp -d)
  git init --quiet --initial-branch=main "$work"
  shas=""
  # Versions are single digits, so the glob's order is the numeric order.
  for version_dir in "$dir"*/; do
    find "$work" -mindepth 1 -maxdepth 1 ! -name .git -exec rm -rf {} +
    cp -R "$version_dir." "$work/"
    git -C "$work" add --all
    git -C "$work" commit --quiet -m "version $(basename "$version_dir")"
    shas="$shas${shas:+,}\"$(git -C "$work" rev-parse HEAD)\""
  done
  git clone --quiet --bare "$work" "$forge/test/$name.git"
  rm -rf "$work"
  json="$json$sep\"$name\":[$shas]"
  sep=","
done
printf '%s}\n' "$json" >"$forge/commits.json"
cat "$forge/commits.json"
