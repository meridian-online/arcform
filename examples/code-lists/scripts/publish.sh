#!/usr/bin/env bash
#
# publish.sh — publish the open zone.
#
# Uploads the published artifacts (both Parquet, the frozen open.ducklake
# catalogue, and datapackage.json) to the R2 open zone. GUARDED: when R2
# credentials are absent it does NOT fail. Instead it stages the same artifact
# set into a local ./dist mirror of the open-zone layout and writes a publish
# receipt, then exits 0.
#
# Real upload requires: R2_ACCOUNT_ID, R2_ACCESS_KEY_ID, R2_SECRET_ACCESS_KEY,
# R2_BUCKET, and rclone on PATH. Any missing piece falls back to local staging.
set -eu

DIST="dist"
ZONE="$DIST/open-zone/code-lists"     # local mirror of the R2 open-zone key prefix
ARTIFACTS="naics.parquet icd10cm.parquet open.ducklake"

sha() { if command -v shasum >/dev/null 2>&1; then shasum -a 256 "$1" | awk '{print $1}';
        else sha256sum "$1" | awk '{print $1}'; fi; }

stage_local() {
    reason="$1"
    mkdir -p "$ZONE"
    for a in $ARTIFACTS; do
        [ -f "$DIST/$a" ] && cp -f "$DIST/$a" "$ZONE/$a"
    done
    cp -f datapackage.json "$ZONE/datapackage.json"

    receipt="$ZONE/_publish_receipt.json"
    {
        echo "{"
        echo "  \"zone\": \"open\","
        echo "  \"mode\": \"local-staging\","
        echo "  \"reason\": \"$reason\","
        echo "  \"target_prefix\": \"s3://<r2-open-bucket>/open/code-lists/\","
        echo "  \"staged_to\": \"$ZONE\","
        echo "  \"published_at\": \"$(date -u +%Y-%m-%dT%H:%M:%SZ)\","
        echo "  \"artifacts\": ["
        first=1
        for a in $ARTIFACTS datapackage.json; do
            src="$DIST/$a"; [ "$a" = "datapackage.json" ] && src="datapackage.json"
            [ -f "$src" ] || continue
            [ $first -eq 0 ] && echo ","
            first=0
            printf '    {"name": "%s", "bytes": %s, "sha256": "%s"}' "$a" "$(wc -c < "$src" | tr -d ' ')" "$(sha "$src")"
        done
        echo ""
        echo "  ]"
        echo "}"
    } > "$receipt"

    echo "publish: $reason — staged open zone locally at $ZONE/ (no failure). Receipt: $receipt"
}

# R2 upload path only when every credential + rclone is present; otherwise stage.
if [ -n "${R2_ACCOUNT_ID:-}" ] && [ -n "${R2_ACCESS_KEY_ID:-}" ] \
   && [ -n "${R2_SECRET_ACCESS_KEY:-}" ] && [ -n "${R2_BUCKET:-}" ]; then
    if command -v rclone >/dev/null 2>&1; then
        echo "publish: R2 credentials present — uploading open zone to bucket $R2_BUCKET ..."
        remote=":s3,provider=Cloudflare,access_key_id=$R2_ACCESS_KEY_ID,secret_access_key=$R2_SECRET_ACCESS_KEY,endpoint=https://$R2_ACCOUNT_ID.r2.cloudflarestorage.com:$R2_BUCKET/open/code-lists"
        if rclone copy "$DIST" "$remote" \
             --include 'naics.parquet' --include 'icd10cm.parquet' --include 'open.ducklake' \
           && rclone copyto datapackage.json "$remote/datapackage.json"; then
            echo "publish: uploaded open zone to R2 (bucket $R2_BUCKET, prefix open/code-lists/)."
            exit 0
        fi
        echo "publish: R2 upload failed — falling back to local staging." >&2
        stage_local "r2-upload-failed"
        exit 0
    fi
    stage_local "rclone-not-installed"
    exit 0
fi

# No R2 credentials configured — never fail, stage locally.
stage_local "r2-credentials-absent"
