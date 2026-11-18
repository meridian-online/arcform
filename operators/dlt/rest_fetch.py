# /// script
# requires-python = ">=3.10"
# dependencies = ["dlt==1.32.0"]
# ///
"""dlt@1 — page one resource of a REST API and write its records as JSON Lines.

The Protocol step's `source` is dlt's own `rest_api` configuration: base URL, headers,
paginator, endpoint path, query parameters and data selector all come from there. This
script names no source, endpoint, field or header value; it adds only what a
reproducible file needs and dlt does not give:

  REFUSALS, before any request (exit 2, one line each). One resource; no `incremental`,
    `auth`, `processing_steps` or `resolve` parameter anywhere; no `write_disposition`
    other than `replace`. Each of those either carries state between runs, carries a
    secret into a manifest, reshapes records out of sight, or fans one fetch out into
    many — none of which a stateless, frozen ingress step can stand behind.

  CANONICAL OUTPUT. Each record serialised with sorted keys, no whitespace and UTF-8
    kept as UTF-8, one per line, lines sorted by their bytes. Page order from an API is
    often unstable; sorted lines make two runs over the same data byte-identical. No
    records writes an empty file.

  ATOMIC WRITE. Built in `<out>.part` and renamed only on success. A failed or short
    fetch leaves an earlier good `out` untouched and removes the `.part`, so a stale
    partial file is never mistaken for a fresh one.

  EXPECT_TOTAL. Optional dotted path into the FIRST response's JSON body where the API
    reports its record count. Fewer records than that count minus the tolerance is a
    paging failure that stopped early, and nothing is written (exit 1).

  PAGING LOOPS. A GET that repeats a request already made in this run, same full URL
    and query, is a next link that leads back to an earlier page; the fetch fails (exit
    1) and writes nothing. An empty page alone does not end the fetch: some cursor APIs
    return one partway through with a cursor to more records.

  PACING. `--page-delay-seconds` sleeps before each request after the first. dlt's REST
    client retries failed requests but has no pacing of its own.

  RETRIES are dlt's own: the session here is the one RESTClient would build for itself
    (dlt's retrying `Client`, without raise_for_status), so `RUNTIME__REQUEST_MAX_ATTEMPTS`
    and its siblings apply as they would to any dlt source. The session is built here
    only so the pacing can wrap it, outside the retries.

dlt is imported inside `fetch()`, so everything above the fetch is importable, and
tested, with the standard library alone.

Run standalone:
    uv run operators/dlt/rest_fetch.py --source-json "$(cat source.json)" --out out.jsonl
"""
from __future__ import annotations

import argparse
import copy
import json
import math
import os
import sys
import time

# Keys whose contents are what a request carries, not dlt configuration: a query
# parameter called `auth` is the API's business, not a credential block.
REQUEST_CONTENT = frozenset({"params", "headers", "json", "data"})
REFUSED_KEYS = {
    "incremental": "incremental loading keeps state between runs; dlt@1 fetches everything every run",
    "auth": "credentials do not belong in a manifest; dlt@1 fetches public APIs only",
    "processing_steps": "records are reshaped in a later SQL step, where the change is visible",
}


def _where(path: str, key: str) -> str:
    return f"{path}.{key}" if path else key


def _walk(node: object, path: str, problems: list[str]) -> None:
    if isinstance(node, dict):
        for key, value in node.items():
            here = _where(path, str(key))
            if key in REFUSED_KEYS:
                problems.append(f"source: {here} is refused: {REFUSED_KEYS[key]}")
                continue
            if key == "write_disposition":
                disposition = value.get("disposition") if isinstance(value, dict) else value
                if disposition != "replace":
                    problems.append(
                        f"source: {here} is {json.dumps(value)}; dlt@1 only replaces its file"
                    )
                continue
            if key == "params" and isinstance(value, dict):
                for name, param in value.items():
                    kind = param.get("type") if isinstance(param, dict) else None
                    if kind in ("resolve", "incremental"):
                        problems.append(
                            f"source: {here}.{name} is a {kind} parameter; dlt@1 fetches "
                            "one resource with no state"
                        )
                continue
            if key in REQUEST_CONTENT:
                continue
            _walk(value, here, problems)
    elif isinstance(node, list):
        for i, value in enumerate(node):
            _walk(value, f"{path}[{i}]", problems)


