#!/usr/bin/env python3
"""A pushed tag is a release only if it names this tree's version and its changelog entry.

WHAT IT REFUSES
    release.yml publishes whatever tag starts it: four platform binaries, a
    GitHub release, two crates on crates.io and the install script. Its first
    job, `tag-guard`, runs this script, and every other job waits on that one,
    so a refusal here stops the run before anything is built or published.

        exit 3  the tag is not `v` followed by three dot-separated numbers
        exit 4  the tag's number is not `[package] version` in Cargo.toml
        exit 5  CHANGELOG.md has no `## [X.Y.Z]` heading for that number
        exit 6  the heading is there with nothing under it, or its section is
                longer than 120,000 characters (GitHub cuts a release body at
                125,000, so a longer section would be published truncated)

    On success it writes that heading's section of the changelog as the step
    output `notes` (to the file `--github-output` names, which on a runner is
    $GITHUB_OUTPUT). `tag-guard` exposes it as a job output, and the `release`
    job publishes it as the release body in place of GitHub's generated notes.

WHAT ONLY A PUSHED TAG RUNS, AND WHAT STANDS IN FOR IT
    GitHub's tag matching cannot be run anywhere but GitHub. `--wiring` reads
    release.yml instead: the tag filter is exactly `v[0-9]+.[0-9]+.[0-9]+`,
    the guard step is unconditional and refuses rather than warns, every other
    job needs it, none runs after a failure, and the release body is the
    guard's notes. That is a reading of the workflow text, and it is the
    honest limit of what a pull request can check about a trigger.

    `--release-rehearsal` goes further for the guard itself: it executes the
    `tag-guard` job's own `run:` line, read out of release.yml, against a copy
    of this tree's Cargo.toml and CHANGELOG.md, once with the tag a release of
    this tree would carry and once for each refusal. Nothing is published.

        exit 7  --wiring found the release path not wired as described above
        exit 8  --release-rehearsal found the guard step not doing its job
        exit 2  the check could not run (unreadable file, a workflow shape the
                reader does not recognise)
        exit 1  --self-test: a case did not get the outcome it requires

    The workflow is read without a YAML library: the standard library has
    none, and this script must run on a bare runner. The reader below
    understands the shapes release.yml is written in and refuses (exit 2) any
    other, because a wiring verdict drawn from a half-read workflow is worse
    than no verdict.

USAGE
    scripts/check_release_tag.py --tag TAG [--github-output FILE] [--root DIR]
    scripts/check_release_tag.py --wiring [--root DIR]
    scripts/check_release_tag.py --release-rehearsal [--root DIR]
    scripts/check_release_tag.py --self-test
"""

from __future__ import annotations

import argparse
import contextlib
import io
import os
import re
import secrets
import shutil
import subprocess
import sys
import tempfile
from dataclasses import dataclass, field
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
RELEASE_WORKFLOW = ".github/workflows/release.yml"
SCRIPT_REL = "scripts/check_release_tag.py"
MANIFEST_REL = "Cargo.toml"
CHANGELOG_REL = "CHANGELOG.md"

GUARD_JOB = "tag-guard"
GUARD_STEP_ID = "guard"
PUBLISH_ACTION = "softprops/action-gh-release"
TAG_FILTER = "v[0-9]+.[0-9]+.[0-9]+"
# The notes travel as step output -> job output -> the publish step's `body:`.
GUARD_ARGS = '--tag "${GITHUB_REF_NAME}" --github-output "${GITHUB_OUTPUT}"'
NOTES_OUTPUT = "notes"
NOTES_JOB_OUTPUT = f"${{{{ steps.{GUARD_STEP_ID}.outputs.{NOTES_OUTPUT} }}}}"
NOTES_BODY = f"${{{{ needs.{GUARD_JOB}.outputs.{NOTES_OUTPUT} }}}}"
# GitHub refuses a release body over 125,000 characters; this leaves headroom.
NOTES_LIMIT = 120_000

# Status functions that let a job run after a job it needs has failed.
# GitHub reads function names case-insensitively, so this does too.
RUNS_AFTER_FAILURE = re.compile(r"\b(always|cancelled|failure)\s*\(", re.IGNORECASE)
TAG_RE = re.compile(r"^v([0-9]+\.[0-9]+\.[0-9]+)$")
HEADING_RE = re.compile(r"^## \[([^\]]+)\]")

EXIT_CANNOT_RUN = 2
EXIT_TAG_FORM = 3
EXIT_MANIFEST = 4
EXIT_NO_HEADING = 5
EXIT_EMPTY_SECTION = 6
EXIT_WIRING = 7
EXIT_REHEARSAL = 8


class Refused(Exception):
    """A finding. Carries the exit code that says which check refused."""

    def __init__(self, code: int, message: str) -> None:
        super().__init__(message)
        self.code = code


# ── the tag ─────────────────────────────────────────────────────────────────


def package_version(manifest: Path) -> str:
    """`version` under `[package]` in Cargo.toml (arcform has no workspace table)."""
    try:
        import tomllib
    except ModuleNotFoundError:
        raise Refused(EXIT_CANNOT_RUN, "this check needs Python 3.11 or later (tomllib)") from None
    try:
        data = tomllib.loads(manifest.read_text(encoding="utf-8"))
    except OSError as exc:
        raise Refused(EXIT_CANNOT_RUN, f"{manifest}: {exc}") from None
    except tomllib.TOMLDecodeError as exc:
        raise Refused(EXIT_CANNOT_RUN, f"{manifest}: not valid TOML: {exc}") from None
    package = data.get("package")
    version = package.get("version") if isinstance(package, dict) else None
    if not isinstance(version, str):
        raise Refused(EXIT_CANNOT_RUN, f"{manifest}: no literal `version` under [package]")
    return version


