# /// script
# requires-python = ">=3.10"
# dependencies = []
# ///
"""Cover rest_fetch.py's decision half with the standard library alone.

dlt loads inside `fetch()`, so every refusal the operator makes before its first
request, and the serialisation and atomic write after its last, is testable here
without it. `test_rest_fetch_e2e.py` runs the fetch itself under `uv`.
"""
from __future__ import annotations

import copy
import importlib.util
import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
SCRIPT = HERE / "rest_fetch.py"


def _load():
    spec = importlib.util.spec_from_file_location("rest_fetch_under_test", SCRIPT)
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


op = _load()

# The catalogue's example step: the one place a real source is named.
GLEIF = {
    "client": {
        "base_url": "https://api.gleif.org/api/v1/",
        "headers": {"User-Agent": "Meridian Protocol (open-analytics; research@meridian.online)"},
        "paginator": {"type": "json_link", "next_url_path": "links.next"},
    },
    "resources": [
        {
            "name": "lei_records",
            "endpoint": {
                "path": "lei-records",
                "data_selector": "data",
                "params": {
                    "filter[entity.registeredAt]": "RA000665",
                    "page[size]": 200,
                    "page[cursor]": "*",
                },
            },
        }
    ],
}


def source(**changes):
    s = copy.deepcopy(GLEIF)
    for key, value in changes.items():
        s[key] = value
    return s


def endpoint(**changes):
    s = copy.deepcopy(GLEIF)
    s["resources"][0]["endpoint"].update(changes)
    return s