def source_problems(source: object) -> list[str]:
    """One message per refusal; empty when the source can run."""
    if not isinstance(source, dict):
        return [f"source must be a mapping (dlt's rest_api configuration); found a {type(source).__name__}"]
    problems: list[str] = []
    client = source.get("client")
    base_url = client.get("base_url") if isinstance(client, dict) else None
    if not isinstance(base_url, str) or not base_url:
        problems.append("source has no client.base_url")
    resources = source.get("resources")
    if not isinstance(resources, list):
        found = "none" if resources is None else f"a {type(resources).__name__}"
        problems.append(f"source.resources must be a list of exactly one resource; found {found}")
    elif len(resources) != 1:
        problems.append(
            f"source.resources must be a list of exactly one resource; found {len(resources)}"
        )
    _walk(source, "", problems)
    return problems


def dotted_get(body: object, path: str) -> object:
    """Walk `a.b.c` through mappings; KeyError naming the path and the missing part."""
    node = body
    for part in path.split("."):
        if not isinstance(node, dict) or part not in node:
            raise KeyError(f"{path}: no {part!r} in the first response body")
        node = node[part]
    return node


def canonical_line(record: object) -> str:
    return json.dumps(record, sort_keys=True, separators=(",", ":"), ensure_ascii=False)


def write_records(records: list, out: str) -> None:
    """Sort the canonical lines by their UTF-8 bytes, write `<out>.part`, rename."""
    lines = sorted(canonical_line(r).encode("utf-8") for r in records)
    part = out + ".part"
    with open(part, "wb") as fh:  # "wb" truncates a stale .part from a killed run
        for line in lines:
            fh.write(line)
            fh.write(b"\n")
    os.replace(part, out)


def _as_count(value: object, path: str) -> int:
    if isinstance(value, bool):
        raise ValueError(f"{path} is {value!r}, not a record count")
    if isinstance(value, int):
        return value
    if isinstance(value, float) and value.is_integer():
        return int(value)
    if isinstance(value, str) and value.strip().isdigit():
        return int(value)
    raise ValueError(f"{path} is {value!r}, not a record count")


class PagingLoop(Exception):
    """The API's next page is a request this run has already made."""


def fetch(source: dict, expect_total_path: str | None, page_delay_seconds: float) -> tuple[list, int | None]:
    """Page the source's one resource; return its records and the first response's total."""
    from dlt.common import jsonpath
    from dlt.sources.helpers.requests.retry import Client
    from dlt.sources.rest_api import rest_api_resources

    source = copy.deepcopy(source)
    defaults = (source.get("resource_defaults") or {}).get("endpoint") or {}
    if isinstance(defaults, str):
        defaults = {}

    resource = source["resources"][0]
    if isinstance(resource, str):
        resource = {"name": resource, "endpoint": {"path": resource}}
    endpoint = resource.get("endpoint")
    if endpoint is None:
        endpoint = {"path": resource.get("name")}
    elif isinstance(endpoint, str):
        endpoint = {"path": endpoint}
    resource["endpoint"] = endpoint
    source["resources"][0] = resource
    selector = endpoint.get("data_selector", defaults.get("data_selector"))

    state: dict = {"first": True, "total": None}

    def on_response(response, *args, **kwargs):
        # A requests response hook: runs on every attempt, before dlt's fallback
        # raise_for_status, so an error response is left for that to raise. requests
        # also runs it on a 3xx before following it (`ok` is true below 400), so a
        # redirect is left alone too and the page it leads to is judged instead.
        if not response.ok or response.is_redirect:
            return response
        body = response.json()
        if selector and not jsonpath.find_values(selector, body):
            raise ValueError(f"data_selector {selector!r} matches nothing in the response from {response.url}")
        if state["first"]:
            state["first"] = False
            if expect_total_path:
                state["total"] = _as_count(dotted_get(body, expect_total_path), expect_total_path)
        return response

    # An endpoint's response_actions replace the defaults' rather than adding to them.
    actions = endpoint.get("response_actions") or defaults.get("response_actions") or []
    endpoint["response_actions"] = list(actions) + [on_response]

    session = Client(raise_for_status=False).session  # what RESTClient builds by default
    send = session.send  # already wrapped in dlt's retries; pacing sits outside them
    sent = [0]
    seen_gets: set[str] = set()

    def paced_send(request, **kwargs):
        # A GET repeating one that already succeeded is a next link that leads back,
        # which no paginator would stop on. dlt's retries sit inside `send`, but a
        # redirect hop is sent through here from inside the outer request's retries,
        # so it can pass more than once; only a URL whose response arrived (ok, or a
        # redirect) is recorded, and a hop retried after failing is not a repeat.
        is_get = request.method == "GET"
        if is_get and request.url in seen_gets:
            raise PagingLoop(
                f"the API's next page repeats an earlier request ({request.url}); "
                "refusing to page in a loop"
            )
        if sent[0] and page_delay_seconds:
            time.sleep(page_delay_seconds)
        sent[0] += 1
        response = send(request, **kwargs)
        if is_get and (response.ok or response.is_redirect):
            seen_gets.add(request.url)
        return response

    session.send = paced_send
    source["client"]["session"] = session

    (only,) = rest_api_resources(source)
    records = list(only)
    return records, state["total"]


