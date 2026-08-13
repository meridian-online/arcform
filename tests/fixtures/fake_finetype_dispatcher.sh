#!/bin/sh
# Static, checked-in stand-in for the `finetype` CLI, used by the `fake_finetype`
# test fixture in src/mcp/finetype.rs. This file itself is never written by the
# test process — only read off disk and exec'd — which is the whole point: see
# the comment on `fake_finetype` for why that removes an ETXTBSY race.
#
# Invoked (via a per-test symlink) as `<symlink-dir>/finetype [args...]`. A
# shebang script keeps `$0` as the path it was invoked through — the symlink's
# path, not this file's real location — so `dirname "$0"` finds the per-test
# temp directory and its sidecar `version` file placed there by the caller.
dir=$(dirname "$0")
version=$(cat "$dir/version")
if [ "$1" = "--version" ]; then
    echo "finetype $version"
    exit 0
fi
echo "ARGS: $@"