def _section_bounds(lines: list[str], version: str) -> tuple[int, int] | None:
    """Line indices of the body under `## [version]`: (first, one past last)."""
    for i, line in enumerate(lines):
        match = HEADING_RE.match(line)
        if match and match.group(1) == version:
            end = next((j for j in range(i + 1, len(lines)) if lines[j].startswith("## ")), len(lines))
            return i + 1, end
    return None


def changelog_section(changelog: Path, version: str) -> str:
    """The text under `## [version]`, up to the next `## ` heading, trimmed."""
    try:
        lines = changelog.read_text(encoding="utf-8").splitlines()
    except OSError as exc:
        raise Refused(EXIT_CANNOT_RUN, f"{changelog}: {exc}") from None
    bounds = _section_bounds(lines, version)
    if bounds is None:
        raise Refused(
            EXIT_NO_HEADING,
            f"{changelog.name} has no `## [{version}]` heading. Move the [Unreleased] "
            "items under that heading, dated, before tagging.",
        )
    body = "\n".join(lines[bounds[0] : bounds[1]]).strip()
    if not body:
        raise Refused(
            EXIT_EMPTY_SECTION,
            f"{changelog.name}'s `## [{version}]` heading has nothing under it; the "
            "release body would be empty.",
        )
    if len(body) > NOTES_LIMIT:
        raise Refused(
            EXIT_EMPTY_SECTION,
            f"{changelog.name}'s `## [{version}]` section is {len(body):,} characters, over "
            f"the {NOTES_LIMIT:,} this guard allows. GitHub cuts a release body at 125,000, "
            "so the release page would end mid-sentence. Shorten the section.",
        )
    return body + "\n"


def write_output(github_output: Path, body: str) -> None:
    """Append `notes` to a GitHub step-output file, multi-line form.

    The delimiter is random so no line of the changelog can end the value early.
    """
    delimiter = f"NOTES_{secrets.token_hex(16)}"
    with github_output.open("a", encoding="utf-8") as fh:
        fh.write(f"{NOTES_OUTPUT}<<{delimiter}\n{body}{delimiter}\n")


def read_output(github_output: Path) -> str | None:
    """The `notes` value back out of a step-output file, or None if absent."""
    if not github_output.is_file():
        return None
    match = re.search(
        rf"^{NOTES_OUTPUT}<<(\S+)\n(.*?)^\1$", github_output.read_text(encoding="utf-8"), re.S | re.M
    )
    return match.group(2) if match else None


def check_tag(root: Path, tag: str, github_output: Path | None) -> None:
    match = TAG_RE.fullmatch(tag)
    if not match:
        raise Refused(
            EXIT_TAG_FORM,
            f"{tag!r} is not a release tag: a release tag is `v` followed by three "
            "dot-separated numbers, like v0.2.0.",
        )
    version = match.group(1)
    manifest_version = package_version(root / MANIFEST_REL)
    if version != manifest_version:
        raise Refused(
            EXIT_MANIFEST,
            f"the tag says {version} and {MANIFEST_REL} says {manifest_version}. "
            "Tag the commit that bumps the version, or bump it first.",
        )
    body = changelog_section(root / CHANGELOG_REL, version)
    if github_output is not None:
        write_output(github_output, body)
    print(f"{tag}: {MANIFEST_REL} and {CHANGELOG_REL} agree; the release notes are that section.")


# ── reading the workflow ────────────────────────────────────────────────────
#
# Line-structured rather than YAML-parsed. It reads the two-space block style
# release.yml is written in: jobs at indent 2, job keys at 4, job outputs at 6,
# steps as `      - ` items with keys at 8 and `with:` entries at 10. Every
# other shape on a line it has to read is refused with exit 2.

BLOCK_SCALARS = {"|", ">", "|-", ">-", "|+", ">+"}
_KEY = r"([A-Za-z0-9_-]+):(?:[ ]+(.*))?"


@dataclass
class Job:
    id: str
    lineno: int
    condition: str = ""
    needs: tuple[str, ...] = ()
    outputs: dict[str, str] = field(default_factory=dict)
    continue_on_error: str = ""


@dataclass
class Step:
    job: str
    lineno: int
    name: str = ""
    step_id: str = ""
    condition: str = ""
    commands: tuple[str, ...] = ()
    continue_on_error: str = ""
    uses: str = ""
    with_: dict[str, str] = field(default_factory=dict)


def _indent(line: str) -> int:
    return len(line) - len(line.lstrip(" "))


def _content(line: str) -> bool:
    stripped = line.strip()
    return bool(stripped) and not stripped.startswith("#")


def _scalar(inline: str, where: str) -> str:
    """A one-line YAML scalar: quoted, or plain with any ` #` comment dropped."""
    if inline[:1] in ("'", '"'):
        quote = inline[0]
        end = inline.find(quote, 1)
        if end == -1 or "\\" in inline[:end] or inline[end + 1 :].strip()[:1] not in ("", "#"):
            raise Refused(EXIT_CANNOT_RUN, f"{where}: cannot read the quoted value {inline}")
        return inline[1:end]
    return re.split(r"\s#", inline, maxsplit=1)[0].strip()


