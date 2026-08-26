#!/bin/sh
# Static, checked-in stand-in for the binary a `tool:` precondition names, used by the
# `write_tool` test fixture in src/precondition.rs. This file itself is never written
# by the test process — only read off disk (from the git checkout, before the test
# binary or its thread pool are even loaded) and exec'd — which is what removes the
# write-then-exec race documented on `write_tool`. Same mechanism as
# tests/fixtures/fake_finetype_dispatcher.sh, for the same reason; see that file's
# comment for the ETXTBSY mechanics this sidesteps.
#
# Invoked (via a per-call symlink) as `<symlink-path>`, with no arguments — the
# `tool:` precondition runs `sh -c "$ARC_TOOL"`, and `$ARC_TOOL` resolves to the
# symlink. A shebang script keeps `$0` as the path it was invoked through — the
# symlink's path, not this file's real location — so `$0.script` finds the sidecar
# `write_tool` placed next to the symlink. `sh` reads that sidecar and runs it as a
# script: a read() of a file this process wrote, never an execve() of it, so a
# concurrent write to the sidecar (which `write_tool` never marks executable in the
# first place) can never race an exec of it.
exec sh "$0.script"