class SourceProblemsTest(unittest.TestCase):
    def assertRefused(self, src, *needles):
        problems = op.source_problems(src)
        self.assertEqual(len(problems), 1, problems)
        for needle in needles:
            self.assertIn(needle, problems[0])
        return problems[0]

    def test_the_gleif_source_has_no_problems(self) -> None:
        self.assertEqual(op.source_problems(copy.deepcopy(GLEIF)), [])

    def test_a_source_that_is_not_a_mapping_is_refused(self) -> None:
        self.assertRefused(["not", "a", "mapping"], "source", "mapping", "list")

    def test_a_source_without_a_client_is_refused(self) -> None:
        s = source()
        del s["client"]
        self.assertRefused(s, "client.base_url")

    def test_a_source_without_a_base_url_is_refused(self) -> None:
        s = source()
        del s["client"]["base_url"]
        self.assertRefused(s, "client.base_url")

    def test_an_empty_base_url_is_refused(self) -> None:
        s = source()
        s["client"]["base_url"] = ""
        self.assertRefused(s, "client.base_url")

    def test_resources_that_are_not_a_list_are_refused(self) -> None:
        self.assertRefused(source(resources={"name": "x"}), "resources", "exactly one")

    def test_no_resources_are_refused(self) -> None:
        self.assertRefused(source(resources=[]), "resources", "exactly one", "0")

    def test_two_resources_are_refused(self) -> None:
        s = source()
        s["resources"].append(copy.deepcopy(s["resources"][0]))
        self.assertRefused(s, "resources", "exactly one", "2")

    def test_a_missing_resources_key_is_refused(self) -> None:
        s = source()
        del s["resources"]
        self.assertRefused(s, "resources", "exactly one")

    def test_incremental_on_the_endpoint_is_refused(self) -> None:
        s = endpoint(incremental={"cursor_path": "updated_at", "initial_value": "2024-01-01"})
        self.assertRefused(s, "incremental", "resources[0].endpoint.incremental")

    def test_incremental_in_resource_defaults_is_refused(self) -> None:
        s = source(resource_defaults={"endpoint": {"incremental": {"cursor_path": "x"}}})
        self.assertRefused(s, "resource_defaults.endpoint.incremental")

    def test_auth_on_the_client_is_refused(self) -> None:
        s = source()
        s["client"]["auth"] = {"type": "bearer", "token": "t"}
        self.assertRefused(s, "auth", "client.auth")

    def test_auth_on_the_endpoint_is_refused(self) -> None:
        s = endpoint(auth={"type": "bearer", "token": "t"})
        self.assertRefused(s, "resources[0].endpoint.auth")

    def test_processing_steps_are_refused(self) -> None:
        s = source()
        s["resources"][0]["processing_steps"] = [{"filter": "x"}]
        self.assertRefused(s, "processing_steps", "resources[0].processing_steps")

    def test_a_resolve_param_is_refused(self) -> None:
        s = endpoint(params={"id": {"type": "resolve", "resource": "parents", "field": "id"}})
        self.assertRefused(s, "resolve", "resources[0].endpoint.params.id")

    def test_a_resolve_param_in_resource_defaults_is_refused(self) -> None:
        s = source(resource_defaults={"endpoint": {"params": {"id": {"type": "resolve"}}}})
        self.assertRefused(s, "resolve", "resource_defaults.endpoint.params.id")

    def test_an_incremental_param_is_refused(self) -> None:
        s = endpoint(params={"since": {"type": "incremental", "cursor_path": "updated_at"}})
        self.assertRefused(s, "incremental", "resources[0].endpoint.params.since")

    def test_a_write_disposition_other_than_replace_is_refused(self) -> None:
        s = source()
        s["resources"][0]["write_disposition"] = "merge"
        self.assertRefused(s, "write_disposition", "resources[0].write_disposition", "merge")

    def test_a_write_disposition_mapping_other_than_replace_is_refused(self) -> None:
        s = source(resource_defaults={"write_disposition": {"disposition": "append"}})
        self.assertRefused(s, "resource_defaults.write_disposition")

    def test_a_replace_write_disposition_is_accepted(self) -> None:
        s = source()
        s["resources"][0]["write_disposition"] = "replace"
        self.assertEqual(op.source_problems(s), [])
        s["resources"][0]["write_disposition"] = {"disposition": "replace"}
        self.assertEqual(op.source_problems(s), [])

    def test_a_query_parameter_or_header_named_like_a_refused_key_is_request_content(self) -> None:
        # `params`, `headers`, `json` and `data` are what the request carries, not dlt's
        # configuration: an API whose query parameter is called `auth` is not refused.
        s = endpoint(params={"auth": "public", "incremental": "no"}, json={"processing_steps": 1})
        s["client"]["headers"]["auth"] = "x"
        self.assertEqual(op.source_problems(s), [])

    def test_every_problem_is_reported_at_once(self) -> None:
        s = endpoint(incremental={"cursor_path": "x"}, auth={"type": "bearer"})
        s["resources"][0]["write_disposition"] = "merge"
        self.assertEqual(len(op.source_problems(s)), 3)


class DottedGetTest(unittest.TestCase):
    def test_dotted_get_reads_a_nested_count(self) -> None:
        body = {"meta": {"pagination": {"total": 28347}}, "data": []}
        self.assertEqual(op.dotted_get(body, "meta.pagination.total"), 28347)

    def test_dotted_get_names_the_missing_part(self) -> None:
        with self.assertRaises(KeyError) as caught:
            op.dotted_get({"meta": {"count": 1}}, "meta.pagination.total")
        said = str(caught.exception)
        self.assertIn("meta.pagination.total", said)
        self.assertIn("pagination", said)

    def test_dotted_get_refuses_to_walk_into_a_non_mapping(self) -> None:
        with self.assertRaises(KeyError):
            op.dotted_get({"meta": [1, 2]}, "meta.total")


class CanonicalLineTest(unittest.TestCase):
    def test_canonical_line_sorts_keys_and_keeps_utf8(self) -> None:
        self.assertEqual(op.canonical_line({"b": 1, "a": "株式会社"}), '{"a":"株式会社","b":1}')

    def test_nested_keys_are_sorted_too(self) -> None:
        self.assertEqual(op.canonical_line({"z": {"y": 1, "x": [2, {"b": 0, "a": 0}]}}),
                         '{"z":{"x":[2,{"a":0,"b":0}],"y":1}}')


