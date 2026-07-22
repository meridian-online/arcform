#!/usr/bin/env bash
#
# finetype_gate.sh — column quality gate.
#
# Runs the finetype CLI to validate the normalised output columns against the
# published JSON Schema contract (schema/*.schema.json). finetype exits non-zero
# when any row's columns violate the contract, which fails this step and blocks
# the catalogue + publish. This is the data-quality counterpart to the license
# gate: the license gate proves we MAY publish; finetype proves the columns are
# fit to publish.
set -eu

command -v finetype >/dev/null 2>&1 || { echo "finetype-gate: finetype CLI is required" >&2; exit 2; }

validate() {
    file="$1"; schema="$2"; label="$3"
    [ -f "$file" ] || { echo "finetype-gate: missing $file" >&2; exit 2; }
    echo "finetype-gate: validating ${label} columns  ($file)"
    finetype validate "$file" "$schema"
}

validate dist/naics.parquet   schema/naics.schema.json   "NAICS"
validate dist/icd10cm.parquet schema/icd10cm.schema.json "ICD-10-CM"

echo "finetype-gate: all output columns conform to the published contract."