def _innermost(exc: BaseException) -> BaseException:
    seen = set()
    while id(exc) not in seen:
        seen.add(id(exc))
        nxt = exc.__cause__ or exc.__context__
        if nxt is None:
            break
        exc = nxt
    return exc


def _one_line(exc: BaseException) -> str:
    root = _innermost(exc)
    text = str(root).strip().splitlines()
    return f"{type(root).__name__}: {text[0] if text else ''}".rstrip(": ")


def _refuse(lines: list[str]) -> None:
    for line in lines:
        print(f"error: {line}", file=sys.stderr)
    sys.exit(2)


def _remove(path: str) -> None:
    try:
        os.remove(path)
    except FileNotFoundError:
        pass


def main() -> None:
    ap = argparse.ArgumentParser(description="Page one REST resource through dlt to sorted JSON Lines.")
    ap.add_argument("--source-json", required=True, help="dlt rest_api configuration, as JSON")
    ap.add_argument("--out", required=True, help="output JSON Lines path (written atomically)")
    ap.add_argument("--expect-total-path", default=None)
    ap.add_argument("--expect-total-tolerance", type=int, default=None)
    ap.add_argument("--page-delay-seconds", type=float, default=0.0)
    args = ap.parse_args()

    try:
        source = json.loads(args.source_json)
    except json.JSONDecodeError as exc:
        _refuse([f"--source-json is not JSON: {exc}"])

    problems = source_problems(source)
    if args.expect_total_tolerance is not None and args.expect_total_path is None:
        problems.append("expect_total has a tolerance but no path")
    if args.expect_total_path is not None and not args.expect_total_path:
        problems.append("expect_total.path is empty")
    tolerance = args.expect_total_tolerance or 0
    if tolerance < 0:
        problems.append(f"expect_total.tolerance is {tolerance}; it must be 0 or more")
    delay = args.page_delay_seconds
    if not math.isfinite(delay) or delay < 0:
        problems.append(f"page_delay_seconds is {delay}; it must be a finite number, 0 or more")
    if problems:
        _refuse(problems)

    part = args.out + ".part"
    try:
        records, total = fetch(source, args.expect_total_path, delay)
    except Exception as exc:  # one line, not dlt's nested traceback
        _remove(part)
        root = _innermost(exc)
        if isinstance(root, PagingLoop):
            print(f"error: {root}", file=sys.stderr)
        else:
            print(f"error: fetch failed: {_one_line(exc)}", file=sys.stderr)
        sys.exit(1)

    if args.expect_total_path is not None:
        if total is None:
            _remove(part)
            print(f"error: expect_total.path {args.expect_total_path} was not read: no response", file=sys.stderr)
            sys.exit(1)
        if len(records) < total - tolerance:
            _remove(part)
            print(
                f"error: fetched {len(records)} of {total} records; refusing to write a partial file",
                file=sys.stderr,
            )
            sys.exit(1)

    try:
        write_records(records, args.out)
    except Exception as exc:
        _remove(part)
        print(f"error: writing {args.out} failed: {_one_line(exc)}", file=sys.stderr)
        sys.exit(1)
    print(f"wrote {len(records)} records to {args.out}", file=sys.stderr)


if __name__ == "__main__":
    main()