def _read_value(inline: str, lines: list[str], i: int, key_indent: int, where: str) -> tuple[list[str], int]:
    """One mapping value, block scalar or one line. Returns (lines, next index)."""
    if inline in BLOCK_SCALARS:
        out: list[str] = []
        j = i + 1
        while j < len(lines) and (not lines[j].strip() or _indent(lines[j]) > key_indent):
            out.append(lines[j].strip())
            j += 1
        while out and not out[-1]:
            out.pop()
        return out, j
    if inline.startswith(("|", ">")):
        raise Refused(EXIT_CANNOT_RUN, f"{where}: cannot read the block scalar header `{inline}`")
    if not inline:
        return [], i + 1
    j = i + 1
    while j < len(lines) and not _content(lines[j]):
        j += 1
    if j < len(lines) and _indent(lines[j]) > key_indent:
        raise Refused(EXIT_CANNOT_RUN, f"{where}: the value is continued onto line {j + 1}; write it on one line")
    return [_scalar(inline, where)], i + 1


def _skip_nested(lines: list[str], i: int, key_indent: int) -> int:
    j = i + 1
    while j < len(lines) and (not _content(lines[j]) or _indent(lines[j]) > key_indent):
        j += 1
    return j


def push_tag_filters(lines: list[str], rel: str) -> list[str]:
    """The `on: push: tags:` entries, as an inline `[...]` list or a block list."""
    i, n = 0, len(lines)
    while i < n and lines[i].rstrip() not in ("on:", '"on":', "'on':"):
        i += 1
    if i == n:
        raise Refused(EXIT_CANNOT_RUN, f"{rel}: no top-level `on:` block")
    i += 1
    in_push = False
    while i < n and (not _content(lines[i]) or lines[i].startswith(" ")):
        line = lines[i]
        if re.match(r"^  [A-Za-z_]+:", line):
            in_push = line.strip() == "push:"
        elif in_push and re.match(r"^    tags:", line):
            inline = line.split(":", 1)[1].strip()
            if inline.startswith("["):
                if not inline.endswith("]"):
                    raise Refused(EXIT_CANNOT_RUN, f"{rel}:{i + 1}: cannot read `tags: {inline}`")
                items = [s.strip() for s in inline[1:-1].split(",") if s.strip()]
                return [s.strip("'\"") for s in items]
            if inline:
                raise Refused(EXIT_CANNOT_RUN, f"{rel}:{i + 1}: cannot read `tags: {inline}`")
            items = []
            j = i + 1
            while j < n and re.match(r"^      - ", lines[j]):
                items.append(lines[j].split("- ", 1)[1].strip().strip("'\""))
                j += 1
            return items
        i += 1
    return []


def _apply_step_key(step: Step, key: str, inline: str, lines: list[str], i: int, where: str) -> int:
    if key == "with":
        if inline:
            raise Refused(EXIT_CANNOT_RUN, f"{where}: `with:` is not a block mapping")
        j = i + 1
        while j < len(lines):
            line = lines[j]
            if not _content(line):
                j += 1
                continue
            if _indent(line) <= 8:
                break
            match = re.fullmatch(" {10}" + _KEY, line)
            if not match:
                raise Refused(EXIT_CANNOT_RUN, f"{where}:{j + 1}: cannot read this line as a `with:` entry")
            values, j = _read_value((match.group(2) or "").strip(), lines, j, 10, f"{where}:{j + 1}")
            step.with_[match.group(1)] = "\n".join(values)
        return j
    if key in ("run", "if", "name", "id", "uses", "continue-on-error"):
        values, j = _read_value(inline, lines, i, 8, where)
        if not values:
            raise Refused(EXIT_CANNOT_RUN, f"{where}: `{key}:` has no value this reader can see")
        if key == "run":
            step.commands = tuple(values)
        else:
            joined = " ".join(v for v in values if v)
            setattr(step, {"if": "condition", "id": "step_id", "continue-on-error": "continue_on_error"}.get(key, key), joined)
        return j
    return _skip_nested(lines, i, 8)