class WriteRecordsTest(unittest.TestCase):
    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self._tmp.cleanup)
        self.out = Path(self._tmp.name) / "out.jsonl"

    def test_write_records_sorts_by_bytes_and_ends_each_line(self) -> None:
        records = [{"n": "Société Générale"}, {"n": "Zeta"}, {"n": "株式会社"}, {"n": "Alpha"}]
        op.write_records(records, str(self.out))
        raw = self.out.read_bytes()
        self.assertTrue(raw.endswith(b"\n"))
        lines = raw.split(b"\n")[:-1]
        self.assertEqual(lines, sorted(lines))
        self.assertEqual(len(lines), 4)
        self.assertIn("Société Générale".encode("utf-8"), raw)
        self.assertNotIn(b"\\u", raw)
        self.assertFalse(Path(str(self.out) + ".part").exists())

    def test_write_records_overwrites_a_stale_part(self) -> None:
        part = Path(str(self.out) + ".part")
        part.write_bytes(b'{"stale":true}\n' * 50)
        op.write_records([{"a": 1}], str(self.out))
        self.assertEqual(self.out.read_bytes(), b'{"a":1}\n')
        self.assertFalse(part.exists())

    def test_write_records_replaces_an_earlier_file(self) -> None:
        self.out.write_bytes(b"old\n")
        op.write_records([{"a": 1}], str(self.out))
        self.assertEqual(self.out.read_bytes(), b'{"a":1}\n')

    def test_zero_records_write_an_empty_file(self) -> None:
        op.write_records([], str(self.out))
        self.assertTrue(self.out.exists())
        self.assertEqual(self.out.read_bytes(), b"")


class MainRefusesBeforeAnyRequestTest(unittest.TestCase):
    """The refusals run before dlt is imported, so they work under plain python3."""

    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self._tmp.cleanup)
        self.out = Path(self._tmp.name) / "out.jsonl"

    def run_op(self, src, *extra):
        cmd = [sys.executable, str(SCRIPT), "--source-json", json.dumps(src),
               "--out", str(self.out), *extra]
        return subprocess.run(cmd, capture_output=True, text=True)

    def test_each_source_problem_is_one_line_and_exit_2(self) -> None:
        s = endpoint(incremental={"cursor_path": "x"}, auth={"type": "bearer"})
        r = self.run_op(s)
        self.assertEqual(r.returncode, 2, r.stderr)
        self.assertEqual(len(r.stderr.strip().splitlines()), 2, r.stderr)
        self.assertFalse(self.out.exists())

    def test_source_json_that_does_not_parse_is_refused(self) -> None:
        cmd = [sys.executable, str(SCRIPT), "--source-json", "{nope", "--out", str(self.out)]
        r = subprocess.run(cmd, capture_output=True, text=True)
        self.assertEqual(r.returncode, 2, r.stderr)
        self.assertEqual(len(r.stderr.strip().splitlines()), 1, r.stderr)

    def test_a_negative_page_delay_is_refused(self) -> None:
        r = self.run_op(GLEIF, "--page-delay-seconds", "-1")
        self.assertEqual(r.returncode, 2, r.stderr)
        self.assertIn("page_delay_seconds", r.stderr)

    def test_a_negative_tolerance_is_refused(self) -> None:
        r = self.run_op(GLEIF, "--expect-total-path", "meta.total", "--expect-total-tolerance", "-1")
        self.assertEqual(r.returncode, 2, r.stderr)
        self.assertIn("tolerance", r.stderr)

    def test_a_tolerance_without_a_path_is_refused(self) -> None:
        r = self.run_op(GLEIF, "--expect-total-tolerance", "5")
        self.assertEqual(r.returncode, 2, r.stderr)
        self.assertIn("expect_total", r.stderr)


if __name__ == "__main__":
    unittest.main()
