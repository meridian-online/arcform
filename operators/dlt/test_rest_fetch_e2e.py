# /// script
# requires-python = ">=3.10"
# dependencies = ["dlt==1.32.0"]
# ///
"""Run rest_fetch.py end to end against a local API: `uv run operators/dlt/test_rest_fetch_e2e.py`.

Offline: a `http.server` on 127.0.0.1 serves a three-page `json_link` API shaped like
JSON:API (`data`, `links.next`, `meta.pagination.total`) and records every request.
The script runs as a subprocess, because what a caller reads is a stream, an exit
code and a file. `RUNTIME__REQUEST_MAX_ATTEMPTS=1` keeps the failure tests from
waiting on dlt's retries.
"""
from __future__ import annotations

import json
import os
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import unquote, urlsplit

SCRIPT = Path(__file__).resolve().parent / "rest_fetch.py"

RECORDS = [
    {"id": "3", "name": "Société Générale"},
    {"id": "1", "name": "株式会社"},
    {"id": "5", "name": "Zeta", "tags": ["b", "a"]},
    {"id": "2", "name": "Alpha", "nested": {"z": 1, "a": 2}},
    {"id": "4", "name": "Omega"},
]
FIRST_QUERY = "filter[x]=1&page[size]=2&page[cursor]=*"
# The next URLs carry their own cursor and deliberately NOT the first request's filter,
# so a client that re-added the first params would be seen doing it.
NEXT_QUERIES = ["page%5Bcursor%5D=c2&page%5Bsize%5D=2", "page%5Bcursor%5D=c3&page%5Bsize%5D=2"]


class Api:
    """One test's API: pages of records, a reported total, and failures to inject."""

    def __init__(self, pages, total=None, fail_page=None, fail_times=None, with_total=True,
                 endless=False):
        self.pages = pages
        self.endless = endless  # the last page links to itself, so a next link never runs out
        self.total = sum(len(p) for p in pages) if total is None else total
        self.fail_page = fail_page
        self.fail_times = fail_times  # None: fail every time
        self.with_total = with_total
        self.requests: list[tuple[float, str]] = []
        self.redirects = 0
        self.redirect_body = b""
        self.lock = threading.Lock()

    def respond(self, query: str) -> tuple[int, dict | None]:
        with self.lock:
            self.requests.append((time.monotonic(), query))
            if query in NEXT_QUERIES:
                index = NEXT_QUERIES.index(query) + 1
            else:
                index = 0
            if index == self.fail_page:
                if self.fail_times is None or self.fail_times > 0:
                    if self.fail_times is not None:
                        self.fail_times -= 1
                    return 500, None
        nxt = None
        if index + 1 < len(self.pages):
            nxt = f"{self.base}/items?{NEXT_QUERIES[index]}"
        elif self.endless and index > 0:
            nxt = f"{self.base}/items?{NEXT_QUERIES[index - 1]}"
        body = {"data": self.pages[index], "links": {"next": nxt}}
        body["meta"] = {"pagination": {"total": self.total}} if self.with_total else {}
        return 200, body


def serve(api: Api):
    class Handler(BaseHTTPRequestHandler):
        def do_GET(self):  # noqa: N802
            parts = urlsplit(self.path)
            if parts.path.startswith("/moved/"):
                # A redirect to the real first page, its body set by the test: JSON that
                # looks like a page with the wrong total, or not JSON at all.
                api.redirects += 1
                payload = api.redirect_body
                self.send_response(302)
                self.send_header("Location", f"/{parts.path[len('/moved/'):]}?{parts.query}")
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(payload)))
                self.end_headers()
                self.wfile.write(payload)
                return
            status, body = api.respond(parts.query)
            payload = json.dumps(body).encode() if body is not None else b'{"error":"boom"}'
            self.send_response(status)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)

        def log_message(self, *args):
            pass

    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    api.base = f"http://127.0.0.1:{server.server_address[1]}"
    threading.Thread(target=server.serve_forever, daemon=True).start()
    return server


def canonical(record) -> bytes:
    return json.dumps(record, sort_keys=True, separators=(",", ":"), ensure_ascii=False).encode()