def scan_workflow(root: Path, rel: str) -> tuple[list[str], dict[str, Job], list[Step]]:
    """The workflow's lines, its jobs and their steps."""
    try:
        lines = (root / rel).read_text(encoding="utf-8").splitlines()
    except OSError as exc:
        raise Refused(EXIT_CANNOT_RUN, f"{rel}: {exc}") from None
    for n, line in enumerate(lines, 1):
        if re.match(r"^ *\t", line):
            raise Refused(EXIT_CANNOT_RUN, f"{rel}:{n}: a tab in the indentation")
    try:
        i = next(k for k, line in enumerate(lines) if line.rstrip() == "jobs:") + 1
    except StopIteration:
        raise Refused(EXIT_CANNOT_RUN, f"{rel}: no top-level `jobs:` block") from None

    jobs: dict[str, Job] = {}
    steps: list[Step] = []
    job: Job | None = None
    step: Step | None = None
    in_steps = False

    while i < len(lines):
        line = lines[i]
        where = f"{rel}:{i + 1}"
        if not _content(line):
            i += 1
            continue
        indent = _indent(line)
        if indent == 0:
            break
        if indent == 2:
            match = re.fullmatch(r"  ([A-Za-z0-9_-]+):[ ]*(#.*)?", line)
            if not match:
                raise Refused(EXIT_CANNOT_RUN, f"{where}: cannot read this line as a job")
            step = None
            job = Job(id=match.group(1), lineno=i + 1)
            jobs[job.id] = job
            in_steps = False
            i += 1
            continue
        if job is None:
            raise Refused(EXIT_CANNOT_RUN, f"{where}: a line before the first job")
        if indent == 4:
            match = re.fullmatch("    " + _KEY, line)
            if not match:
                raise Refused(EXIT_CANNOT_RUN, f"{where}: cannot read this line as a key of job `{job.id}`")
            key, inline = match.group(1), (match.group(2) or "").strip()
            step = None
            in_steps = key == "steps"
            if in_steps:
                if inline:
                    raise Refused(EXIT_CANNOT_RUN, f"{where}: `steps:` is not a block sequence")
                i += 1
            elif key == "if":
                values, i = _read_value(inline, lines, i, 4, where)
                job.condition = " ".join(v for v in values if v)
            elif key == "continue-on-error":
                values, i = _read_value(inline, lines, i, 4, where)
                job.continue_on_error = " ".join(values)
            elif key == "needs":
                if not inline or inline in BLOCK_SCALARS:
                    raise Refused(
                        EXIT_CANNOT_RUN,
                        f"{where}: job `{job.id}` writes `needs:` as a block; write it inline, "
                        "as `needs: job` or `needs: [a, b]`",
                    )
                values, i = _read_value(inline, lines, i, 4, where)
                value = values[0]
                if value.startswith("["):
                    if not value.endswith("]"):
                        raise Refused(EXIT_CANNOT_RUN, f"{where}: cannot read `needs: {value}`")
                    value = value[1:-1]
                job.needs = tuple(n.strip().strip("'\"") for n in value.split(",") if n.strip())
            elif key == "outputs":
                j = i + 1
                while j < len(lines):
                    nxt = lines[j]
                    if not _content(nxt):
                        j += 1
                        continue
                    if _indent(nxt) <= 4:
                        break
                    out = re.fullmatch("      " + _KEY, nxt)
                    if not out or not out.group(2):
                        raise Refused(EXIT_CANNOT_RUN, f"{rel}:{j + 1}: cannot read this line as an output of job `{job.id}`")
                    values, j = _read_value(out.group(2).strip(), lines, j, 6, f"{rel}:{j + 1}")
                    job.outputs[out.group(1)] = " ".join(values)
                i = j
            else:
                i = _skip_nested(lines, i, 4)
            continue
        if in_steps and line.startswith("      - "):
            step = Step(job=job.id, lineno=i + 1)
            steps.append(step)
            match = re.fullmatch(_KEY, line[8:])
            if not match:
                raise Refused(EXIT_CANNOT_RUN, f"{where}: cannot read this line as the start of a step")
            i = _apply_step_key(step, match.group(1), (match.group(2) or "").strip(), lines, i, where)
            continue
        if in_steps and step is not None and indent == 8:
            match = re.fullmatch("        " + _KEY, line)
            if not match:
                raise Refused(EXIT_CANNOT_RUN, f"{where}: cannot read this line as a key of a step")
            i = _apply_step_key(step, match.group(1), (match.group(2) or "").strip(), lines, i, where)
            continue
        raise Refused(EXIT_CANNOT_RUN, f"{where}: a line this reader does not recognise in job `{job.id}`")

    if not jobs:
        raise Refused(EXIT_CANNOT_RUN, f"{rel}: no jobs found")
    return lines, jobs, steps


def _logical(step: Step) -> list[str]:
    """A step's `run:` as command lines: comments dropped, continuations joined."""
    out: list[str] = []
    pending = ""
    for raw in step.commands:
        line = raw.strip()
        if not pending and (not line or line.startswith("#")):
            continue
        if line.endswith("\\"):
            pending += line[:-1].rstrip() + " "
            continue
        out.append(pending + line)
        pending = ""
    if pending:
        out.append(pending.rstrip())
    return out


def _calls_guard(line: str) -> bool:
    return bool(re.match(rf"^(python3 )?{re.escape(SCRIPT_REL)} ", line))


# ── the wiring ──────────────────────────────────────────────────────────────


