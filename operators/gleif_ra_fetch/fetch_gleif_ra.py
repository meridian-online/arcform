# /// script
# requires-python = ">=3.10"
# dependencies = ["dlt>=1,<2"]
# ///
"""Fetch GLEIF entities registered under a given registration authority (RA).

This is the dlt-backed replacement for the retired `scripts/fetch_gleif_ra.sh`. It
pages the public GLEIF v1 API (`api.gleif.org/api/v1/lei-records`) filtered to one
registration authority and emits a CSV the `load` step's `load.sql` consumes:

    lei,registered_as,category

  registered_as = the SEC identifier the entity registered under — a numeric CIK
    for operating companies, or an S…/C… series/class ID for funds.
  category      = GLEIF's authoritative entity type (FUND | GENERAL | BRANCH | …).

Why cursor paging (not offset). GLEIF hard-caps OFFSET paging (`page[number]`) at
10,000 records — RA000665 has ~27k, so `page[number]` 400s the moment it crosses
record 10,000. We use CURSOR paging (`page[cursor]=*`, then follow `links.next`),
which has no offset cap. dlt's `RESTClient` + `JSONLinkPaginator(next_url_path=
"links.next")` follows the body's next-URL until it is null — the exact termination
the old bash loop hand-rolled with `.links.next // ""`. STATELESS full-refresh: no
incremental cursor is persisted between runs; every run re-fetches the whole RA.

Determinism. The GLEIF page order is not stable, so we collect every record, SORT
by (lei, registered_as, category), and write once — a deterministic byte layout that
makes the DuckDB `EXCEPT`-both-ways parity gate against the retired script's output a
pure row-set check. Fields are rendered exactly as the old `jq -r … | @csv` did
(strings double-quoted with internal quotes doubled, null → an empty unquoted field)
so the two CSVs parse to identical values under `read_csv(all_varchar=true)`.

Atomic write. We build into `<out>.part` and rename only on a verified-complete
fetch — a failed or partial fetch must leave NO output file, otherwise arc's
`modified_after` precondition mistakes a stale partial for "fresh" and silently
builds on truncated data (the 10k-offset failure this step replaced did exactly
that). Completeness is enforced against `meta.pagination.total`: we refuse a fetch
materially short of the advertised count (allowing ~one page of mid-run drift).

Run standalone (production — full RA):
    uv run operators/gleif_ra_fetch/fetch_gleif_ra.py --ra RA000665 --out out.csv
Smoke test (a tiny 2-page pull that skips the completeness guard):
    uv run operators/gleif_ra_fetch/fetch_gleif_ra.py \
        --ra RA000665 --out /tmp/smoke.csv --page-size 5 --max-pages 2
"""
from __future__ import annotations

import argparse
import os
import sys
import time

API_BASE = "https://api.gleif.org/api/v1"
RESOURCE = "lei-records"
DEFAULT_UA = "Meridian Protocol (open-analytics; research@meridian.online)"
# Politeness delay between pages — mirrors the retired script's `sleep 0.3`.
PAGE_DELAY_S = 0.3


def _csv_field(value: object) -> str:
    """Render one field exactly as jq's `@csv` did in the retired script.

    strings → double-quoted with internal `"` doubled; None → an empty *unquoted*
    field (jq renders JSON null as bare empty); numbers/bools → bare. lei /
    registeredAs / category are always JSON strings or null in practice, but the
    general rendering keeps parity if GLEIF ever returns another scalar type.
    """
    if value is None:
        return ""
    if isinstance(value, bool):
        return "true" if value else "false"
    if isinstance(value, (int, float)):
        return repr(value)
    return '"' + str(value).replace('"', '""') + '"'


def _total_records(ra: str, ua: str) -> int:
    """Advertised record count for the RA (meta.pagination.total), for the guard."""
    import requests  # transitive dep of dlt — always present under this script

    resp = requests.get(
        f"{API_BASE}/{RESOURCE}",
        params={"filter[entity.registeredAt]": ra, "page[size]": 1},
        headers={"User-Agent": ua},
        timeout=60,
    )
    resp.raise_for_status()
    return int(resp.json()["meta"]["pagination"]["total"])