class RestFetchE2E(unittest.TestCase):
    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self._tmp.cleanup)
        self.dir = Path(self._tmp.name)
        self.out = self.dir / "out.jsonl"

    def start(self, api: Api) -> Api:
        server = serve(api)
        self.addCleanup(server.server_close)
        self.addCleanup(server.shutdown)
        return api

    def source(self, api: Api, **endpoint):
        ep = {
            "path": "items",
            "data_selector": "data",
            "params": {"filter[x]": "1", "page[size]": 2, "page[cursor]": "*"},
        }
        ep.update(endpoint)
        return {
            "client": {
                "base_url": api.base + "/",
                "paginator": {"type": "json_link", "next_url_path": "links.next"},
            },
            "resources": [{"name": "items", "endpoint": ep}],
        }

    def run_op(self, src, *extra, env=None):
        cmd = [sys.executable, str(SCRIPT), "--source-json", json.dumps(src), "--out", str(self.out), *extra]
        environ = {**os.environ, "RUNTIME__REQUEST_MAX_ATTEMPTS": "1", **(env or {})}
        # Run outside the repository so no `.dlt/` folder there is read as configuration.
        # The timeout turns a fetch that never stops paging into a failure, not a hang.
        return subprocess.run(cmd, capture_output=True, text=True, env=environ, cwd=self.dir,
                              timeout=60)

    def three_pages(self, **kwargs) -> Api:
        return self.start(Api([RECORDS[0:2], RECORDS[2:4], RECORDS[4:5]], **kwargs))

    def assertFailed(self, r, *needles):
        self.assertEqual(r.returncode, 1, r.stderr)
        self.assertEqual(len(r.stderr.strip().splitlines()), 1, r.stderr)
        for needle in needles:
            self.assertIn(needle, r.stderr)
        self.assertFalse(Path(str(self.out) + ".part").exists())

    def test_follows_every_page_and_writes_sorted_lines(self) -> None:
        api = self.three_pages()
        r = self.run_op(self.source(api))
        self.assertEqual(r.returncode, 0, r.stderr)
        expected = b"".join(line + b"\n" for line in sorted(canonical(rec) for rec in RECORDS))
        self.assertEqual(self.out.read_bytes(), expected)
        self.assertIn("Société Générale".encode(), expected)
        self.assertEqual(len(api.requests), 3)

    def test_bracketed_params_reach_the_first_request_and_next_urls_are_followed_as_given(self) -> None:
        api = self.three_pages()
        r = self.run_op(self.source(api))
        self.assertEqual(r.returncode, 0, r.stderr)
        queries = [q for _, q in api.requests]
        self.assertEqual(unquote(queries[0]), FIRST_QUERY)
        self.assertEqual(queries[1:], NEXT_QUERIES)

    def test_two_runs_write_identical_bytes(self) -> None:
        api = self.three_pages()
        self.assertEqual(self.run_op(self.source(api)).returncode, 0)
        first = self.out.read_bytes()
        self.out.unlink()
        # The second run is served the same records in the reverse page order.
        api.pages = [RECORDS[4:5][::-1] + RECORDS[2:4][::-1], RECORDS[0:2][::-1], []]
        self.assertEqual(self.run_op(self.source(api)).returncode, 0)
        self.assertEqual(self.out.read_bytes(), first)

    def test_expect_total_within_the_tolerance_writes_the_file(self) -> None:
        api = self.three_pages(total=6)
        r = self.run_op(self.source(api), "--expect-total-path", "meta.pagination.total",
                        "--expect-total-tolerance", "1")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(len(self.out.read_bytes().splitlines()), 5)

    def test_a_short_fetch_is_refused_and_writes_nothing(self) -> None:
        api = self.three_pages(total=10)
        r = self.run_op(self.source(api), "--expect-total-path", "meta.pagination.total")
        self.assertFailed(r, "fetched 5 of 10 records; refusing to write a partial file")
        self.assertFalse(self.out.exists())

    def test_a_failed_page_keeps_the_earlier_file(self) -> None:
        self.out.write_bytes(b'{"earlier":"run"}\n')
        Path(str(self.out) + ".part").write_bytes(b"stale\n")
        api = self.three_pages(fail_page=1)
        r = self.run_op(self.source(api))
        self.assertFailed(r, "500")
        self.assertEqual(self.out.read_bytes(), b'{"earlier":"run"}\n')
        self.assertEqual(len(api.requests), 2, "RUNTIME__REQUEST_MAX_ATTEMPTS=1 reached the session")

    def test_a_page_that_fails_once_is_retried_when_the_runtime_allows(self) -> None:
        api = self.three_pages(fail_page=1, fail_times=1)
        r = self.run_op(self.source(api), env={"RUNTIME__REQUEST_MAX_ATTEMPTS": "2",
                                               "RUNTIME__REQUEST_BACKOFF_FACTOR": "0"})
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(len(api.requests), 4)
        self.assertEqual(len(self.out.read_bytes().splitlines()), 5)

    def test_a_missing_total_path_is_refused(self) -> None:
        api = self.three_pages(with_total=False)
        r = self.run_op(self.source(api), "--expect-total-path", "meta.pagination.total")
        self.assertFailed(r, "meta.pagination.total")
        self.assertFalse(self.out.exists())
        self.assertEqual(len(api.requests), 1, "the check runs on the first response")

    def test_a_data_selector_that_matches_nothing_fails(self) -> None:
        api = self.three_pages()
        r = self.run_op(self.source(api, data_selector="results"))
        self.assertFailed(r, "data_selector", "results")
        self.assertFalse(self.out.exists())

    def test_an_empty_first_page_writes_an_empty_file(self) -> None:
        api = self.start(Api([[]]))
        r = self.run_op(self.source(api), "--expect-total-path", "meta.pagination.total")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(self.out.read_bytes(), b"")

    def test_an_empty_page_with_a_next_link_ends_the_fetch(self) -> None:
        # The empty page links to itself, so only the empty page can stop the paging.
        api = self.start(Api([RECORDS[0:2], []], endless=True))
        r = self.run_op(self.source(api))
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(len(api.requests), 2)
        expected = b"".join(line + b"\n" for line in sorted(canonical(rec) for rec in RECORDS[0:2]))
        self.assertEqual(self.out.read_bytes(), expected)

    def test_an_empty_page_ends_the_fetch_before_any_later_page(self) -> None:
        api = self.start(Api([RECORDS[0:2], [], RECORDS[2:4]]))
        r = self.run_op(self.source(api))
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(len(api.requests), 2)
        self.assertEqual(len(self.out.read_bytes().splitlines()), 2)

    def test_a_fetch_an_empty_page_leaves_short_is_refused(self) -> None:
        api = self.start(Api([RECORDS[0:2], []], total=5, endless=True))
        r = self.run_op(self.source(api), "--expect-total-path", "meta.pagination.total")
        self.assertFailed(r, "fetched 2 of 5 records")
        self.assertFalse(self.out.exists())

    def test_the_page_delay_paces_every_request_after_the_first(self) -> None:
        api = self.three_pages()
        r = self.run_op(self.source(api), "--page-delay-seconds", "0.3")
        self.assertEqual(r.returncode, 0, r.stderr)
        times = [t for t, _ in api.requests]
        gaps = [b - a for a, b in zip(times, times[1:])]
        self.assertEqual(len(gaps), 2)
        for gap in gaps:
            self.assertGreaterEqual(gap, 0.3)

    def test_a_redirected_first_request_is_followed_and_judged_by_its_target(self) -> None:
        # requests runs response hooks on a 3xx before following it; the hook must leave
        # the redirect alone and read the total from the page the redirect leads to.
        api = self.three_pages()
        api.redirect_body = json.dumps({"data": [], "meta": {"pagination": {"total": 999}}}).encode()
        r = self.run_op(self.source(api, path="moved/items"),
                        "--expect-total-path", "meta.pagination.total")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(api.redirects, 1)
        self.assertEqual(unquote(api.requests[0][1]), FIRST_QUERY)
        expected = b"".join(line + b"\n" for line in sorted(canonical(rec) for rec in RECORDS))
        self.assertEqual(self.out.read_bytes(), expected)

    def test_a_redirect_whose_body_is_not_json_is_followed(self) -> None:
        api = self.three_pages()
        api.redirect_body = b"<html>Moved</html>"
        r = self.run_op(self.source(api, path="moved/items"))
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(len(self.out.read_bytes().splitlines()), 5)

    def test_a_refused_source_sends_no_request(self) -> None:
        api = self.three_pages()
        r = self.run_op(self.source(api, incremental={"cursor_path": "id"}))
        self.assertEqual(r.returncode, 2, r.stderr)
        self.assertEqual(api.requests, [])


if __name__ == "__main__":
    unittest.main(verbosity=2)