def check_wiring(root: Path) -> None:
    lines, jobs, steps = scan_workflow(root, RELEASE_WORKFLOW)
    rel = RELEASE_WORKFLOW
    findings: list[str] = []

    filters = push_tag_filters(lines, rel)
    if filters != [TAG_FILTER]:
        findings.append(
            f"{rel}: the push tag filter is {filters or 'absent'}, not ['{TAG_FILTER}']; "
            "any other filter lets a tag that is not a version start a release."
        )

    guard = jobs.get(GUARD_JOB)
    if guard is None:
        findings.append(f"{rel}: no `{GUARD_JOB}` job")
    else:
        if guard.condition:
            findings.append(f"{rel}: job `{GUARD_JOB}` carries `if: {guard.condition}`")
        if guard.continue_on_error:
            findings.append(f"{rel}: job `{GUARD_JOB}` carries continue-on-error")
        for s in steps:
            if s.job == GUARD_JOB and s.condition:
                findings.append(f"{rel}:{s.lineno}: a step of `{GUARD_JOB}` carries `if: {s.condition}`")
        calls = [(s, line) for s in steps if s.job == GUARD_JOB for line in _logical(s) if _calls_guard(line)]
        if len(calls) != 1:
            findings.append(f"{rel}: job `{GUARD_JOB}` calls {SCRIPT_REL} {len(calls)} time(s), not once")
        else:
            step, line = calls[0]
            if step.continue_on_error:
                findings.append(f"{rel}:{step.lineno}: the guard step carries continue-on-error")
            if not line.endswith(GUARD_ARGS):
                findings.append(f"{rel}:{step.lineno}: the guard is not called with {GUARD_ARGS}")
            if step.step_id != GUARD_STEP_ID:
                findings.append(f"{rel}:{step.lineno}: the guard step's id is {step.step_id!r}, not {GUARD_STEP_ID!r}")
        if guard.outputs.get(NOTES_OUTPUT) != NOTES_JOB_OUTPUT:
            findings.append(f"{rel}: job `{GUARD_JOB}` does not output `{NOTES_OUTPUT}: {NOTES_JOB_OUTPUT}`")

    def needs_guard(job_id: str, seen: frozenset = frozenset()) -> bool:
        job = jobs.get(job_id)
        if job is None or job_id in seen:
            return False
        if GUARD_JOB in job.needs:
            return True
        return any(needs_guard(n, seen | {job_id}) for n in job.needs)

    for job_id, job in jobs.items():
        if job_id == GUARD_JOB:
            continue
        if not needs_guard(job_id):
            findings.append(f"{rel}: job `{job_id}` does not wait on `{GUARD_JOB}`, so it runs for a tag the guard refuses")
        # `needs:` stops a job only under the default `success()` condition. A
        # status function in its `if:` runs it after the guard has refused.
        if RUNS_AFTER_FAILURE.search(job.condition):
            findings.append(
                f"{rel}: job `{job_id}` carries `if: {job.condition}`, which runs it even when `{GUARD_JOB}` refuses the tag"
            )

    publishing = [s for s in steps if s.uses.startswith(PUBLISH_ACTION)]
    if len(publishing) != 1:
        findings.append(f"{rel}: {len(publishing)} step(s) use {PUBLISH_ACTION}, not one")
    else:
        publish = publishing[0]
        if GUARD_JOB not in jobs[publish.job].needs:
            findings.append(f"{rel}: job `{publish.job}` does not list `{GUARD_JOB}` in `needs:`, so it cannot read the guard's notes")
        body = publish.with_.get("body")
        if body != NOTES_BODY:
            findings.append(f"{rel}:{publish.lineno}: the release body is not `body: {NOTES_BODY}` (found {body!r})")
        if publish.with_.get("generate_release_notes", "false") != "false":
            findings.append(f"{rel}:{publish.lineno}: generate_release_notes is on, which adds GitHub's notes to the changelog section")
        if "body_path" in publish.with_:
            findings.append(f"{rel}:{publish.lineno}: `body_path:` overrides `body:`")
        if publish.with_.get("fail_on_unmatched_files") != "true":
            findings.append(f"{rel}:{publish.lineno}: fail_on_unmatched_files is not true, so a missing asset is skipped with a warning")

    if findings:
        raise Refused(EXIT_WIRING, "\n".join(findings))
    print("release wiring: tag filter, guard job, job graph and release body are as required.")


# ── the rehearsal ───────────────────────────────────────────────────────────


def _run_guard(lines: list[str], workdir: Path, tag: str) -> int:
    env = {**os.environ, "GITHUB_REF_NAME": tag, "GITHUB_OUTPUT": str(workdir / "github_output")}
    script = "set -euo pipefail\n" + "\n".join(lines) + "\n"
    proc = subprocess.run(["bash", "-c", script], cwd=workdir, env=env, capture_output=True, text=True)
    return proc.returncode