def fetch(ra: str, page_size: int, ua: str, max_pages: int) -> list[tuple]:
    """Page the RA via cursor paging, returning every (lei, registered_as, category)."""
    from dlt.sources.helpers.rest_client import RESTClient
    from dlt.sources.helpers.rest_client.paginators import JSONLinkPaginator

    client = RESTClient(
        base_url=API_BASE,
        headers={"User-Agent": ua},
        # Follow the next-page URL embedded in the response body at links.next; the
        # paginator clears our query params once it switches to that URL (the cursor
        # + filter are already baked into links.next). Terminates on a null links.next.
        paginator=JSONLinkPaginator(next_url_path="links.next"),
        # Records live under the JSON:API top-level `data` array — pin it rather than
        # rely on auto-detection so the extraction is deterministic.
        data_selector="data",
    )

    rows: list[tuple] = []
    pages = 0
    for page in client.paginate(
        RESOURCE,
        params={
            "filter[entity.registeredAt]": ra,
            "page[size]": page_size,
            "page[cursor]": "*",
        },
    ):
        # Empty data page — stop (a defensive guard against a non-null links.next that
        # never drains, matching the retired loop's `[ "$n" -eq 0 ] && break`).
        if len(page) == 0:
            break
        for rec in page:
            attrs = rec.get("attributes", {}) or {}
            entity = attrs.get("entity", {}) or {}
            rows.append((attrs.get("lei"), entity.get("registeredAs"), entity.get("category")))
        pages += 1
        if max_pages and pages >= max_pages:
            break
        time.sleep(PAGE_DELAY_S)
    return rows


def main() -> None:
    ap = argparse.ArgumentParser(
        description="Fetch GLEIF RA registrations to a deterministic sorted CSV.",
    )
    ap.add_argument("--ra", required=True, help="registration authority id, e.g. RA000665")
    ap.add_argument("--out", required=True, help="output CSV path (written atomically)")
    ap.add_argument("--page-size", type=int, default=200, help="GLEIF page[size] (default 200)")
    ap.add_argument("--user-agent", default=DEFAULT_UA, help="User-Agent request header")
    ap.add_argument(
        "--max-pages",
        type=int,
        default=0,
        help="cap pages fetched (0 = unlimited); a smoke-test lever that also skips the "
        "completeness guard. Production leaves this unset.",
    )
    args = ap.parse_args()

    smoke = args.max_pages > 0

    total = _total_records(args.ra, args.user_agent)
    print(f"GLEIF {args.ra}: {total} records (cursor paging)", file=sys.stderr)

    rows = fetch(args.ra, args.page_size, args.user_agent, args.max_pages)

    # Refuse a materially short fetch (allow ~one page of drift if GLEIF's count shifts
    # mid-run). A partial fetch must NOT be written — see the atomic-write note above.
    # Skipped when --max-pages caps the pull (smoke test).
    if not smoke and len(rows) < total - args.page_size:
        sys.exit(
            f"error: fetched only {len(rows)} of {total} records for {args.ra} — "
            "refusing to write partial output"
        )

    # Deterministic order: sort by the three fields (None sorts as empty string so a
    # null field never raises comparing None to str).
    rows.sort(key=lambda r: tuple("" if v is None else str(v) for v in r))

    lines = ["lei,registered_as,category"]
    lines.extend(",".join(_csv_field(v) for v in r) for r in rows)
    payload = "\n".join(lines) + "\n"

    tmp = args.out + ".part"
    with open(tmp, "w", encoding="utf-8", newline="") as fh:
        fh.write(payload)
    os.replace(tmp, args.out)
    print(f"wrote {len(rows)} rows → {args.out}", file=sys.stderr)


if __name__ == "__main__":
    main()
