#!/usr/bin/env python3
"""Report code a pull request changed that no test would notice being broken.

The question this answers is not "how many lines did the tests execute" — a line
coverage number goes green on a test that calls a function and asserts nothing
about it.  It is: *if I break this line on purpose, does anything go red?*  A
line where the answer is no is a line the suite is not holding.

WHAT IT DOES.  Takes the lines this branch added or changed under `src/`, rewrites
each one in a way that changes what the program does, and runs the test suite
against each rewrite.  A rewrite the suite still passes is reported by the file,
the function, and the exact before/after text.

WHY THE MUTATIONS ARE THE SHAPE THEY ARE — this is the load-bearing design
choice and it was made from measurement, not from a catalogue.  The obvious
operator, and the one a general-purpose Rust mutation tool leads with, is
*replace a function's whole body with a trivial value*.  Against this repo's own
history that operator is close to useless: the holes this gate exists to catch
were all sub-function, inside functions that several tests already drove.
`hash_directory_contents` is the worked example.  Replacing its whole body with a
constant reddens two of its own unit tests, so a whole-body tool reports the
function covered — while the mutation that actually shipped (hash the directory's
filenames and not their contents) left the entire suite green and reintroduced a
silent-skip defect.  So the operators here work at the granularity the defects
did: a guard's condition, one call inside an expression, a `?` that stops
propagating, a map insert that stops keeping the first write.

The operators, and the failure each is modelled on:

  GUARD_FALSE / GUARD_TRUE   an `if` condition pinned to a literal — deletes a
                             guard, or makes it fire always.  Two shipped builds
                             had a directory-skip guard that could be deleted
                             with the whole suite green, and a third had a
                             recursive descent that could be switched off the
                             same way.
  CALL_DEFAULT               one call inside a larger expression replaced by
                             `Default::default()`.  This is the one that catches
                             "the value is computed and then not really used".
  TRY_DEFAULT                `expr?` becomes `expr.unwrap_or_default()`, so a
                             failure stops propagating and folds into a stable
                             value instead.  A missing artifact that hashes to a
                             constant compares equal to itself on the next run.
  ENTRY_OVERWRITE            `m.entry(k).or_insert(v)` becomes `m.insert(k, v)`,
                             so a later, worse-informed write wins over an
                             earlier, better-informed one.
  CMP_FLIP / ORD_RELAX /     token swaps: `==`/`!=`, `<`/`<=`, `&&`/`||`, a
  LOGIC_FLIP / NOT_DROP /    dropped `!`, a flipped bool literal.
  BOOL_LIT_FLIP
  FN_BODY_DEFAULT            the whole-body replacement, kept because a function
                             the suite never calls at all should still be named.

THREE TIERS IN THE REPORT, and the split is the point.  A real codebase produces
surviving mutations that are *equivalent* — the code cannot be reached in
production, or the only thing that changes is a warning's wording.  Listing those
next to a genuine hole is how a report becomes wallpaper.  So every survivor is
probed a second time with a `panic!` at the same span:

  UNPINNED    the panic fired, so the tests DO execute this line — and none of
              them noticed the behaviour change.  This is the tier to act on.
  UNREACHED   the panic did not fire: no test executes this line at all.  Either
              a test is missing outright, or the code is genuinely unreachable
              and the survivor is equivalent.  Either way it is a different
              question from the tier above.
  EQUIVALENT  ruled a non-defect by a human, in
              scripts/mutation-coverage-equivalents.txt, with a reason and a
              date.  Keyed on the mutation's content, not its line number, so
              editing the file above it does not silently unrule it.

The panic probe splits the "unreachable in production" half of that automatically
and cannot split the "observable only in a warning" half — that one needs a
ruling, which is what the registry is for.

BLOCKING, DELIBERATELY.  Exit 1 on an UNPINNED survivor fails the job.  This is a
change in kind from a report-only check and it is the whole reason the check is
worth having: the failure it exists to stop is a mechanism arriving with no test
that catches its removal, and a finding nobody has to act on is a finding that
gets read past.  The escape hatch is not a flag but a ruling — one line in the
equivalents registry, naming the reason and the date, which stays in the tree and
can be argued with.  UNREACHED does not block on its own (see --strict).

Exit codes: 0 clean · 1 at least one UNPINNED survivor · 2 usage · 3 the harness
could not run (no baseline, dirty tree, cargo missing).
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time
from dataclasses import dataclass, field
from pathlib import Path

# --------------------------------------------------------------------------- #
# Constants
# --------------------------------------------------------------------------- #

PROBE_MESSAGE = "mutation-coverage reachability probe"
DEFAULT_BUDGET_SECONDS = 600
DEFAULT_MAX_MUTANTS = 60
DEFAULT_TIMEOUT_SECONDS = 900

# Calls that are already trivial: replacing them with `Default::default()` is
# either a no-op or a compile error, so they cost a build and tell you nothing.
TRIVIAL_CALLEES = {
    "default",
    "Default::default",
    "String::new",
    "Vec::new",
    "new",
    "clone",
    "to_string",
    "to_owned",
    "into",
    "as_str",
    "as_ref",
    "unwrap_or_default",
}


# --------------------------------------------------------------------------- #
# Source scanning helpers
# --------------------------------------------------------------------------- #


def code_mask(line: str, in_block_comment: bool = False) -> tuple[list[bool], bool]:
    """Positions in `line` that are real code, and whether a block comment is open.

    False for anything inside a string literal, a char literal, a `//` comment or
    a `/* */` comment.  Every operator below consults this before matching, so a
    `==` inside a doc comment or an `if` inside a string is never mutated.
    """
    mask = [False] * len(line)
    i = 0
    n = len(line)
    while i < n:
        if in_block_comment:
            if line.startswith("*/", i):
                in_block_comment = False
                i += 2
            else:
                i += 1
            continue
        c = line[i]
        if line.startswith("//", i):
            break
        if line.startswith("/*", i):
            in_block_comment = True
            i += 2
            continue
        if c == '"':
            # Raw strings (r"..", r#".."#) end differently; treat the rest of the
            # line as non-code, which is conservative in the safe direction.
            if i >= 1 and line[i - 1] == "r":
                break
            i += 1
            while i < n:
                if line[i] == "\\":
                    i += 2
                    continue
                if line[i] == '"':
                    i += 1
                    break
                i += 1
            continue
        if c == "'":
            # A lifetime (`'a`) is code; a char literal is not.  Both start the
            # same way, so look for the closing quote within four characters.
            close = line.find("'", i + 1)
            if close != -1 and close - i <= 4:
                i = close + 1
                continue
            mask[i] = True
            i += 1
            continue
        mask[i] = True
        i += 1
    return mask, in_block_comment


def masks_for(lines: list[str]) -> list[list[bool]]:
    out = []
    open_block = False
    for line in lines:
        mask, open_block = code_mask(line, open_block)
        out.append(mask)
    return out


CFG_ATTR = re.compile(r"^\s*#\[cfg\((?P<pred>.*)\)\]\s*$")
CFG_FEATURE = re.compile(r'feature\s*=\s*"(?P<name>[A-Za-z0-9_.\-]+)"')


def cfg_conditions(predicate: str) -> set[str]:
    """What a `#[cfg(...)]` predicate requires: `test`, and `feature:<name>`.

    `not(...)` anywhere in the predicate returns nothing at all.  Under a negation
    the sense inverts — enabling the named feature REMOVES the code — and a
    requirement read the wrong way round is worse than no requirement.
    """
    if "not(" in predicate:
        return set()
    found = {f"feature:{m.group('name')}" for m in CFG_FEATURE.finditer(predicate)}
    if re.search(r"\btest\b", predicate):
        found.add("test")
    return found


def cfg_regions(lines: list[str], masks: list[list[bool]]) -> dict[int, set[str]]:
    """0-based line index -> the cfg conditions of every item it sits inside.

    Two things depend on this and both were wrong when it only matched the exact
    string `#[cfg(test)]`.

    Mutating a test is pointless — the mutant either fails or does not, and
    either way it says nothing about whether production code is held.  But test
    code is not always spelled `#[cfg(test)]`: `operator.rs` has a whole test
    module behind `#[cfg(all(test, feature = "mcp"))]`, and an exact-string match
    offered every line of it as a candidate.

    And code behind a feature the test command does not enable is not compiled at
    all, so mutating it changes nothing, the suite stays green, and the report
    says the tests do not reach it.  That is true and useless: the tests may cover
    it perfectly well with the feature on.  Knowing which feature guards a changed
    line is what lets the run turn it on.

    `#[cfg(test)]` guards a `mod` in most files here and a bare `fn` in at least
    one (`runner.rs`'s parameterless `run`), and several files have more than one,
    not all at the end, so this brace-matches each item rather than assuming the
    rest of the file is tests.
    """
    marked: dict[int, set[str]] = {}
    for i, line in enumerate(lines):
        m = CFG_ATTR.match(line)
        if not m:
            continue
        conditions = cfg_conditions(m.group("pred"))
        if not conditions:
            continue
        # Walk forward to the item's opening brace, skipping further attributes
        # and doc comments.
        depth = 0
        started = False
        j = i
        while j < len(lines):
            for pos, ch in enumerate(lines[j]):
                if not masks[j][pos]:
                    continue
                if ch == "{":
                    depth += 1
                    started = True
                elif ch == "}":
                    depth -= 1
            marked.setdefault(j, set()).update(conditions)
            if started and depth <= 0:
                break
            if not started and lines[j].rstrip().endswith(";"):
                break  # an item with no body
            j += 1
    return marked


def test_region(lines: list[str], masks: list[list[bool]]) -> set[int]:
    """0-based line indices belonging to a test item, however its cfg is spelled."""
    return {i for i, conditions in cfg_regions(lines, masks).items() if "test" in conditions}


def enclosing_function(lines: list[str], index: int) -> str:
    """The name of the `fn` a line sits inside, for the report's benefit."""
    pattern = re.compile(r"\bfn\s+([A-Za-z_][A-Za-z0-9_]*)")
    for i in range(index, -1, -1):
        stripped = lines[i].lstrip()
        if stripped.startswith("//"):
            continue
        m = pattern.search(lines[i])
        if m:
            return m.group(1)
    return "<file scope>"


def match_paren(lines: list[str], masks: list[list[bool]], li: int, ci: int) -> tuple[int, int] | None:
    """Given the position of an opening `(`, return the position of its match."""
    depth = 0
    i, j = li, ci
    while i < len(lines):
        start = j if i == li else 0
        for pos in range(start, len(lines[i])):
            if not masks[i][pos]:
                continue
            ch = lines[i][pos]
            if ch == "(":
                depth += 1
            elif ch == ")":
                depth -= 1
                if depth == 0:
                    return (i, pos)
        i += 1
        j = 0
    return None


# --------------------------------------------------------------------------- #
# Mutants
# --------------------------------------------------------------------------- #


@dataclass
class Mutant:
    path: str
    start: int  # 0-based, inclusive
    end: int  # 0-based, inclusive
    operator: str
    before: str  # exact original text of the span, newline-terminated
    after: str  # replacement text, newline-terminated
    probe: str | None  # replacement that panics, or None if none can be built
    function: str
    verdict: str = ""
    detail: str = ""
    reached: str = ""  # "yes" | "no" | "not probed"
    key: str = field(default="", init=False)

    def __post_init__(self) -> None:
        self.key = mutation_key(self.path, self.function, self.operator, self.before, self.after)

    def one_line_before(self) -> str:
        return " ".join(self.before.split())

    def one_line_after(self) -> str:
        return " ".join(self.after.split())


def mutation_key(path: str, function: str, operator: str, before: str, after: str) -> str:
    """A line-number-independent identity for one mutation.

    Keyed on the text rather than the position so that editing the file above a
    ruled-equivalent mutation does not silently unrule it — and so that changing
    the mutated code itself DOES, which is the direction you want.
    """
    payload = "\0".join(
        [path, function, operator, " ".join(before.split()), " ".join(after.split())]
    )
    return hashlib.sha256(payload.encode("utf-8")).hexdigest()[:16]


# --------------------------------------------------------------------------- #
# Operators
# --------------------------------------------------------------------------- #


def _block_open_span(lines: list[str], masks: list[list[bool]], i: int) -> int | None:
    """The last line of a condition whose block opens with `{`, starting at line i.

    An `if` condition here is often three lines of method chain with the brace on
    its own line, which is why this is a span rather than a single line.
    """
    for j in range(i, min(i + 8, len(lines))):
        text = lines[j].rstrip()
        # Only accept a `{` that is real code and is the last thing on the line.
        if not text.endswith("{"):
            continue
        pos = len(text) - 1
        if pos < len(masks[j]) and masks[j][pos]:
            return j
    return None


def op_guard(lines, masks, i, path, fn) -> list[Mutant]:
    stripped = lines[i].lstrip()
    indent = lines[i][: len(lines[i]) - len(stripped)]
    for prefix in ("} else if ", "if "):
        if stripped.startswith(prefix):
            break
    else:
        return []
    condition_head = stripped[len(prefix) :]
    # `if let` / `while let` bind a pattern; a bool literal cannot replace them.
    if condition_head.startswith("let "):
        return []
    end = _block_open_span(lines, masks, i)
    if end is None:
        return []
    before = "".join(lines[i : end + 1])
    if condition_head.strip() in ("true {", "false {"):
        return []
    out = []
    for literal in ("false", "true"):
        out.append(
            Mutant(
                path=path,
                start=i,
                end=end,
                operator=f"GUARD_{literal.upper()}",
                before=before,
                after=f"{indent}{prefix}{literal} {{\n",
                probe=f'{indent}{prefix}panic!("{PROBE_MESSAGE}") {{\n',
                function=fn,
            )
        )
    return out


def _statement_probe(lines: list[str], i: int) -> str:
    """A `panic!` immediately before line `i`, keeping the line itself intact.

    Deliberately not "replace the expression with `panic!`" — that needs the
    expression's left edge, which for a method chain cannot be found by looking
    at text.  Prepending a statement needs nothing but the indent, and where the
    line is not in statement position the probe simply fails to compile and the
    survivor is reported as unprobed rather than mis-tiered.
    """
    indent = lines[i][: len(lines[i]) - len(lines[i].lstrip())]
    return f'{indent}panic!("{PROBE_MESSAGE}");\n{lines[i]}'


def _macro_spans(line: str, mask: list[bool]) -> list[tuple[int, int]]:
    spans = []
    for m in re.finditer(r"[A-Za-z_][A-Za-z0-9_]*!\s*[\(\[]", line):
        if mask[m.end() - 1]:
            spans.append((m.start(), len(line)))
    return spans


def op_call_default(lines, masks, i, path, fn) -> list[Mutant]:
    """Replace a call that is a sub-expression with `Default::default()`.

    Restricted to calls nested inside another call: the point is to blank one
    computed value while leaving the surrounding statement intact, which is the
    shape of the defect that hashed a directory's filenames and not its contents.
    Replacing the OUTER call instead deletes the whole statement, which the tests
    generally do notice — so it is the weaker mutation and it is not generated.
    """
    line = lines[i]
    mask = masks[i]
    macros = _macro_spans(line, mask)
    calls: list[tuple[int, int, str]] = []  # (start_col, end_col, callee)
    for m in re.finditer(r"([A-Za-z_][A-Za-z0-9_]*(?:::[A-Za-z_][A-Za-z0-9_]*)*)\s*\(", line):
        open_col = m.end() - 1
        if open_col >= len(mask) or not mask[open_col]:
            continue
        callee = m.group(1)
        if callee in TRIVIAL_CALLEES or callee.split("::")[-1] in TRIVIAL_CALLEES:
            continue
        if line[m.start() - 1 : m.start()] == "!":
            continue  # a macro's name
        close = match_paren(lines, masks, i, open_col)
        if close is None or close[0] != i:
            continue  # multi-line call: leave it to the coarser operators
        start_col = m.start()
        if line[start_col - 1 : start_col] == ".":
            # A method call.  Replacing `recv.method(..)` means replacing the
            # receiver too, or the mutant reads `recv.Default::default()`.  Only a
            # plain identifier receiver can be found reliably from text; anything
            # chained is left alone rather than mangled.
            j = start_col - 1
            k = j - 1
            while k >= 0 and (line[k].isalnum() or line[k] == "_"):
                k -= 1
            if k == j - 1 or (k >= 0 and line[k] in ".)]?"):
                continue
            start_col = k + 1
        calls.append((start_col, close[1], callee))
    if len(calls) < 2:
        return []
    nested = [
        (s, e)
        for s, e, _ in calls
        if any(os_ < s and e < oe for os_, oe, _ in calls)
        and not any(ms <= s < me for ms, me in macros)
    ]
    out = []
    for start_col, end_col in nested[:2]:
        after = line[:start_col] + "Default::default()" + line[end_col + 1 :]
        out.append(
            Mutant(
                path=path,
                start=i,
                end=i,
                operator="CALL_DEFAULT",
                before=line,
                after=after,
                probe=_statement_probe(lines, i),
                function=fn,
            )
        )
    return out


def op_try_default(lines, masks, i, path, fn) -> list[Mutant]:
    """`expr?` stops propagating and folds into a default instead.

    The failure this models: a `None` that used to force unconditional staleness
    becomes a stable value that compares equal to itself on the next run, so the
    step settles over an artifact that is not there.
    """
    line = lines[i]
    mask = masks[i]
    for m in re.finditer(r"\)\?", line):
        pos = m.start() + 1
        if pos >= len(mask) or not mask[pos]:
            continue
        after = line[:pos] + ".unwrap_or_default()" + line[pos + 1 :]
        return [
            Mutant(
                path=path,
                start=i,
                end=i,
                operator="TRY_DEFAULT",
                before=line,
                after=after,
                probe=_statement_probe(lines, i),
                function=fn,
            )
        ]
    return []


def op_entry_overwrite(lines, masks, i, path, fn) -> list[Mutant]:
    """`m.entry(k).or_insert(v)` becomes `m.insert(k, v)`: last write wins.

    Modelled on a real survivor.  Two phases record the same key, the first with
    a parsed, source-derived answer and the second with a guess; `or_insert` keeps
    the first, `insert` keeps the guess.
    """
    line = lines[i]
    mask = masks[i]
    m = re.search(r"\.entry\s*\(", line)
    if not m or not mask[m.end() - 1]:
        return []
    close = match_paren(lines, masks, i, m.end() - 1)
    if close is None or close[0] != i:
        return []
    key_expr = line[m.end() : close[1]]
    rest = line[close[1] + 1 :]
    m2 = re.match(r"\s*\.(or_insert|or_default|or_insert_with)\s*\(", rest)
    if not m2:
        return []
    call_open = close[1] + 1 + m2.end() - 1
    close2 = match_paren(lines, masks, i, call_open)
    if close2 is None or close2[0] != i:
        return []
    value_expr = line[call_open + 1 : close2[1]]
    if m2.group(1) == "or_default":
        value_expr = "Default::default()"
    elif m2.group(1) == "or_insert_with":
        cl = re.match(r"\s*\|\|\s*(.*)$", value_expr)
        if not cl:
            return []
        value_expr = cl.group(1)
    receiver = line[: m.start()]
    tail = line[close2[1] + 1 :]
    after = f"{receiver}.insert({key_expr}, {value_expr}){tail}"
    return [
        Mutant(
            path=path,
            start=i,
            end=i,
            operator="ENTRY_OVERWRITE",
            before=line,
            after=after,
            probe=_statement_probe(lines, i),
            function=fn,
        )
    ]


TOKEN_SWAPS: list[tuple[str, str, str]] = [
    ("CMP_FLIP", " == ", " != "),
    ("CMP_FLIP", " != ", " == "),
    ("ORD_RELAX", " < ", " <= "),
    ("ORD_RELAX", " > ", " >= "),
    ("ORD_RELAX", " <= ", " < "),
    ("ORD_RELAX", " >= ", " > "),
    ("LOGIC_FLIP", " && ", " || "),
    ("LOGIC_FLIP", " || ", " && "),
]


def op_token_swap(lines, masks, i, path, fn) -> list[Mutant]:
    line = lines[i]
    mask = masks[i]
    out = []
    for operator, needle, replacement in TOKEN_SWAPS:
        pos = line.find(needle)
        if pos == -1 or not all(mask[pos : pos + len(needle)]):
            continue
        after = line[:pos] + replacement + line[pos + len(needle) :]
        out.append(
            Mutant(
                path=path,
                start=i,
                end=i,
                operator=operator,
                before=line,
                after=after,
                probe=None,
                function=fn,
            )
        )
        break
    for m in re.finditer(r"\b(true|false)\b", line):
        if not all(mask[m.start() : m.end()]):
            continue
        flipped = "false" if m.group(1) == "true" else "true"
        out.append(
            Mutant(
                path=path,
                start=i,
                end=i,
                operator="BOOL_LIT_FLIP",
                before=line,
                after=line[: m.start()] + flipped + line[m.end() :],
                probe=None,
                function=fn,
            )
        )
        break
    return out


def op_fn_body_default(lines, masks, i, path, fn) -> list[Mutant]:
    """Replace a whole function body with a trivial value.

    Kept for the case it is genuinely good at — a function no test calls at all —
    and NOT relied on for the rest.  Every historical survivor in this repo lived
    inside a function that several tests already drove, where this operator is
    killed at a granularity coarser than the defect and reports the function
    covered.  See the module docstring.
    """
    stripped = lines[i].lstrip()
    if not re.match(r"(pub(\([^)]*\))?\s+)?(async\s+)?(unsafe\s+)?fn\s", stripped):
        return []
    open_line = None
    for j in range(i, min(i + 12, len(lines))):
        text = lines[j].rstrip()
        if text.endswith("{") and masks[j][len(text) - 1]:
            open_line = j
            break
        if text.endswith(";"):
            return []  # a trait signature with no body
    if open_line is None:
        return []
    depth = 0
    close_line = None
    for j in range(open_line, len(lines)):
        for pos, ch in enumerate(lines[j]):
            if not masks[j][pos]:
                continue
            if ch == "{":
                depth += 1
            elif ch == "}":
                depth -= 1
                if depth == 0:
                    close_line = j
                    break
        if close_line is not None:
            break
    if close_line is None or close_line <= open_line + 1:
        return []
    indent = lines[open_line][: len(lines[open_line]) - len(lines[open_line].lstrip())]
    head = "".join(lines[i : open_line + 1])
    before = "".join(lines[i : close_line + 1])
    return [
        Mutant(
            path=path,
            start=i,
            end=close_line,
            operator="FN_BODY_DEFAULT",
            before=before,
            after=f"{head}{indent}    Default::default()\n{indent}}}\n",
            probe=f'{head}{indent}    panic!("{PROBE_MESSAGE}")\n{indent}}}\n',
            function=fn,
        )
    ]


# Every operator's span must stay inside the item its first line belongs to.
# Nothing re-checks it downstream: a filter that did was removed after a planted
# failure proved no operator can reach the case, and a check nothing can redden
# is the defect this whole gate looks for.  An operator whose span could cross an
# item boundary — into a `#[cfg(test)]` module, for instance — has to consult
# `cfg_regions` itself.
OPERATORS = [
    op_guard,
    op_call_default,
    op_try_default,
    op_entry_overwrite,
    op_token_swap,
    op_fn_body_default,
]


def generate(path: str, source: str, changed: set[int]) -> list[Mutant]:
    """Every candidate mutation on the changed lines of one file."""
    lines = source.splitlines(keepends=True)
    masks = masks_for(lines)
    skip = test_region(lines, masks)
    out: list[Mutant] = []
    seen: set[str] = set()
    for i in sorted(changed):
        if i >= len(lines) or i in skip:
            continue
        stripped = lines[i].lstrip()
        if stripped.startswith("//") or stripped.startswith("#[") or not stripped.strip():
            continue
        fn = enclosing_function(lines, i)
        for operator in OPERATORS:
            for mutant in operator(lines, masks, i, path, fn):
                if mutant.after == mutant.before:
                    continue
                if mutant.key in seen:
                    continue
                seen.add(mutant.key)
                out.append(mutant)
    return out


# --------------------------------------------------------------------------- #
# Applying and restoring
# --------------------------------------------------------------------------- #


class Tree:
    """The working tree, with a pristine copy of every file it touches.

    Restores by copying bytes back, never by `git checkout --`: a mutation loop
    that reverts through git will also throw away work that was not committed.
    """

    def __init__(self, root: Path) -> None:
        self.root = root
        self.snapshot_dir = Path(tempfile.mkdtemp(prefix="mutation-coverage-pristine-"))
        self.saved: dict[str, Path] = {}

    def _pristine(self, rel: str) -> Path:
        if rel not in self.saved:
            dest = self.snapshot_dir / rel.replace("/", "__")
            shutil.copyfile(self.root / rel, dest)
            self.saved[rel] = dest
        return self.saved[rel]

    def apply(self, mutant: Mutant, text: str) -> None:
        """Write one mutant, having first proved the span it claims to replace is there.

        A mutation that silently fails to apply produces the same green run as a
        test that cannot fail — the proof reports success in exactly the case it
        exists to catch.  Two things stop that here, and both are errors rather
        than warnings: the span must match the recorded text byte for byte, and
        the result must differ from what was there.

        There is deliberately no read-back of the write.  One was written and then
        deleted: `write_text` followed by `read_text` on the same path always
        agrees, so no planted failure could make it fire, and a check that cannot
        fail is the thing this whole gate exists to find.
        """
        self._pristine(mutant.path)
        target = self.root / mutant.path
        original = target.read_text()
        lines = original.splitlines(keepends=True)
        actual = "".join(lines[mutant.start : mutant.end + 1])
        if actual != mutant.before:
            raise Harness(
                f"anchor mismatch in {mutant.path} at lines "
                f"{mutant.start + 1}-{mutant.end + 1}\n  expected: {mutant.before!r}\n"
                f"  found:    {actual!r}"
            )
        mutated = "".join(lines[: mutant.start]) + text + "".join(lines[mutant.end + 1 :])
        if mutated == original:
            raise Harness(f"mutation for {mutant.key} changes nothing in {mutant.path}")
        target.write_text(mutated)

    def restore(self) -> None:
        for rel, pristine in self.saved.items():
            shutil.copyfile(pristine, self.root / rel)

    def cleanup(self) -> None:
        self.restore()
        shutil.rmtree(self.snapshot_dir, ignore_errors=True)


class Harness(Exception):
    """The harness could not do its job — distinct from a verdict about the code."""


# --------------------------------------------------------------------------- #
# Running the suite
# --------------------------------------------------------------------------- #

TEST_RESULT = re.compile(r"^test result: \w+\. (\d+) passed; (\d+) failed", re.M)
FAILED_TEST = re.compile(r"^---- (\S+) stdout ----", re.M)
FAILURE_LIST = re.compile(r"^failures:\n((?:    \S+\n)+)", re.M)


@dataclass
class SuiteRun:
    exit_code: int
    passed: int
    failed: int
    failing_tests: list[str]
    compile_error: bool
    timed_out: bool
    seconds: float


def run_suite(root: Path, command: list[str], timeout: int) -> SuiteRun:
    started = time.monotonic()
    try:
        proc = subprocess.run(
            command,
            cwd=root,
            capture_output=True,
            text=True,
            timeout=timeout,
        )
        out = proc.stdout + proc.stderr
        code = proc.returncode
        timed_out = False
    except subprocess.TimeoutExpired as exc:
        out = (exc.stdout or "") + (exc.stderr or "")
        if isinstance(out, bytes):
            out = out.decode("utf-8", "replace")
        code = 124
        timed_out = True
    seconds = time.monotonic() - started
    passed = sum(int(m.group(1)) for m in TEST_RESULT.finditer(out))
    failed = sum(int(m.group(2)) for m in TEST_RESULT.finditer(out))
    names = FAILED_TEST.findall(out)
    if not names:
        m = FAILURE_LIST.search(out)
        if m:
            names = [n.strip() for n in m.group(1).splitlines()]
    return SuiteRun(
        exit_code=code,
        passed=passed,
        failed=failed,
        failing_tests=names,
        compile_error="could not compile" in out,
        timed_out=timed_out,
        seconds=seconds,
    )


# --------------------------------------------------------------------------- #
# The equivalents registry
# --------------------------------------------------------------------------- #


def load_equivalents(path: Path) -> dict[str, str]:
    """key -> the one-line reason it was ruled a non-defect."""
    rulings: dict[str, str] = {}
    if not path.exists():
        return rulings
    for lineno, raw in enumerate(path.read_text().splitlines(), 1):
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        parts = raw.split("\t")
        if len(parts) < 6:
            raise Harness(
                f"{path}:{lineno}: a ruling needs six tab-separated fields "
                f"(key, path, function, operator, date, reason); found {len(parts)}"
            )
        key = parts[0].strip()
        rulings[key] = f"{parts[4].strip()} — {parts[5].strip()}"
    return rulings


# --------------------------------------------------------------------------- #
# Diff scoping
# --------------------------------------------------------------------------- #


def git(root: Path, *args: str) -> str:
    proc = subprocess.run(
        ["git", *args], cwd=root, capture_output=True, text=True, check=False
    )
    if proc.returncode != 0:
        raise Harness(f"git {' '.join(args)} failed: {proc.stderr.strip()}")
    return proc.stdout


HUNK = re.compile(r"^@@ -\d+(?:,\d+)? \+(\d+)(?:,(\d+))? @@")


def merge_base(root: Path, base: str) -> str:
    """Where this branch left `base`, not where `base` is now.

    Diffing against the tip of the target branch attributes every commit that
    landed on it since the branch was cut to this branch — as DELETIONS, which
    produce no mutants, and as additions in files this branch never touched,
    which produce mutants for somebody else's code.  The first wastes the budget
    and the second reports a survivor against the wrong author.
    """
    proc = subprocess.run(
        ["git", "merge-base", base, "HEAD"],
        cwd=root,
        capture_output=True,
        text=True,
        check=False,
    )
    return proc.stdout.strip() if proc.returncode == 0 and proc.stdout.strip() else base


def changed_lines(root: Path, base: str, paths: list[str] | None) -> dict[str, set[int]]:
    """0-based line indices added or modified in `src/**/*.rs` since `base`.

    Deleted lines are not represented: there is nothing left to mutate.  That is a
    real hole and it is named in the report — a diff that only DELETES a guard is
    invisible here, and the check that catches that one is code review.
    """
    base = merge_base(root, base)
    args = ["diff", "--unified=0", "--no-color", base, "--", "src"]
    if paths:
        args = ["diff", "--unified=0", "--no-color", base, "--", *paths]
    diff = git(root, *args)
    result: dict[str, set[int]] = {}
    current: str | None = None
    for line in diff.splitlines():
        if line.startswith("+++ b/"):
            candidate = line[6:]
            current = candidate if candidate.endswith(".rs") else None
            continue
        if line.startswith("+++ /dev/null"):
            current = None
            continue
        m = HUNK.match(line)
        if m and current:
            start = int(m.group(1))
            count = int(m.group(2) or 1)
            if count == 0:
                continue
            result.setdefault(current, set()).update(range(start - 1, start - 1 + count))
    return {p: v for p, v in result.items() if p.startswith("src/") and v}


def declared_features(root: Path) -> set[str]:
    """Feature names the crate actually declares, read from `Cargo.toml`.

    Passing cargo a feature that does not exist is a hard error, so a name picked
    out of a `cfg` attribute is checked against this before it is used.  A plain
    scan of the `[features]` table rather than a TOML parse: `tomllib` is 3.11+,
    and this file otherwise runs on any Python 3 a CI runner ships.
    """
    manifest = root / "Cargo.toml"
    if not manifest.exists():
        return set()
    names: set[str] = set()
    in_features = False
    for raw in manifest.read_text().splitlines():
        line = raw.strip()
        if line.startswith("["):
            in_features = line == "[features]"
            continue
        if in_features and "=" in line and not line.startswith("#"):
            names.add(line.split("=", 1)[0].strip())
    return names


def features_for(root: Path, changed: dict[str, set[int]]) -> tuple[list[str], list[str]]:
    """Cargo features the changed lines need in order to be COMPILED at all.

    Without this the gate has a blind spot of exactly the shape it exists to
    catch.  Code behind an off-by-default feature is not compiled by the default
    test command, so every mutation in it survives for a reason that has nothing
    to do with the tests, and a report full of false survivors is one nobody
    reads.  CI runs the suite twice for the same reason.

    Two sources.  `src/mcp/` is a whole module gated at its declaration in
    `lib.rs`, where nothing inside the module says so.  Everything else is found
    by reading the `cfg` attribute of the item each changed line sits in — which
    is how `catalog_names`, a lone `#[cfg(feature = "mcp")]` function in the
    middle of an otherwise unconditional file, gets its feature turned on.

    Returns the cargo arguments and any feature names the crate does not declare,
    which are reported rather than passed (cargo errors on an unknown feature).
    """
    wanted: set[str] = set()
    if any(p.startswith("src/mcp/") for p in changed):
        wanted.add("mcp")
    for path, line_numbers in changed.items():
        source_path = root / path
        if not source_path.exists():
            continue
        lines = source_path.read_text().splitlines(keepends=True)
        regions = cfg_regions(lines, masks_for(lines))
        for i in line_numbers:
            for condition in regions.get(i, ()):
                if condition.startswith("feature:"):
                    wanted.add(condition.split(":", 1)[1])
    declared = declared_features(root)
    unknown = sorted(wanted - declared)
    usable = sorted(wanted & declared)
    return (["--features", ",".join(usable)] if usable else [], unknown)


# --------------------------------------------------------------------------- #
# Selection under a budget
# --------------------------------------------------------------------------- #


def prioritise(mutants: list[Mutant]) -> list[Mutant]:
    """Round-robin across (file, function) so every changed function is reached.

    Straight source order spends the whole budget at the top of the first file.
    A diff touching twenty functions should mutate all twenty once before it
    mutates any of them twice.
    """
    buckets: dict[tuple[str, str], list[Mutant]] = {}
    for m in mutants:
        buckets.setdefault((m.path, m.function), []).append(m)
    for group in buckets.values():
        group.sort(key=lambda m: (m.start, m.operator))
    order = sorted(buckets)
    out: list[Mutant] = []
    depth = 0
    while True:
        added = False
        for name in order:
            group = buckets[name]
            if depth < len(group):
                out.append(group[depth])
                added = True
        if not added:
            return out
        depth += 1


# --------------------------------------------------------------------------- #
# The run
# --------------------------------------------------------------------------- #


def classify(run: SuiteRun) -> tuple[str, str]:
    if run.timed_out:
        return "KILLED", "the suite timed out"
    if run.compile_error:
        return "KILLED", "rejected by the compiler"
    if run.exit_code != 0:
        names = ", ".join(run.failing_tests[:4]) or "unnamed"
        more = f" (+{len(run.failing_tests) - 4} more)" if len(run.failing_tests) > 4 else ""
        return "KILLED", f"{names}{more}"
    return "SURVIVED", f"{run.passed} passed, nothing reddened"


def evaluate(
    tree: Tree,
    mutants: list[Mutant],
    command: list[str],
    timeout: int,
    budget: float,
    max_mutants: int,
    verbose: bool,
) -> tuple[list[Mutant], list[Mutant]]:
    """Run each mutant until the budget runs out.  Returns (run, not-run)."""
    started = time.monotonic()
    done: list[Mutant] = []
    for index, mutant in enumerate(mutants):
        elapsed = time.monotonic() - started
        if index >= max_mutants or elapsed >= budget:
            return done, mutants[index:]
        tree.apply(mutant, mutant.after)
        try:
            run = run_suite(tree.root, command, timeout)
        finally:
            tree.restore()
        mutant.verdict, mutant.detail = classify(run)
        if verbose:
            print(
                f"  [{index + 1}/{min(len(mutants), max_mutants)}] "
                f"{mutant.path}:{mutant.start + 1} {mutant.operator} "
                f"-> {mutant.verdict} ({run.seconds:.0f}s)",
                flush=True,
            )
        if mutant.verdict == "SURVIVED":
            mutant.reached = probe_reachability(tree, mutant, command, timeout, verbose)
        done.append(mutant)
    return done, []


def probe_reachability(
    tree: Tree, mutant: Mutant, command: list[str], timeout: int, verbose: bool
) -> str:
    """Does any test execute this line at all?

    The whole of the equivalent-mutant problem in one question.  A survivor whose
    line no test executes is a different finding from a survivor whose line every
    test executes and none of them checks — and a report that cannot tell them
    apart is one that gets skimmed.  Put a `panic!` at the same span: if the suite
    stays green the line never runs.
    """
    if mutant.probe is None:
        return "not probed"
    tree.apply(mutant, mutant.probe)
    try:
        run = run_suite(tree.root, command, timeout)
    finally:
        tree.restore()
    if run.compile_error:
        return "not probed"
    result = "no" if run.exit_code == 0 else "yes"
    if verbose:
        print(f"        reachability probe: executed by a test = {result}", flush=True)
    return result


# --------------------------------------------------------------------------- #
# Reporting
# --------------------------------------------------------------------------- #


def report(
    mutants: list[Mutant],
    skipped: list[Mutant],
    rulings: dict[str, str],
    baseline: SuiteRun,
    command: list[str],
    elapsed: float,
    strict: bool,
) -> int:
    unpinned: list[Mutant] = []
    unreached: list[Mutant] = []
    equivalent: list[Mutant] = []
    killed = 0
    for m in mutants:
        if m.verdict != "SURVIVED":
            killed += 1
        elif m.key in rulings:
            equivalent.append(m)
        elif m.reached == "no":
            unreached.append(m)
        else:
            unpinned.append(m)

    print()
    print("=" * 78)
    print("MUTATION COVERAGE")
    print("=" * 78)
    print(f"test command      {' '.join(command)}")
    print(f"baseline          {baseline.passed} passed, {baseline.failed} failed")
    print(f"mutations run     {len(mutants)} in {elapsed / 60:.1f} min")
    print(f"  killed          {killed}")
    print(f"  unpinned        {len(unpinned)}")
    print(f"  unreached       {len(unreached)}")
    print(f"  ruled equiv.    {len(equivalent)}")
    if skipped:
        print(f"not checked       {len(skipped)} (budget exhausted)")

    def show(title: str, group: list[Mutant], note: str) -> None:
        if not group:
            return
        print()
        print("-" * 78)
        print(f"{title} ({len(group)})")
        print(note)
        print("-" * 78)
        for m in group:
            print()
            print(f"  {m.path}:{m.start + 1}  fn {m.function}  [{m.operator}]")
            print(f"    -  {m.one_line_before()[:150]}")
            print(f"    +  {m.one_line_after()[:150]}")
            print(f"    {m.detail}")
            if m.key in rulings:
                print(f"    ruled: {rulings[m.key]}")
            print(f"    key: {m.key}")

    show(
        "UNPINNED — the tests run this line and none of them noticed",
        unpinned,
        "Add a test that reddens for the change shown, or rule it equivalent in\n"
        "scripts/mutation-coverage-equivalents.txt with a reason.  These fail the job.",
    )
    show(
        "UNREACHED — no test executes this line at all",
        unreached,
        "A `panic!` at the same place left the suite green.  Either the code is not\n"
        "exercised and should be, or it is genuinely unreachable and the survivor is\n"
        "equivalent.  Advisory unless --strict.",
    )
    show(
        "RULED EQUIVALENT — a human has already decided these are non-defects",
        equivalent,
        "Listed so the ruling stays visible and can be argued with, not hidden.",
    )
    if skipped:
        print()
        print("-" * 78)
        print(f"NOT CHECKED ({len(skipped)}) — the budget ran out before these")
        print("-" * 78)
        for m in skipped[:20]:
            print(f"  {m.path}:{m.start + 1}  fn {m.function}  [{m.operator}]")
        if len(skipped) > 20:
            print(f"  ... and {len(skipped) - 20} more")

    print()
    if unpinned:
        print(f"FAIL: {len(unpinned)} change(s) no test would notice.")
        return 1
    if strict and unreached:
        print(f"FAIL (--strict): {len(unreached)} change(s) no test executes.")
        return 1
    print("PASS: every mutation the budget reached was either caught or ruled.")
    return 0


# --------------------------------------------------------------------------- #
# Fixture mode
# --------------------------------------------------------------------------- #


def run_fixtures(root: Path, spec: Path, ref: str, timeout: int, verbose: bool) -> int:
    """Apply a recorded set of mutations at one commit and print the verdicts.

    This is the gate's regression fixture: mutations taken from builds that really
    shipped, each recorded with what it should do at each commit.  A harness that
    reports SURVIVED where the mutation is pinned, or KILLED where it is not, is
    broken in the direction that matters, and only a fixture with both answers in
    it can tell.
    """
    fixtures = json.loads(spec.read_text())["fixtures"]
    tree = Tree(root)
    command = ["cargo", "test", "--workspace"]
    print(f"fixture run at {ref} ({git(root, 'rev-parse', '--short', 'HEAD').strip()})")
    baseline = run_suite(root, command, timeout)
    if baseline.exit_code != 0 or baseline.passed == 0:
        print(f"SETUP: baseline is not green at {ref} ({baseline.passed} passed)")
        return 3
    print(f"baseline: {baseline.passed} passed")
    print()
    failures = 0
    for fx in fixtures:
        expected = fx["expect"].get(ref, "N/A")
        mutant = Mutant(
            path=fx["path"],
            start=0,
            end=0,
            operator=fx["operator"],
            before=fx["before"],
            after=fx["after"],
            probe=None,
            function=fx["function"],
        )
        located = locate(root / fx["path"], fx["before"])
        if located is None:
            got = "ABSENT"
        else:
            mutant.start, mutant.end = located
            tree.apply(mutant, mutant.after)
            try:
                run = run_suite(root, command, timeout)
            finally:
                tree.restore()
            got, detail = classify(run)
            if verbose:
                print(f"    {fx['id']}: {detail}")
        ok = got == expected
        if not ok:
            failures += 1
        print(f"  {'ok  ' if ok else 'FAIL'}  {fx['id']:<8} expected {expected:<9} got {got}")
    tree.cleanup()
    print()
    if failures:
        print(f"FAIL: {failures} fixture(s) did not match the recorded verdict at {ref}.")
        return 1
    print(f"PASS: every fixture matched its recorded verdict at {ref}.")
    return 0


def locate(path: Path, before: str) -> tuple[int, int] | None:
    """The 0-based line span whose text is exactly `before`, or None.

    Exactly one match is required.  Anchoring on the first hit is how a mutation
    lands on a comment that quotes the code instead of on the code.
    """
    if not path.exists():
        return None
    lines = path.read_text().splitlines(keepends=True)
    want = before.splitlines(keepends=True)
    hits = [
        i for i in range(len(lines) - len(want) + 1) if lines[i : i + len(want)] == want
    ]
    if len(hits) != 1:
        return None
    return hits[0], hits[0] + len(want) - 1


# --------------------------------------------------------------------------- #
# Entry point
# --------------------------------------------------------------------------- #


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description="Report changed code no test would notice being broken.",
    )
    parser.add_argument("--base", default="origin/main", help="what to diff against")
    parser.add_argument("--paths", nargs="*", help="restrict to these paths")
    parser.add_argument(
        "--budget-seconds",
        type=float,
        default=float(os.environ.get("MUTATION_BUDGET_SECONDS", DEFAULT_BUDGET_SECONDS)),
        help="wall-clock ceiling for the mutation loop",
    )
    parser.add_argument("--max-mutants", type=int, default=DEFAULT_MAX_MUTANTS)
    parser.add_argument("--timeout", type=int, default=DEFAULT_TIMEOUT_SECONDS)
    parser.add_argument(
        "--strict", action="store_true", help="fail on UNREACHED survivors too"
    )
    parser.add_argument("--fixtures", help="run the recorded fixture set at --ref")
    parser.add_argument("--ref", help="the name the fixture table records verdicts under")
    parser.add_argument("--list-only", action="store_true", help="print candidates, run none")
    parser.add_argument("--quiet", action="store_true")
    args = parser.parse_args(argv)

    root = Path(
        subprocess.run(
            ["git", "rev-parse", "--show-toplevel"],
            capture_output=True,
            text=True,
            check=False,
        ).stdout.strip()
        or "."
    )
    verbose = not args.quiet

    try:
        if args.fixtures:
            if not args.ref:
                print("usage: --fixtures needs --ref", file=sys.stderr)
                return 2
            return run_fixtures(root, Path(args.fixtures), args.ref, args.timeout, verbose)

        equivalents = root / "scripts" / "mutation-coverage-equivalents.txt"
        rulings = load_equivalents(equivalents)

        changed = changed_lines(root, args.base, args.paths)
        if not changed:
            print("No changed lines under src/ — nothing to mutate.")
            print("(This check is scoped to the diff by design: it never walks the crate.)")
            return 0

        mutants: list[Mutant] = []
        for path, lines in sorted(changed.items()):
            source = (root / path).read_text()
            mutants.extend(generate(path, source, lines))
        mutants = prioritise(mutants)

        print(f"changed files     {len(changed)}")
        print(f"changed lines     {sum(len(v) for v in changed.values())}")
        print(f"candidates        {len(mutants)}")
        if args.list_only:
            for m in mutants:
                print(f"  {m.path}:{m.start + 1} fn {m.function} [{m.operator}] {m.key}")
                print(f"      - {m.one_line_before()[:120]}")
                print(f"      + {m.one_line_after()[:120]}")
            return 0
        if not mutants:
            print("No mutable code on the changed lines.")
            return 0

        feature_args, unknown_features = features_for(root, changed)
        command = ["cargo", "test", "--workspace", *feature_args]
        print(f"test command      {' '.join(command)}")
        if unknown_features:
            print(
                "note              a changed line is behind "
                f"{', '.join(unknown_features)}, which Cargo.toml does not declare"
            )
        print("baseline          running...", flush=True)
        baseline = run_suite(root, command, args.timeout)
        if baseline.exit_code != 0:
            print("SETUP: the suite is not green before any mutation was applied.")
            print("       Nothing below would mean anything; fix the branch first.")
            for name in baseline.failing_tests[:10]:
                print(f"       failing: {name}")
            return 3
        if baseline.passed == 0:
            print("SETUP: the baseline run executed zero tests.")
            print("       A runner with nothing to run prints the same green as one")
            print("       that ran everything, so this is a harness fault, not a pass.")
            return 3
        print(f"baseline          {baseline.passed} passed in {baseline.seconds:.0f}s")

        tree = Tree(root)
        started = time.monotonic()
        try:
            done, skipped = evaluate(
                tree,
                mutants,
                command,
                args.timeout,
                args.budget_seconds,
                args.max_mutants,
                verbose,
            )
        finally:
            tree.cleanup()
        elapsed = time.monotonic() - started
        return report(done, skipped, rulings, baseline, command, elapsed, args.strict)
    except Harness as exc:
        print(f"SETUP: {exc}", file=sys.stderr)
        return 3


if __name__ == "__main__":
    sys.exit(main())