def check_rehearsal(root: Path) -> None:
    _, _, steps = scan_workflow(root, RELEASE_WORKFLOW)
    lines = [line for s in steps if s.job == GUARD_JOB and s.commands for line in _logical(s)]
    if not lines:
        raise Refused(EXIT_REHEARSAL, f"{RELEASE_WORKFLOW}: job `{GUARD_JOB}` runs nothing")
    version = package_version(root / MANIFEST_REL)
    if not TAG_RE.fullmatch(f"v{version}"):
        raise Refused(
            EXIT_CANNOT_RUN,
            f"{MANIFEST_REL}'s version `{version}` is not three dot-separated numbers, so no "
            "tag could release this tree and there is nothing to rehearse",
        )
    major, minor, patch = (int(p) for p in version.split("."))
    try:
        changelog = (root / CHANGELOG_REL).read_text(encoding="utf-8")
    except OSError as exc:
        raise Refused(EXIT_CANNOT_RUN, f"{CHANGELOG_REL}: {exc}") from None
    findings: list[str] = []

    def with_body(body: str | None) -> str:
        """This tree's changelog with the version's section replaced, or its heading renamed."""
        text = changelog.splitlines()
        bounds = _section_bounds(text, version)
        if bounds is None:
            return changelog
        if body is None:
            text[bounds[0] - 1] = "## [renamed]"
            return "\n".join(text) + "\n"
        return "\n".join(text[: bounds[0]] + ["", body, ""] + text[bounds[1] :]) + "\n"

    with tempfile.TemporaryDirectory() as tmp:
        base = Path(tmp)

        def tree(name: str, log: str = changelog) -> Path:
            work = base / name
            (work / "scripts").mkdir(parents=True)
            # The guard as it stands in the tree under check, so a rehearsal of
            # a mutated tree runs that tree's script, not this one.
            shutil.copy(root / SCRIPT_REL, work / SCRIPT_REL)
            shutil.copy(root / MANIFEST_REL, work / MANIFEST_REL)
            (work / CHANGELOG_REL).write_text(log, encoding="utf-8")
            return work

        good = tree("good")
        rc = _run_guard(lines, good, f"v{version}")
        notes = read_output(good / "github_output")
        if rc != 0:
            findings.append(f"v{version} (this tree's version) was refused with exit {rc}")
        elif notes is None:
            findings.append(f"v{version} passed but the step set no `{NOTES_OUTPUT}` output")
        elif notes != changelog_section(root / CHANGELOG_REL, version):
            findings.append(f"the `{NOTES_OUTPUT}` output is not the `## [{version}]` section")

        refusals = (
            ("a tag that is not a version", "v19-train-state", tree("form"), EXIT_TAG_FORM),
            ("a version the manifest does not carry", f"v{major}.{minor}.{patch + 1}", tree("manifest"), EXIT_MANIFEST),
            ("a version with no changelog heading", f"v{version}", tree("heading", with_body(None)), EXIT_NO_HEADING),
            ("a version with an empty section", f"v{version}", tree("empty", with_body("")), EXIT_EMPTY_SECTION),
            ("a section over the length cap", f"v{version}", tree("long", with_body("x" * (NOTES_LIMIT + 1))), EXIT_EMPTY_SECTION),
        )
        for label, tag, work, want in refusals:
            rc = _run_guard(lines, work, tag)
            if rc != want:
                findings.append(f"{label} ({tag}): the guard step exited {rc}, not {want}")
            if read_output(work / "github_output") is not None:
                findings.append(f"{label} ({tag}): the guard step set `{NOTES_OUTPUT}` anyway")

    if findings:
        raise Refused(EXIT_REHEARSAL, "\n".join(f"{RELEASE_WORKFLOW} `{GUARD_JOB}`: {f}" for f in findings))
    print(f"release rehearsal: `{GUARD_JOB}` passes v{version} and refuses the five bad cases.")


# ── self-test ───────────────────────────────────────────────────────────────


