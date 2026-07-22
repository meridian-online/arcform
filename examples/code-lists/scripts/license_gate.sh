#!/usr/bin/env bash
#
# license_gate.sh — GREEN-list license gate (Pre-SQL).
#
# Reads each source's SPDX id + redistribution flag from datapackage.json and
# clears the pipeline ONLY if every source is proven GREEN. The policy is
# default-RED: anything not on the green-list — a missing/unknown SPDX id, or a
# redistribution flag that isn't explicitly allowed — blocks the run (exit 1),
# so transform and publish never touch un-cleared data.
#
# It also prepares the local open zone: creates ./dist and removes any prior
# frozen catalogue so the catalog step rebuilds a single clean snapshot.
#
# Usage: license_gate.sh [datapackage.json]   (bash 3.2 compatible)
set -eu

DP="${1:-datapackage.json}"

command -v jq >/dev/null 2>&1 || { echo "license-gate: jq is required" >&2; exit 2; }
[ -f "$DP" ] || { echo "license-gate: $DP not found" >&2; exit 2; }

# Prepare the local open-zone output dir + reset the frozen catalogue.
mkdir -p dist
rm -f dist/open.ducklake dist/open.ducklake.wal

# Green-list policy from the descriptor (newline-delimited allow-lists).
GREEN_SPDX=$(jq -r '.["x-meridian-license-gate"].green.spdx[]?'           "$DP")
GREEN_REDIST=$(jq -r '.["x-meridian-license-gate"].green.redistribution[]?' "$DP")

# exact, whole-line, fixed-string membership test
in_list() { printf '%s\n' "$2" | grep -qxF -- "$1"; }

n=$(jq '.sources | length' "$DP")
fail=0
echo "license-gate: policy=green-list default=red  (${n} source(s))"

i=0
while [ "$i" -lt "$n" ]; do
    title=$(jq  -r ".sources[$i].title // \"source $i\""        "$DP")
    spdx=$(jq   -r ".sources[$i][\"x-spdx-license\"]  // \"\""   "$DP")
    redist=$(jq -r ".sources[$i][\"x-redistribution\"] // \"\""  "$DP")

    status="RED"   # default RED — prove GREEN or block
    if [ -n "$spdx" ] && [ -n "$redist" ] \
        && in_list "$spdx" "$GREEN_SPDX" \
        && in_list "$redist" "$GREEN_REDIST"; then
        status="GREEN"
    fi

    printf '  [%-5s] %s  (spdx=%s redistribution=%s)\n' "$status" "$title" "${spdx:-<none>}" "${redist:-<none>}"
    [ "$status" = "GREEN" ] || fail=1
    i=$((i + 1))
done

if [ "$fail" -ne 0 ]; then
    echo "license-gate: BLOCKED — a source is not proven GREEN (default RED). Not clearing to transform/publish." >&2
    exit 1
fi

echo "license-gate: all sources proven GREEN — cleared to transform + publish."