def self_test() -> int:
    passed = 0
    failed: list[str] = []

    def expect(label: str, fn, want: int, says: str | None = None) -> None:
        nonlocal passed
        message = ""
        try:
            with contextlib.redirect_stdout(io.StringIO()):
                fn()
            got = 0
        except Refused as exc:
            got, message = exc.code, str(exc)
        except Exception as exc:  # noqa: BLE001 -- a case that raises is a failure
            failed.append(f"{label}: raised {type(exc).__name__}: {exc}")
            return
        if got != want:
            failed.append(f"{label}: exit {got}, wanted {want}" + (f" ({message})" if message else ""))
        elif says is not None and says not in message:
            failed.append(f"{label}: exit {got} as wanted, but the message does not say {says!r}: {message}")
        else:
            passed += 1

    manifest = (
        '[package]\nname = "arcform"\nversion = "0.2.0"\nedition = "2024"\n\n'
        '[dependencies]\nsqlparser = { package = "arcform-sqlparser", version = "=0.55.0" }\n'
    )
    changelog = (
        "# Changelog\n\n## [Unreleased]\n\n## [0.2.0] - 2026-10-14\n\n### Fixed\n\n- A thing.\n\n"
        "## [0.1.0] - 2026-03-31\n\n- Older.\n"
    )

    def with_section(body: str) -> str:
        return f"# Changelog\n\n## [0.2.0] - 2026-10-14\n\n{body}\n\n## [0.1.0]\n\n- Older.\n"

    with tempfile.TemporaryDirectory() as tmp:
        base = Path(tmp)

        def tag_tree(name: str, cargo: str = manifest, log: str = changelog) -> Path:
            root = base / name
            root.mkdir()
            (root / MANIFEST_REL).write_text(cargo, encoding="utf-8")
            (root / CHANGELOG_REL).write_text(log, encoding="utf-8")
            return root

        # exit 0, and the notes are exactly the section, appended to the file.
        good = tag_tree("good")
        output = base / "github_output"
        output.write_text("earlier=1\n", encoding="utf-8")
        expect("the manifest's version with a changelog entry", lambda: check_tag(good, "v0.2.0", output), 0)
        try:
            notes_ok = read_output(output) == "### Fixed\n\n- A thing.\n" and output.read_text().startswith("earlier=1\n")
        except Exception:  # noqa: BLE001
            notes_ok = False
        if notes_ok:
            passed += 1
        else:
            failed.append("notes: not exactly the 0.2.0 section, appended to the output file")

        # exit 3: the tag's shape.
        for bad in ("v19-train-state", "v0.2", "0.2.0", "v0.2.0-rc1", "v0.2.0.1", "vX.Y.Z", "v0.2.0\n"):
            expect(f"tag form {bad!r}", lambda bad=bad: check_tag(good, bad, None), EXIT_TAG_FORM)

        # exit 4: the tag's number against [package] version.
        expect("v0.2.1 against 0.2.0", lambda: check_tag(good, "v0.2.1", None), EXIT_MANIFEST)
        workspace = tag_tree("workspace", cargo='[workspace.package]\nversion = "0.3.0"\n\n' + manifest)
        expect("a [workspace.package] version is not the package's", lambda: check_tag(workspace, "v0.3.0", None), EXIT_MANIFEST)

        # exit 5: no heading.
        no_heading = tag_tree("no-heading", cargo=manifest.replace('"0.2.0"', '"0.2.1"'))
        expect("no changelog heading", lambda: check_tag(no_heading, "v0.2.1", None), EXIT_NO_HEADING)
        unreleased_only = tag_tree("unreleased-only", log="# Changelog\n\n## [Unreleased]\n\n- Not rolled yet.\n")
        expect("only [Unreleased], not rolled", lambda: check_tag(unreleased_only, "v0.2.0", None), EXIT_NO_HEADING)

        # exit 6: an empty section, and one too long to publish whole.
        empty = tag_tree("empty", log="## [0.2.0] - 2026-10-14\n\n## [0.1.0]\n\n- Older.\n")
        expect("an empty changelog section", lambda: check_tag(empty, "v0.2.0", None), EXIT_EMPTY_SECTION)
        out_long = base / "out-long"
        too_long = tag_tree("too-long", log=with_section("x" * (NOTES_LIMIT + 1)))
        expect(
            "a 120,001-character section",
            lambda: check_tag(too_long, "v0.2.0", out_long),
            EXIT_EMPTY_SECTION,
            says="120,001",
        )
        if out_long.exists():
            failed.append("a refused section was still written to the output file")
        else:
            passed += 1
        at_limit = tag_tree("at-limit", log=with_section("x" * NOTES_LIMIT))
        expect("a section of exactly 120,000 characters", lambda: check_tag(at_limit, "v0.2.0", None), 0)
        last = tag_tree("last", log="## [0.2.0]\n\n- Only entry.\n")
        expect("the last section, undated", lambda: check_tag(last, "v0.2.0", None), 0)

        # exit 2: the manifest cannot be read.
        no_manifest = base / "no-manifest"
        no_manifest.mkdir()
        expect("no Cargo.toml", lambda: check_tag(no_manifest, "v0.2.0", None), EXIT_CANNOT_RUN)
        broken = tag_tree("broken-toml", cargo='[package\nversion = "0.2.0"\n')
        expect("Cargo.toml that is not TOML", lambda: check_tag(broken, "v0.2.0", None), EXIT_CANNOT_RUN)
        no_version = tag_tree("no-version", cargo='[package]\nname = "arcform"\nversion.workspace = true\n')
        expect("no literal [package] version", lambda: check_tag(no_version, "v0.2.0", None), EXIT_CANNOT_RUN)

        # The wiring and the rehearsal, against a copy of this tree's workflow
        # with one defect each. The unmutated copy must pass both, or a
        # mutation reddening proves nothing.
        def wired_tree(name: str, edit=None) -> Path:
            root = base / ("w-" + re.sub(r"[^A-Za-z0-9]+", "-", name))
            for rel in (RELEASE_WORKFLOW, SCRIPT_REL, MANIFEST_REL, CHANGELOG_REL):
                (root / rel).parent.mkdir(parents=True, exist_ok=True)
                shutil.copy(ROOT / rel, root / rel)
            if edit is not None:
                rel, old, new = edit
                text = (root / rel).read_text(encoding="utf-8")
                if old not in text:
                    raise AssertionError(f"mutation `{name}` found nothing to change in {rel}")
                (root / rel).write_text(text.replace(old, new, 1), encoding="utf-8")
            return root

        def mutated(label: str, edit, run, want: int) -> None:
            try:
                tree = wired_tree(label, edit)
            except (AssertionError, OSError) as exc:
                failed.append(f"{label}: {exc}")
                return
            expect(label, lambda: run(tree), want)

        mutated("wiring: this tree", None, check_wiring, 0)
        mutated("rehearsal: this tree", None, check_rehearsal, 0)
        mutated(
            "rehearsal: a package version that is not three numbers",
            (MANIFEST_REL, '\nversion = "', '\nversion = "9.9.9-rc1"\nold_version = "'),
            check_rehearsal,
            EXIT_CANNOT_RUN,
        )

        guard_call = f"python3 {SCRIPT_REL} {GUARD_ARGS}"
        build_needs = f"    needs: {GUARD_JOB}\n    runs-on: ${{{{ matrix.os }}}}"
        wiring_cases = (
            ("the filter loosened to v*", (RELEASE_WORKFLOW, f'tags: ["{TAG_FILTER}"]', 'tags: ["v*"]')),
            ("a second, looser filter", (RELEASE_WORKFLOW, f'tags: ["{TAG_FILTER}"]', f'tags: ["{TAG_FILTER}", "v*"]')),
            ("the filter without its pluses", (RELEASE_WORKFLOW, f'tags: ["{TAG_FILTER}"]', 'tags: ["v[0-9].[0-9].[0-9]"]')),
            ("build no longer needs tag-guard", (RELEASE_WORKFLOW, build_needs, "    runs-on: ${{ matrix.os }}")),
            ("release stops listing tag-guard", (RELEASE_WORKFLOW, f"    needs: [{GUARD_JOB}, build]", "    needs: [build]")),
            ("publish-crates made to run after a failure", (RELEASE_WORKFLOW, "    needs: release\n", "    needs: release\n    if: ${{ always() }}\n")),
            ("build made to run unless cancelled, in capitals", (RELEASE_WORKFLOW, build_needs, f"{build_needs}\n    if: ${{{{ !Cancelled() }}}}")),
            ("update-install-site made to run on failure", (RELEASE_WORKFLOW, "    name: Update the install site\n    needs: release\n", "    name: Update the install site\n    needs: release\n    if: failure() || success()\n")),
            ("the body is something else", (RELEASE_WORKFLOW, f"          body: {NOTES_BODY}", "          body: ${{ github.event.head_commit.message }}")),
            ("generated notes switched on", (RELEASE_WORKFLOW, f"          body: {NOTES_BODY}", f"          body: {NOTES_BODY}\n          generate_release_notes: true")),
            ("body_path overrides the body", (RELEASE_WORKFLOW, f"          body: {NOTES_BODY}", f"          body: {NOTES_BODY}\n          body_path: CHANGELOG.md")),
            ("unmatched files allowed", (RELEASE_WORKFLOW, "          fail_on_unmatched_files: true", "          fail_on_unmatched_files: false")),
            ("the guard step made conditional", (RELEASE_WORKFLOW, f"        run: {guard_call}", f"        if: false\n        run: {guard_call}")),
            ("the guard step made advisory", (RELEASE_WORKFLOW, f"        run: {guard_call}", f"        continue-on-error: true\n        run: {guard_call}")),
            ("the guard job made conditional", (RELEASE_WORKFLOW, "    timeout-minutes: 5\n    outputs:", "    timeout-minutes: 5\n    if: github.ref_type == 'tag'\n    outputs:")),
            ("the guard call commented out", (RELEASE_WORKFLOW, f"        run: {guard_call}", f"        run: echo skipped # {guard_call}")),
            ("the guard step's id changed", (RELEASE_WORKFLOW, f"        id: {GUARD_STEP_ID}\n", "        id: check\n")),
            ("the guard's job output renamed", (RELEASE_WORKFLOW, f"      {NOTES_OUTPUT}: {NOTES_JOB_OUTPUT}", f"      body: {NOTES_JOB_OUTPUT}")),
        )
        for label, edit in wiring_cases:
            mutated(f"wiring: {label}", edit, check_wiring, EXIT_WIRING)

        cannot_read = (
            ("needs written as a block sequence", (RELEASE_WORKFLOW, build_needs, f"    needs:\n      - {GUARD_JOB}\n    runs-on: ${{{{ matrix.os }}}}")),
            ("the trigger written inline", (RELEASE_WORKFLOW, "on:\n  push:\n", "on: push\nx-on:\n  push:\n")),
            ("a tab in the indentation", (RELEASE_WORKFLOW, build_needs, f"\tneeds: {GUARD_JOB}\n    runs-on: ${{{{ matrix.os }}}}")),
            ("a job written as a flow mapping", (RELEASE_WORKFLOW, "\n  build:\n", "\n  build: {}\n  build-steps:\n")),
            ("an if: continued onto a second line", (RELEASE_WORKFLOW, "    needs: release\n", "    needs: release\n    if: success() &&\n      true\n")),
        )
        for label, edit in cannot_read:
            mutated(f"wiring: {label}", edit, check_wiring, EXIT_CANNOT_RUN)

        rehearsal_cases = (
            ("the guard call replaced by `true`", (RELEASE_WORKFLOW, f"        run: {guard_call}", f"        run: true # {guard_call}")),
            ("the guard's notes sent nowhere", (RELEASE_WORKFLOW, '--github-output "${GITHUB_OUTPUT}"', "--github-output /dev/null")),
            ("the guard's manifest check removed", (SCRIPT_REL, "    if version != manifest_version:", "    if False:")),
            ("the guard's length cap removed", (SCRIPT_REL, "    if len(body) > NOTES_LIMIT:", "    if False:")),
        )
        for label, edit in rehearsal_cases:
            mutated(f"rehearsal: {label}", edit, check_rehearsal, EXIT_REHEARSAL)

    print(f"self-test: {passed} passed, {len(failed)} failed")
    for f in failed:
        print(f"  FAILED {f}", file=sys.stderr)
    return 1 if failed else 0


def main() -> int:
    parser = argparse.ArgumentParser(description=(__doc__ or "").split("\n")[0])
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--tag")
    mode.add_argument("--wiring", action="store_true")
    mode.add_argument("--release-rehearsal", action="store_true")
    mode.add_argument("--self-test", action="store_true")
    parser.add_argument("--github-output", type=Path)
    parser.add_argument("--root", type=Path, default=None)
    args = parser.parse_args()

    if args.self_test:
        return self_test()
    # --tag runs from the working directory, as the release job runs it; the
    # other two read the repository this script lives in unless told otherwise.
    root = args.root or (Path.cwd() if args.tag is not None else ROOT)
    try:
        if args.tag is not None:
            check_tag(root, args.tag, args.github_output)
        elif args.wiring:
            check_wiring(root)
        else:
            check_rehearsal(root)
    except Refused as exc:
        print(f"error: {exc}", file=sys.stderr)
        return exc.code
    return 0


if __name__ == "__main__":
    sys.exit(main())
