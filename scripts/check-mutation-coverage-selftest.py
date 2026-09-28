#!/usr/bin/env python3
"""Prove `check-mutation-coverage.py` can return each of its verdicts.

A gate that cannot fail is green against working code and green against broken
code, and the two runs are indistinguishable — so the only evidence that a gate
works is watching it go red on purpose.  This is that evidence, checked in and run
by CI BEFORE the gate itself, for the same reason the public-hygiene workflow
runs its self-test first: a checker fails silently in both directions, and "the
tree is clean" means nothing until "the checker still bites" is established.

Two layers, because the failure modes are different.

The pure cases exercise the parts that decide WHAT to mutate — the string and
comment mask, the `#[cfg(test)]` skip, each operator, the content-addressed key,
and the two places the harness must refuse rather than proceed: an anchor that
does not match, and a mutation that changes nothing.  A mutation that silently
fails to apply produces exactly the same green as a test that cannot fail, so
those two are errors here and not warnings.

The end-to-end cases build real, tiny cargo crates and run the whole gate against
them.  The first has three functions differing only in how the tests treat them —
one asserted on, one called and not asserted on, one never called.  The second
repeats that shape with a token swap rather than a guard, because a token swap is
tiered by `_statement_probe` and a guard is not: with only the first crate here,
`op_token_swap` could be deleted and the probe could stop emitting its `panic!`,
both with this file green.  The third puts the mutated line in a struct literal's
field, where no probe compiles, which is the case that has to reach UNPROBED
rather than the blocking tier.  The fourth repeats the first's shape for the arm
operators, with one match per function, and adds an arm whose deletion and
widening the compiler refuses.  The fifth has guarded arms led by a string and a
char literal, whose widenings have to compile and survive, where a widening that
kept the literal was refused and set aside.

This is the layer that pins the verdict path: that a caught mutation exits 0, that
a survivor the tests execute exits 1, that a survivor nothing executes is
separated from it rather than listed beside it, that a survivor nothing measured
is separated from BOTH, that a ruling moves a survivor out of the failing tier,
that an arm mutation the compiler refuses is set aside rather than killed, and
that a red baseline is a setup error rather than a pass.  Nothing short of
running the real thing distinguishes those.

Exit 0 all cases passed · 1 at least one failed · 3 the harness could not run.
"""

from __future__ import annotations

import importlib.util
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

sys.dont_write_bytecode = True  # see the pyc trap note below

HERE = Path(__file__).resolve().parent
GATE = HERE / "check-mutation-coverage.py"

# CPython decides a cached .pyc is fresh from the source's mtime (one-second
# resolution) and byte length.  A mutation loop rewrites one line, runs, restores
# and rewrites another — which violates both at once, so a mutant of equal length
# written in the same second can silently import the previous mutant's bytecode
# and misattribute the kill.  Belt and braces: no new pyc, and clear any left by
# an earlier session.
os.environ["PYTHONDONTWRITEBYTECODE"] = "1"
shutil.rmtree(HERE / "__pycache__", ignore_errors=True)

spec = importlib.util.spec_from_file_location("mutation_coverage", GATE)
assert spec and spec.loader
mc = importlib.util.module_from_spec(spec)
sys.modules["mutation_coverage"] = mc
spec.loader.exec_module(mc)


# --------------------------------------------------------------------------- #
# Tiny harness
# --------------------------------------------------------------------------- #

CASES: list[tuple[str, object]] = []
FAILURES: list[str] = []


def case(name: str):
    def wrap(fn):
        CASES.append((name, fn))
        return fn

    return wrap


def check(name: str, condition: bool, detail: str = "") -> None:
    if not condition:
        raise AssertionError(f"{name}: {detail}")


# --------------------------------------------------------------------------- #
# Layer 1 — what gets mutated
# --------------------------------------------------------------------------- #


@case("a `==` inside a string or a comment is not code")
def _() -> None:
    line = 'let s = "a == b"; // c == d\n'
    mask, *_ = mc.code_mask(line)
    positions = [i for i, ok in enumerate(mask) if ok]
    text = "".join(line[i] for i in positions)
    check("string content masked", "a == b" not in text, text)
    check("comment content masked", "c == d" not in text, text)
    check("code kept", "let s =" in text, text)


@case("a `#[cfg(test)]` fn and a mid-file `#[cfg(test)]` mod are both skipped")
def _() -> None:
    source = (
        "pub fn before() -> u8 { 1 }\n"  # 0
        "#[cfg(test)]\n"  # 1
        "pub fn test_only(x: u8) -> u8 {\n"  # 2
        "    if x > 0 { 1 } else { 0 }\n"  # 3
        "}\n"  # 4
        "pub fn between() -> u8 { 2 }\n"  # 5
        "#[cfg(test)]\n"  # 6
        "mod tests {\n"  # 7
        "    fn helper() { if true { } }\n"  # 8
        "}\n"  # 9
        "pub fn after() -> u8 { 3 }\n"  # 10
    )
    lines = source.splitlines(keepends=True)
    marked = mc.test_region(lines, mc.masks_for(lines))
    check("cfg(test) fn body skipped", {1, 2, 3, 4} <= marked, sorted(marked))
    check("cfg(test) mod skipped", {6, 7, 8, 9} <= marked, sorted(marked))
    check("production before kept", 0 not in marked, sorted(marked))
    check("production between kept", 5 not in marked, sorted(marked))
    check("production after kept", 10 not in marked, sorted(marked))


@case("a multi-line `if` condition is replaced whole, brace included")
def _() -> None:
    source = (
        "fn f(p: &str) -> u8 {\n"
        "    if std::fs::metadata(p)\n"
        "        .map(|m| m.is_dir())\n"
        "        .unwrap_or(false)\n"
        "    {\n"
        "        return 1;\n"
        "    }\n"
        "    0\n"
        "}\n"
    )
    got = mc.generate("src/x.rs", source, set(range(9)))
    guards = [m for m in got if m.operator == "GUARD_FALSE"]
    check("one GUARD_FALSE", len(guards) == 1, [m.operator for m in got])
    g = guards[0]
    check("span covers the brace line", (g.start, g.end) == (1, 4), (g.start, g.end))
    check("replacement is a literal condition", g.one_line_after() == "if false {", g.after)


@case("a nested method call is blanked without mangling its receiver")
def _() -> None:
    source = "fn f(h: &mut Vec<u8>, rel: &str) {\n    h.extend(rel.as_bytes());\n}\n"
    got = [m for m in mc.generate("src/x.rs", source, {1}) if m.operator == "CALL_DEFAULT"]
    check("one CALL_DEFAULT", len(got) == 1, [m.one_line_after() for m in got])
    check(
        "receiver replaced with the call",
        got[0].one_line_after() == "h.extend(Default::default());",
        got[0].after,
    )


@case("`entry(..).or_insert(..)` becomes an unconditional overwrite")
def _() -> None:
    source = "fn f() {\n    m.entry(k.clone()).or_insert(kind);\n}\n"
    got = [m for m in mc.generate("src/x.rs", source, {1}) if m.operator == "ENTRY_OVERWRITE"]
    check("one ENTRY_OVERWRITE", len(got) == 1, [m.operator for m in got])
    check(
        "first-write-wins becomes last-write-wins",
        got[0].one_line_after() == "m.insert(k.clone(), kind);",
        got[0].after,
    )


@case("`?` is rewritten so a failure stops propagating")
def _() -> None:
    source = "fn f(p: &str) -> Option<String> {\n    let h = hash_dir(p)?;\n    Some(h)\n}\n"
    got = [m for m in mc.generate("src/x.rs", source, {1}) if m.operator == "TRY_DEFAULT"]
    check("one TRY_DEFAULT", len(got) == 1, [m.operator for m in got])
    check(
        "propagation replaced by a default",
        got[0].one_line_after() == "let h = hash_dir(p).unwrap_or_default();",
        got[0].after,
    )


@case("nothing inside a `#[cfg(test)]` block is offered as a candidate")
def _() -> None:
    source = (
        "pub fn real(x: u8) -> u8 {\n    if x > 0 { 1 } else { 0 }\n}\n"
        "#[cfg(test)]\nmod tests {\n"
        "    #[test]\n    fn t() {\n        if real(1) == 1 { }\n    }\n}\n"
    )
    got = mc.generate("src/x.rs", source, set(range(20)))
    check("some candidates in production code", got, "none at all")
    check(
        "no candidate inside the test module",
        all(m.start < 3 for m in got),
        [(m.start, m.operator) for m in got],
    )


ALL_OPERATORS_SOURCE = (
    "pub fn every(a: usize, b: usize, m: &mut Map) -> Option<usize> {\n"  # 0
    "    if a == b {\n"  # 1
    "        return None;\n"  # 2
    "    }\n"  # 3
    "    let flag = true;\n"  # 4
    "    let relax = a < b;\n"  # 5
    "    let both = flag || relax;\n"  # 6
    "    let hashed = compute(inner(a))?;\n"  # 7
    "    m.entry(a).or_insert(b);\n"  # 8
    "    Some(hashed)\n"  # 9
    "}\n"  # 10
    "pub fn arms(v: Option<u8>, loud: bool) -> u8 {\n"  # 11
    "    match v {\n"  # 12
    "        Some(_) if loud => 2,\n"  # 13
    "        Some(n) => n,\n"  # 14
    "        None => 0,\n"  # 15
    "    }\n"  # 16
    "}\n"  # 17
)

# The operator names `generate` is expected to emit for ALL_OPERATORS_SOURCE.
# Written out rather than derived from OPERATORS so that deleting an operator
# reddens this instead of quietly shrinking both sides of the comparison.
EXPECTED_OPERATORS = {
    "ARM_DELETE",
    "ARM_WIDEN",
    "BOOL_LIT_FLIP",
    "CALL_DEFAULT",
    "CMP_FLIP",
    "ENTRY_OVERWRITE",
    "FN_BODY_DEFAULT",
    "GUARD_FALSE",
    "GUARD_TRUE",
    "LOGIC_FLIP",
    "ORD_RELAX",
    "TRY_DEFAULT",
}


@case("every operator the docstring names is generated, and none is named that is not")
def _() -> None:
    # The gate's own defect, one level in: four of its operators had no case here
    # at all, so `op_token_swap` could be deleted outright with this file green.
    got = mc.generate("src/x.rs", ALL_OPERATORS_SOURCE, set(range(18)))
    emitted = {m.operator for m in got}
    check("no operator missing", EXPECTED_OPERATORS <= emitted, sorted(EXPECTED_OPERATORS - emitted))
    check("no operator unexpected", emitted <= EXPECTED_OPERATORS, sorted(emitted - EXPECTED_OPERATORS))


@case("every mutant carries a reachability probe that panics")
def _() -> None:
    # The claim in the module docstring, pinned.  A mutant with no probe is
    # reported unmeasured, and an unmeasured survivor used to print under a
    # header asserting the tests execute its line.
    got = mc.generate("src/x.rs", ALL_OPERATORS_SOURCE, set(range(18)))
    check("some mutants to check", len(got) >= len(EXPECTED_OPERATORS), len(got))
    for m in got:
        check(f"{m.operator} has a probe", m.probe is not None, m.one_line_before())
        check(
            f"{m.operator}'s probe panics",
            mc.PROBE_MESSAGE in (m.probe or ""),
            (m.operator, m.probe),
        )


ARM_SOURCE = (
    "pub fn shapes(e: Expr, n: Option<u8>) -> u8 {\n"  # 0
    "    let x = match e {\n"  # 1
    "        Expr::Value(v) => matches!(\n"  # 2
    "            v,\n"  # 3
    "            Value::A(_)\n"  # 4
    "                | Value::B(_)\n"  # 5
    "        ) as u8,\n"  # 6
    "        Expr::Block(b) if b.ok() => {\n"  # 7
    "            b.len()\n"  # 8
    "        }\n"  # 9
    "        Expr::Nested(inner) => match inner {\n"  # 10
    "            Some(_) => 1,\n"  # 11
    "            None => 2,\n"  # 12
    "        },\n"  # 13
    "        Expr::One\n"  # 14
    "        | Expr::Two => 3,\n"  # 15
    "        Expr::Cond(c) => if c {\n"  # 16
    "            1\n"  # 17
    "        } else {\n"  # 18
    "            2\n"  # 19
    "        }\n"  # 20
    "        Expr::Pair { a, .. }\n"  # 21
    "        | Expr::Other { a, .. } => a,\n"  # 22
    "        #[cfg(unix)]\n"  # 23
    "        Expr::Unix => 5,\n"  # 24
    "        Expr::X => 6, Expr::Y => 7,\n"  # 25
    "        Expr::Sum(a, b) => a\n"  # 26
    "            + b,\n"  # 27
    "        _ => 0,\n"  # 28
    "    };\n"  # 29
    "    let y = if x > 0 { 1 } else { 2 };\n"  # 30
    "    x + y\n"  # 31
    "}\n"  # 32
    "macro_rules! twice {\n"  # 33
    "    ($e:expr) => {\n"  # 34
    "        $e + $e\n"  # 35
    "    };\n"  # 36
    "}\n"  # 37
    "pub fn waits(rx: Rx, tick: Tick) -> u8 {\n"  # 38
    "    select! {\n"  # 39
    "        v = rx => v,\n"  # 40
    "        _ = tick => 0,\n"  # 41
    "    }\n"  # 42
    "}\n"  # 43
)


@case("an arm is found by its head, and spans its whole body")
def _() -> None:
    lines = ARM_SOURCE.splitlines(keepends=True)
    masks = mc.masks_for(lines)
    # (line the scan starts from) -> (start, end, has a guard, last), or None
    expected = {
        2: (2, 6, False, False),  # a body that is a multi-line macro call
        7: (7, 9, True, False),  # a guarded arm with a block body and no comma
        10: (10, 13, False, False),  # an arm whose body is a match of its own
        11: (11, 11, False, False),  # an arm of that inner match
        12: (12, 12, False, True),  # ... and its last arm
        14: (14, 15, False, False),  # an or-pattern over two lines, from its first
        15: (14, 15, False, False),  # ... and from its second
        16: (16, 20, False, False),  # an `if … else` body with no comma after it
        21: (21, 22, False, False),  # an or-pattern of struct patterns, from its first
        # 22: from its second line the arm cannot be told from one opening with a
        # leading `|`, and is left.  23 and 24: an arm under an attribute is left.
        # 25: two arms on one line cannot be cut out by whole lines, and are left.
        # 34, 40 and 41: a macro's rules and arms are not a match's.
        26: (26, 27, False, False),  # a body over two lines with no bracket around it
        28: (28, 28, False, True),  # the outer match's last arm
    }
    for i in range(len(lines)):
        arm = mc.arm_at(lines, masks, i)
        got = None if arm is None else (arm.start, arm.end, arm.guard is not None, arm.last)
        want = expected.get(i)
        check(f"line {i}", got == want, f"got {got}, want {want}: {lines[i]!r}")


@case("ARM_DELETE and ARM_WIDEN rewrite the arm they name, and keep the guard")
def _() -> None:
    lines = ARM_SOURCE.splitlines(keepends=True)
    listed = mc.generate("src/x.rs", ARM_SOURCE, {7, 14, 15})
    got = {(m.operator, m.start): m for m in listed}
    delete = got.get(("ARM_DELETE", 7))
    check("the guarded arm is deleted", delete is not None, sorted(got))
    check("the whole arm, block and all", delete.before == "".join(lines[7:10]), delete.before)
    check("and nothing is put back", delete.after == "", delete.after)
    check(
        "its probe is the arm's own head, taken first",
        delete.probe
        == f'        Expr::Block(b) if b.ok() => panic!("{mc.PROBE_MESSAGE}"),\n' + delete.before,
        delete.probe,
    )
    widen = got.get(("ARM_WIDEN", 7))
    check("the guarded arm is widened", widen is not None, sorted(got))
    check("its pattern goes and its guard stays", widen.after == "        _ if b.ok() => {\n", widen.after)
    check(
        "its probe fires when the match reaches the arm",
        widen.probe == f'        _ => panic!("{mc.PROBE_MESSAGE}"),\n' + lines[7],
        widen.probe,
    )
    widen = got.get(("ARM_WIDEN", 14))
    check("a two-line or-pattern is widened", widen is not None, sorted(got))
    check("to one `_`", widen.after == "        _ => 3,\n", widen.after)
    check("from both of its lines", widen.before == "".join(lines[14:16]), widen.before)
    check(
        "and two changed lines in one head make one mutant each, not two",
        sorted((m.operator, m.start) for m in listed if m.start == 14)
        == [("ARM_DELETE", 14), ("ARM_WIDEN", 14)],
        [(m.operator, m.start) for m in listed],
    )


@case("the last arm of a match, a body line, and a macro's arms are not arms to mutate")
def _() -> None:
    # The last arm: in a match that compiles, every value reaching it already
    # matches it, so widening it changes nothing and deleting it cannot compile.
    got = mc.generate("src/x.rs", ARM_SOURCE, {12, 28})
    check("no arm mutant on a last arm", not [m for m in got if m.operator.startswith("ARM_")], got)
    got = mc.generate("src/x.rs", ARM_SOURCE, {3, 4, 5, 8, 17, 18, 19, 27, 30, 31})
    check("no arm mutant on a body line", not [m for m in got if m.operator.startswith("ARM_")], got)
    got = mc.generate("src/x.rs", ARM_SOURCE, {34})
    check("no arm mutant in a macro_rules!", not [m for m in got if m.operator.startswith("ARM_")], got)
    # `select!` separates its arms with commas as a match does, so nothing but the
    # check that the enclosing brace is a match's tells the two apart.
    got = mc.generate("src/x.rs", ARM_SOURCE, {40, 41})
    check("no arm mutant in a select!", not [m for m in got if m.operator.startswith("ARM_")], got)


@case("a diff that changes no arm lists what it listed without the arm operators")
def _() -> None:
    # The candidates a change outside any match head produces are the six older
    # operators' and nothing else, in the same order.
    changed = {0, 3, 4, 5, 8, 17, 19, 27, 30, 31}
    full = [m.key for m in mc.generate("src/x.rs", ARM_SOURCE, changed)]
    saved = mc.OPERATORS
    mc.OPERATORS = [op for op in saved if op not in (mc.op_arm_delete, mc.op_arm_widen)]
    try:
        older = [m.key for m in mc.generate("src/x.rs", ARM_SOURCE, changed)]
    finally:
        mc.OPERATORS = saved
    check("some candidates to compare", older, older)
    check("the same list", full == older, (full, older))


# Arms whose pattern begins with a literal.  The code mask hides a literal as it
# hides a comment, and an arm finder that read its start through the mask began
# these arms after the literal, so their widening kept it: `"b" _ if loud`.
LITERAL_ARM_SOURCE = (
    "pub fn level(k: &str, c: char, loud: bool) -> u8 {\n"  # 0
    "    let a = match k {\n"  # 1
    '        "a" => 1,\n'  # 2
    '        "b" if loud => 2,\n'  # 3
    '        r"c" if loud => 3,\n'  # 4
    '        r#"d"# => 4,\n'  # 5
    "        // a comment above an arm\n"  # 6
    '        "e" | "f" => 5,\n'  # 7
    '        /* inline */ "g" => 6,\n'  # 8
    '        "h"\n'  # 9
    '        | "i" if loud => 7,\n'  # 10
    '        b"j" => 8,\n'  # 11
    "        _ => 0,\n"  # 12
    "    };\n"  # 13
    "    let b = match c {\n"  # 14
    "        'x' if loud => 1,\n"  # 15
    "        'a'..='z' => 2,\n"  # 16
    "        _ => 0,\n"  # 17
    "    };\n"  # 18
    "    a + b\n"  # 19
    "}\n"  # 20
)


@case("an arm whose pattern begins with a literal starts at the literal, and widens to `_`")
def _() -> None:
    lines = LITERAL_ARM_SOURCE.splitlines(keepends=True)
    masks = mc.masks_for(lines)
    # (line the scan starts from) -> (start, pattern, has a guard, last), or None
    expected = {
        2: (2, '"a"', False, False),
        3: (3, '"b"', True, False),  # a guarded string literal
        4: (4, 'r"c"', True, False),  # a guarded raw string
        5: (5, 'r#"d"#', False, False),
        7: (7, '"e" | "f"', False, False),  # below a comment, which is not the arm
        8: (8, '"g"', False, False),  # after a comment on its own line
        # 9: a line holding only a literal holds no code to start a scan from; the
        # arm is found from the next line of its head.
        10: (9, '"h" | "i"', True, False),  # an or-pattern whose first line is a literal
        11: (11, 'b"j"', False, False),
        12: (12, "_", False, True),
        15: (15, "'x'", True, False),  # a guarded char literal
        16: (16, "'a'..='z'", False, False),  # a char range
        17: (17, "_", False, True),
    }
    for i in range(len(lines)):
        arm = mc.arm_at(lines, masks, i)
        got = None if arm is None else (arm.start, arm.pattern, arm.guard is not None, arm.last)
        want = expected.get(i)
        check(f"line {i}", got == want, f"got {got}, want {want}: {lines[i]!r}")
    listed = mc.generate("src/x.rs", LITERAL_ARM_SOURCE, set(range(len(lines))))
    widened = {m.start: m for m in listed if m.operator == "ARM_WIDEN"}
    after = {
        2: "        _ => 1,\n",
        3: "        _ if loud => 2,\n",
        4: "        _ if loud => 3,\n",
        5: "        _ => 4,\n",
        7: "        _ => 5,\n",
        8: "        /* inline */ _ => 6,\n",
        9: "        _ if loud => 7,\n",
        11: "        _ => 8,\n",
        15: "        _ if loud => 1,\n",
        16: "        _ => 2,\n",
    }
    check("every arm but the last two is widened", sorted(widened) == sorted(after), sorted(widened))
    for start, want in after.items():
        m = widened.get(start)
        if m is None:
            continue
        check(f"line {start} widens to `_`, its guard kept", m.after == want, m.after)
        check(
            f"line {start}'s probe is a `_` arm in front of it",
            m.probe == want.split("_")[0] + f'_ => panic!("{mc.PROBE_MESSAGE}"),\n' + m.before,
            m.probe,
        )


@case("each token swap rewrites the token it is named for")
def _() -> None:
    swaps = [
        ("CMP_FLIP", "    let v = a == b;\n", "let v = a != b;"),
        ("CMP_FLIP", "    let v = a != b;\n", "let v = a == b;"),
        ("ORD_RELAX", "    let v = a < b;\n", "let v = a <= b;"),
        ("ORD_RELAX", "    let v = a > b;\n", "let v = a >= b;"),
        ("ORD_RELAX", "    let v = a <= b;\n", "let v = a < b;"),
        ("ORD_RELAX", "    let v = a >= b;\n", "let v = a > b;"),
        ("LOGIC_FLIP", "    let v = a && b;\n", "let v = a || b;"),
        ("LOGIC_FLIP", "    let v = a || b;\n", "let v = a && b;"),
        ("BOOL_LIT_FLIP", "    let v = true;\n", "let v = false;"),
        ("BOOL_LIT_FLIP", "    let v = false;\n", "let v = true;"),
    ]
    for operator, line, expected in swaps:
        source = "fn f(a: usize, b: usize) {\n" + line + "}\n"
        got = [m for m in mc.generate("src/x.rs", source, {1}) if m.operator == operator]
        check(f"{operator} generated for {line.strip()}", len(got) == 1, len(got))
        check(f"{operator} rewrote {line.strip()}", got[0].one_line_after() == expected, got[0].after)


@case("a match arm's body is wrapped, so the probe fires only if the arm is taken")
def _() -> None:
    # A match arm is not statement position, so a prepended `panic!` cannot compile
    # there — and a match arm is where a BOOL_LIT_FLIP on an option table lands.
    lines = ["            _ => true,\n"]
    got = mc.probe_for(lines, mc.masks_for(lines), 0)
    check(
        "the arm's body is wrapped",
        got.strip() == f'_ => {{ panic!("{mc.PROBE_MESSAGE}"); true }},',
        got,
    )
    lines = ["    Value::Number(n, _) => n.parse::<f64>().map(|x| x != 0.0).unwrap_or(true),\n"]
    got = mc.probe_for(lines, mc.masks_for(lines), 0)
    check("a guarded, nested arm too", "=> { panic!(" in got and got.rstrip().endswith("},"), got)
    # A body that opens a block here and closes it later is not on this line to wrap.
    lines = ["        Some(x) => {\n"]
    check(
        "an arm whose block runs past the line is refused",
        mc._match_arm_probe(lines, mc.masks_for(lines), 0) is None,
        mc._match_arm_probe(lines, mc.masks_for(lines), 0),
    )
    # The last arm of a match may omit its comma.  Without the comma the body's
    # right edge is not on the line to find, and taking the last code character
    # as the edge lops a character off the expression instead — `tru` from
    # `true`.  Refused rather than mangled.
    lines = ["        _ => true\n"]
    check(
        "a comma-less arm is refused rather than truncated",
        mc._match_arm_probe(lines, mc.masks_for(lines), 0) is None,
        mc._match_arm_probe(lines, mc.masks_for(lines), 0),
    )


@case("a predicate in a method chain is probed inside its closure")
def _() -> None:
    # The shape that let a real defect through as "unmeasured": a single-line
    # predicate mid-chain, which the tests drive on every load.
    lines = ['                .filter(|(kind, _, _)| *kind == "produces:")\n']
    got = mc.probe_for(lines, mc.masks_for(lines), 0)
    check(
        "the closure body is wrapped",
        got.strip()
        == f'.filter(|(kind, _, _)| {{ panic!("{mc.PROBE_MESSAGE}"); *kind == "produces:" }})',
        got,
    )
    # And the case it must NOT take: the mutated token can be outside the closure
    # when the chain continues, so "was the closure called" answers a different
    # question from the one asked.
    lines = ["                .map(|x| x != 0.0).unwrap_or(true)\n"]
    check(
        "a chain that continues past the closure is refused",
        mc._closure_body_probe(lines, mc.masks_for(lines), 0) is None,
        mc._closure_body_probe(lines, mc.masks_for(lines), 0),
    )
    check(
        "and it falls back to the statement probe",
        mc.probe_for(lines, mc.masks_for(lines), 0).lstrip().startswith("panic!"),
        mc.probe_for(lines, mc.masks_for(lines), 0),
    )


@case("a mutation's identity follows its text, not its line number")
def _() -> None:
    a = mc.mutation_key("src/x.rs", "f", "GUARD_FALSE", "    if a {\n", "    if false {\n")
    b = mc.mutation_key("src/x.rs", "f", "GUARD_FALSE", "  if a {\n", "  if false {\n")
    c = mc.mutation_key("src/x.rs", "f", "GUARD_FALSE", "    if b {\n", "    if false {\n")
    check("indentation does not change the key", a == b, (a, b))
    check("the mutated code does change the key", a != c, (a, c))


@case("an anchor matching twice is refused rather than applied to the first hit")
def _() -> None:
    with tempfile.TemporaryDirectory() as tmp:
        p = Path(tmp) / "x.rs"
        p.write_text("let a = 1;\nlet a = 1;\n")
        check("ambiguous anchor rejected", mc.locate(p, "let a = 1;\n") is None)
        p.write_text("let a = 1;\nlet b = 2;\n")
        check("unique anchor located", mc.locate(p, "let b = 2;\n") == (1, 1))


@case("an anchor that does not match is a harness error, not a silent skip")
def _() -> None:
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        (root / "src").mkdir()
        (root / "src" / "x.rs").write_text("line one\nline two\n")
        tree = mc.Tree(root)
        wrong = mc.Mutant(
            path="src/x.rs",
            start=0,
            end=0,
            operator="T",
            before="NOT THE TEXT\n",
            after="whatever\n",
            probe=None,
            function="f",
        )
        raised = False
        try:
            tree.apply(wrong, wrong.after)
        except mc.Harness:
            raised = True
        check("mismatch raises", raised, "apply() accepted a bad anchor")
        check("file untouched", (root / "src" / "x.rs").read_text() == "line one\nline two\n")
        tree.cleanup()


@case("a mutation that changes nothing is a harness error")
def _() -> None:
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        (root / "src").mkdir()
        (root / "src" / "x.rs").write_text("line one\n")
        tree = mc.Tree(root)
        inert = mc.Mutant(
            path="src/x.rs",
            start=0,
            end=0,
            operator="T",
            before="line one\n",
            after="line one\n",
            probe=None,
            function="f",
        )
        raised = False
        try:
            tree.apply(inert, inert.after)
        except mc.Harness:
            raised = True
        check("inert mutation raises", raised, "apply() accepted a no-op mutation")
        tree.cleanup()


@case("restore puts the original bytes back exactly")
def _() -> None:
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        (root / "src").mkdir()
        original = "alpha\nbeta\ngamma\n"
        (root / "src" / "x.rs").write_text(original)
        tree = mc.Tree(root)
        m = mc.Mutant(
            path="src/x.rs",
            start=1,
            end=1,
            operator="T",
            before="beta\n",
            after="BETA\n",
            probe=None,
            function="f",
        )
        tree.apply(m, m.after)
        check("mutation landed", (root / "src" / "x.rs").read_text() != original)
        tree.restore()
        check("restored byte for byte", (root / "src" / "x.rs").read_text() == original)
        tree.cleanup()


@case("a failing run is KILLED and the report names the test that reddened")
def _() -> None:
    run = mc.SuiteRun(
        exit_code=101,
        passed=530,
        failed=1,
        failing_tests=["runner::tests::test_directory_content_change"],
        compile_error=False,
        timed_out=False,
        seconds=12.0,
    )
    verdict, detail = mc.classify(run)
    check("verdict", verdict == "KILLED", verdict)
    check("names the test", "test_directory_content_change" in detail, detail)


@case("a green run is SURVIVED and the detail is a count, not a percentage")
def _() -> None:
    run = mc.SuiteRun(
        exit_code=0,
        passed=561,
        failed=0,
        failing_tests=[],
        compile_error=False,
        timed_out=False,
        seconds=12.0,
    )
    verdict, detail = mc.classify(run)
    check("verdict", verdict == "SURVIVED", verdict)
    check("count reported", "561 passed" in detail, detail)


@case("a mutant that hangs the suite is KILLED, not a survivor")
def _() -> None:
    # A mutation that sends the suite into a loop is caught behaviour, not a
    # survivor.  Read as SURVIVED it becomes a report entry nobody can act on.
    run = mc.SuiteRun(124, 0, 0, [], compile_error=False, timed_out=True, seconds=900.0)
    verdict, detail = mc.classify(run)
    check("verdict", verdict == "KILLED", verdict)
    check("reason", "timed out" in detail, detail)


@case("a probe run that reddens without the panic firing measured nothing")
def _() -> None:
    # A probe run can go red for a reason that has nothing to do with the probe.
    # Reading the exit code alone then puts a survivor in the blocking tier on no
    # evidence, which is the shape this whole gate exists to report.
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        (root / "src").mkdir()
        (root / "src" / "x.rs").write_text("let a = 1;\n")
        tree = mc.Tree(root)
        mutant = mc.Mutant(
            path="src/x.rs",
            start=0,
            end=0,
            operator="T",
            before="let a = 1;\n",
            after="let a = 2;\n",
            probe=f'panic!("{mc.PROBE_MESSAGE}");\nlet a = 1;\n',
            function="f",
        )

        def stub(exit_code: int, probe_fired: bool, **rest):
            def fake(_root, _command, _timeout):
                return mc.SuiteRun(
                    exit_code=exit_code,
                    passed=1,
                    failed=0,
                    failing_tests=[],
                    compile_error=rest.get("compile_error", False),
                    timed_out=rest.get("timed_out", False),
                    seconds=0.1,
                    probe_fired=probe_fired,
                )

            return fake

        real = mc.run_suite
        try:
            mc.run_suite = stub(101, True)
            answer, note = mc.probe_reachability(tree, mutant, [], 1, False)
            check("the panic firing means the line was reached", answer == "yes", (answer, note))

            mc.run_suite = stub(101, False)
            answer, note = mc.probe_reachability(tree, mutant, [], 1, False)
            check("red without the panic is not 'reached'", answer == "not probed", (answer, note))
            check("and the report is told why", "without the panic" in note, note)

            mc.run_suite = stub(0, False)
            answer, note = mc.probe_reachability(tree, mutant, [], 1, False)
            check("green means no test executes it", answer == "no", (answer, note))

            mc.run_suite = stub(124, False, timed_out=True)
            answer, note = mc.probe_reachability(tree, mutant, [], 1, False)
            check("a probe that timed out measured nothing", answer == "not probed", (answer, note))

            mc.run_suite = stub(101, False, compile_error=True)
            answer, note = mc.probe_reachability(tree, mutant, [], 1, False)
            check("a probe that would not compile measured nothing", answer == "not probed", (answer, note))
        finally:
            mc.run_suite = real
        check("the file is back", (root / "src" / "x.rs").read_text() == "let a = 1;\n")
        tree.cleanup()


@case("a mutant the compiler rejects is KILLED and says so")
def _() -> None:
    run = mc.SuiteRun(0, 0, 0, [], compile_error=True, timed_out=False, seconds=1.0)
    verdict, detail = mc.classify(run)
    check("verdict", verdict == "KILLED", verdict)
    check("reason", "compiler" in detail, detail)


@case("a malformed ruling is refused rather than read as an empty registry")
def _() -> None:
    with tempfile.TemporaryDirectory() as tmp:
        p = Path(tmp) / "eq.txt"
        p.write_text("# a comment\nabc123\tsrc/x.rs\tf\tGUARD_FALSE\n")
        raised = False
        try:
            mc.load_equivalents(p)
        except mc.Harness:
            raised = True
        check("short record raises", raised, "a four-field ruling was accepted")
        p.write_text("abc123\tsrc/x.rs\tf\tGUARD_FALSE\t2026-08-19\tunreachable in production\n")
        rulings = mc.load_equivalents(p)
        check("well-formed record parses", "abc123" in rulings, rulings)
        check("reason carried", "unreachable" in rulings["abc123"], rulings)


CFG_SOURCE = (
    "pub fn plain() -> u8 {\n    if true { 1 } else { 0 }\n}\n"  # 0 1 2
    '#[cfg(feature = "mcp")]\n'  # 3
    "pub fn gated() -> u8 {\n    if true { 2 } else { 0 }\n}\n"  # 4 5 6
    '#[cfg(all(test, feature = "mcp"))]\n'  # 7
    "mod gated_tests {\n    fn t() { if true { } }\n}\n"  # 8 9 10
    '#[cfg(not(feature = "opendal"))]\n'  # 11
    "pub fn without() -> u8 {\n    if true { 3 } else { 0 }\n}\n"  # 12 13 14
)


@case("a test module behind `#[cfg(all(test, ...))]` is still test code")
def _() -> None:
    # Found by running the gate over src/operator.rs: an exact-string match on
    # `#[cfg(test)]` offered every line of a whole test module as a candidate,
    # because that module is spelled `#[cfg(all(test, feature = "mcp"))]`.
    lines = CFG_SOURCE.splitlines(keepends=True)
    marked = mc.test_region(lines, mc.masks_for(lines))
    check("the all(test, ..) module is skipped", {7, 8, 9, 10} <= marked, sorted(marked))
    check("the plain function is not", 1 not in marked, sorted(marked))
    check("the feature-gated function is not", 5 not in marked, sorted(marked))
    got = mc.generate("src/x.rs", CFG_SOURCE, set(range(len(lines))))
    check(
        "and nothing inside it is a candidate",
        all(not (7 <= m.start <= 10) for m in got),
        [(m.start, m.operator) for m in got],
    )


@case("a hashed raw string masks its own contents and nothing after it")
def _() -> None:
    # `r#"..."#` was not recognised at all: the mask looked at the character
    # before a quote and only knew `r"`, so the hashed form was parsed as an
    # ordinary string ending at the first quote INSIDE it.  Everything after that
    # -- braces and all -- was marked as code.
    line = 'let s = r#"{"zebra": "quokka"} == 1"#; let n = 1 == 2;\n'
    mask, *_ = mc.code_mask(line)
    text = "".join(line[i] for i, ok in enumerate(mask) if ok)
    # The words are the assertion: parsed as an ordinary string, the literal's
    # quotes pair up wrongly and its content lands OUTSIDE the pairs, so these
    # come back marked as code.
    check("content between the inner quotes is masked", "zebra" not in text, text)
    check("and so is the rest of it", "quokka" not in text, text)
    check("a == inside the literal is not a candidate", text.count("==") == 1, text)
    check("and code after the literal is still code", "let n" in text, text)


@case("a raw string spanning lines does not leak its braces into the cfg scan")
def _() -> None:
    # The real one, from src/operator.rs: a multi-line `r#"..."#` JSON fixture
    # inside `#[cfg(test)] mod tests`.  Its opening line carries an unmatched `{`,
    # which the brace matcher counted, closed the module a thousand lines early,
    # and offered every test after it as a candidate.
    source = (
        "pub fn before() -> u8 { if true { 1 } else { 0 } }\n"  # 0
        "#[cfg(test)]\n"  # 1
        "mod tests {\n"  # 2
        "    fn fixture() -> &'static str {\n"  # 3
        '        r#"{"a": {\n'  # 4
        '            "b": 1}}"#\n'  # 5
        "    }\n"  # 6
        "    fn t() { if true { } }\n"  # 7
        "}\n"  # 8
        "pub fn after() -> u8 { if true { 3 } else { 0 } }\n"  # 9
    )
    lines = source.splitlines(keepends=True)
    marked = mc.test_region(lines, mc.masks_for(lines))
    check("the module runs to its real closing brace", {2, 7, 8} <= marked, sorted(marked))
    check("production before it is not test code", 0 not in marked, sorted(marked))
    check("production after it is not test code", 9 not in marked, sorted(marked))
    got = mc.generate("src/x.rs", source, set(range(len(lines))))
    check(
        "so nothing inside the module is a candidate",
        all(not (1 <= m.start <= 8) for m in got),
        [(m.start, m.operator) for m in got],
    )
    check(
        "while the production functions either side still are",
        {m.start for m in got} == {0, 9},
        [(m.start, m.operator) for m in got],
    )


@case("a changed line behind a feature turns that feature on for the test run")
def _() -> None:
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        (root / "src").mkdir()
        (root / "src" / "x.rs").write_text(CFG_SOURCE)
        (root / "Cargo.toml").write_text(
            '[package]\nname = "x"\nversion = "0.0.0"\n\n[features]\nmcp = []\nopendal = []\n'
        )
        check("features are read from the manifest", mc.declared_features(root) == {"mcp", "opendal"})

        args, unknown = mc.features_for(root, {"src/x.rs": {5}})
        check("gated line asks for its feature", args == ["--features", "mcp"], args)
        check("nothing unknown", unknown == [], unknown)

        args, _ = mc.features_for(root, {"src/x.rs": {1}})
        check("ungated line asks for nothing", args == [], args)

        # `not(feature = ..)` inverts the sense: turning the feature ON removes
        # the code, so it must not be requested.
        args, _ = mc.features_for(root, {"src/x.rs": {13}})
        check("a negated cfg asks for nothing", args == [], args)

        args, _ = mc.features_for(root, {"src/mcp/mod.rs": {1}})
        check("src/mcp/ still asks for mcp by path", args == ["--features", "mcp"], args)

        (root / "Cargo.toml").write_text('[package]\nname = "x"\nversion = "0.0.0"\n')
        args, unknown = mc.features_for(root, {"src/x.rs": {5}})
        check("an undeclared feature is not passed to cargo", args == [], args)
        check("but it is named", unknown == ["mcp"], unknown)


# --------------------------------------------------------------------------- #
# Layer 2 — the verdict path, against a real crate
# --------------------------------------------------------------------------- #

CRATE_MANIFEST = """[package]
name = "gatefixture"
version = "0.0.0"
edition = "2021"

[workspace]
"""

# Three functions of identical shape.  The only difference is what the tests do
# with them, which is exactly the distinction the three tiers exist to draw.
CRATE_LIB = """pub fn pinned(v: &[u8]) -> usize {
    if v.is_empty() {
        return 99;
    }
    v.len()
}

pub fn executed_but_unchecked(v: &[u8]) -> usize {
    if v.is_empty() {
        return 99;
    }
    v.len()
}

pub fn never_called(v: &[u8]) -> usize {
    if v.is_empty() {
        return 99;
    }
    v.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pinned_is_asserted_on() {
        assert_eq!(pinned(b""), 99);
        assert_eq!(pinned(b"abc"), 3);
    }

    #[test]
    fn executed_but_unchecked_is_called_and_never_asserted_on() {
        let _ = executed_but_unchecked(b"");
        let _ = executed_but_unchecked(b"abc");
    }
}
"""


def build_fixture_crate(root: Path, lib: str = CRATE_LIB) -> None:
    (root / "src").mkdir(parents=True, exist_ok=True)
    (root / "Cargo.toml").write_text(CRATE_MANIFEST)
    (root / "src" / "lib.rs").write_text("")
    env = {
        **os.environ,
        "GIT_AUTHOR_NAME": "selftest",
        "GIT_AUTHOR_EMAIL": "selftest@example.invalid",
        "GIT_COMMITTER_NAME": "selftest",
        "GIT_COMMITTER_EMAIL": "selftest@example.invalid",
    }
    for args in (
        ["init", "-q"],
        ["add", "-A"],
        ["commit", "-qm", "empty"],
    ):
        subprocess.run(["git", *args], cwd=root, check=True, env=env, capture_output=True)
    # Uncommitted, so `git diff HEAD` presents every line as newly added — the
    # same shape a branch presents against its merge base.
    (root / "src" / "lib.rs").write_text(lib)


def run_gate(root: Path, *extra: str) -> tuple[int, str]:
    proc = subprocess.run(
        [sys.executable, str(GATE), "--base", "HEAD", *extra],
        cwd=root,
        capture_output=True,
        text=True,
        env={**os.environ, "PYTHONDONTWRITEBYTECODE": "1"},
    )
    return proc.returncode, proc.stdout + proc.stderr


TIER_HEADINGS = {
    "UNPINNED": "UNPINNED",
    "UNREACHED": "UNREACHED",
    "UNPROBED": "UNPROBED",
    "RULED EQUIVALENT": "EQUIVALENT",
    "SET ASIDE": "SET ASIDE",
    "NOT CHECKED": "NOT CHECKED",
}


def tier_of(output: str, function: str) -> str:
    """Which report section a function's survivor landed in."""
    current = "KILLED"
    found = "KILLED"
    for line in output.splitlines():
        for prefix, name in TIER_HEADINGS.items():
            if line.startswith(prefix + " "):
                current = name
        if f"fn {function} " in line or line.rstrip().endswith(f"fn {function}"):
            found = current
    return found


def tier_of_operator(output: str, function: str, operator: str) -> str:
    """Which section ONE operator's survivor landed in.

    `tier_of` answers for the whole function, and a function with two survivors in
    two different tiers gives it whichever the report printed last.  Every case
    that pins the probe needs the answer for one operator, because the point is
    that two operators on the same function can disagree.
    """
    needle = f"fn {function}  [{operator}]"
    current = "KILLED"
    found = "KILLED"
    for line in output.splitlines():
        for prefix, name in TIER_HEADINGS.items():
            if line.startswith(prefix + " "):
                current = name
        if needle in line:
            found = current
    return found


def tier_at(output: str, where: str, operator: str) -> str:
    """Which section the mutant at `path:line` from one operator landed in.

    A function with two arms can carry two ARM_DELETE mutants that land in
    different sections, so neither the function nor the operator names one.
    """
    current = "KILLED"
    found = "KILLED"
    for line in output.splitlines():
        for prefix, name in TIER_HEADINGS.items():
            if line.startswith(prefix + " "):
                current = name
        if line.strip().startswith(f"{where}  fn ") and f"[{operator}]" in line:
            found = current
    return found


@case("the fixture table is well formed and can distinguish both answers")
def _() -> None:
    import json

    spec = json.loads((HERE / "mutation-coverage-fixtures.json").read_text())
    fixtures = spec["fixtures"]
    check("fixtures present", len(fixtures) >= 7, len(fixtures))
    ids = [f["id"] for f in fixtures]
    check("ids unique", len(set(ids)) == len(ids), ids)
    verdicts = set()
    for f in fixtures:
        for field in ("id", "path", "function", "operator", "before", "after", "expect", "what"):
            check(f"{f.get('id')} has {field}", field in f, sorted(f))
        check(f"{f['id']} really changes something", f["before"] != f["after"], f["id"])
        check(f"{f['id']} names a ref in refs", set(f["expect"]) <= set(spec["refs"]), f["expect"])
        verdicts |= set(f["expect"].values())
    # A table where every recorded answer is the same cannot tell a harness that
    # always says SURVIVED from one that works.
    check("both answers are represented", {"SURVIVED", "KILLED"} <= verdicts, sorted(verdicts))


@case("the base is the branch point, not the tip of the branch being merged into")
def _() -> None:
    import subprocess as sp

    env = {
        **os.environ,
        "GIT_AUTHOR_NAME": "selftest",
        "GIT_AUTHOR_EMAIL": "selftest@example.invalid",
        "GIT_COMMITTER_NAME": "selftest",
        "GIT_COMMITTER_EMAIL": "selftest@example.invalid",
    }
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        (root / "src").mkdir()
        (root / "src" / "a.rs").write_text("fn a() -> u8 { 1 }\n")

        def g(*args: str) -> str:
            return sp.run(
                ["git", *args], cwd=root, env=env, capture_output=True, text=True, check=True
            ).stdout

        g("init", "-q", "-b", "trunk")
        g("add", "-A")
        g("commit", "-qm", "base")
        branch_point = g("rev-parse", "HEAD").strip()
        # trunk moves on, in a file this branch never touches
        (root / "src" / "b.rs").write_text("fn b() -> u8 {\n    if true { 2 } else { 3 }\n}\n")
        g("add", "-A")
        g("commit", "-qm", "someone else's work")
        g("checkout", "-q", "-b", "feature", branch_point)
        (root / "src" / "a.rs").write_text("fn a() -> u8 {\n    if false { 1 } else { 4 }\n}\n")
        g("add", "-A")
        g("commit", "-qm", "my work")

        resolved = mc.merge_base(root, "trunk")
        check("resolves to the branch point", resolved == branch_point, (resolved, branch_point))
        touched = mc.changed_lines(root, "trunk", None)
        check("my file is in scope", "src/a.rs" in touched, sorted(touched))
        check(
            "the other branch's file is NOT attributed to me",
            "src/b.rs" not in touched,
            sorted(touched),
        )


@case("END TO END: a survivor the tests execute fails the job and is named UNPINNED")
def _() -> None:
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        build_fixture_crate(root)
        code, out = run_gate(root, "--quiet")
        check("exit 1", code == 1, f"exit {code}\n{out[-3000:]}")
        check(
            "unchecked function reported UNPINNED",
            tier_of(out, "executed_but_unchecked") == "UNPINNED",
            out[-3000:],
        )
        check(
            "the asserted-on function is not reported",
            tier_of(out, "pinned") == "KILLED",
            out[-3000:],
        )
        check("the failure line says what it means", "no test would notice" in out, out[-800:])


# A token swap on a line in statement position, in three functions that differ
# only in what the tests do with them.  This is the crate that pins
# `op_token_swap` end to end AND pins `_statement_probe` keeping its `panic!`:
# every mutation here that is not a whole-body swap is tiered by that probe, and
# nothing else in this file evaluates one.
CRATE_TOKEN_SWAP = """pub fn compared_and_checked(a: usize, b: usize) -> usize {
    let same = a == b;
    same as usize
}

pub fn compared_but_unchecked(a: usize, b: usize) -> usize {
    let same = a == b;
    same as usize
}

pub fn compared_never_called(a: usize, b: usize) -> usize {
    let same = a == b;
    same as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compared_and_checked_is_asserted_on() {
        assert_eq!(compared_and_checked(1, 1), 1);
        assert_eq!(compared_and_checked(1, 2), 0);
    }

    #[test]
    fn compared_but_unchecked_is_called_and_never_asserted_on() {
        let _ = compared_but_unchecked(1, 1);
        let _ = compared_but_unchecked(1, 2);
    }
}
"""

# A struct literal field: not statement position, no `=>` to wrap, and no call
# closing at the end of the line — so none of `probe_for`'s three shapes reaches
# it and the survivor's reachability genuinely cannot be measured.  A method
# chain used to be the example here and is no longer one: `_closure_body_probe`
# reaches it, which is the point of that function.  `total` is asserted on, which
# kills the whole-body swap; `same` is not, so the token swap survives.
CRATE_UNPROBEABLE = """pub struct Pair {
    pub same: bool,
    pub total: usize,
}

pub fn describe(a: usize, b: usize) -> Pair {
    Pair {
        same: a == b,
        total: 99 + a + b,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_total_is_asserted_on_but_the_comparison_is_not() {
        assert_eq!(describe(1, 2).total, 102);
    }
}
"""


# Two functions with one match between them, differing only in what the tests
# check.  In each, the first arm's deletion and widening compile; the second
# arm's do not — deleting it leaves `Some(_)` uncovered when `loud` is false, and
# widening it drops the `n` its body returns — and the last arm is left alone.
CRATE_MATCH_ARMS = """pub fn arm_pinned(v: Option<u8>, loud: bool) -> u8 {
    match v {
        Some(_) if loud => 200,
        Some(n) => n,
        None => 0,
    }
}

pub fn arm_unpinned(v: Option<u8>, loud: bool) -> u8 {
    match v {
        Some(_) if loud => 200,
        Some(n) => n,
        None => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arm_pinned_is_asserted_on() {
        assert_eq!(arm_pinned(Some(7), true), 200);
        assert_eq!(arm_pinned(Some(7), false), 7);
        assert_eq!(arm_pinned(None, true), 0);
    }

    #[test]
    fn arm_unpinned_is_called_and_never_asserted_on() {
        let _ = arm_unpinned(Some(7), true);
        let _ = arm_unpinned(Some(7), false);
        let _ = arm_unpinned(None, true);
    }
}
"""


@case("END TO END: a deleted or widened arm survives unpinned, dies pinned, and a refusal is set aside")
def _() -> None:
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        build_fixture_crate(root, CRATE_MATCH_ARMS)
        code, listing = run_gate(root, "--quiet", "--list-only")
        check("list-only exits 0", code == 0, listing[-2000:])
        for where in ("src/lib.rs:3", "src/lib.rs:11"):
            for operator in ("ARM_DELETE", "ARM_WIDEN"):
                check(
                    f"{operator} listed at {where}",
                    any(
                        ln.strip().startswith(where + " fn ") and f"[{operator}]" in ln
                        for ln in listing.splitlines()
                    ),
                    listing,
                )
        check("the widening keeps the guard", "+ _ if loud => 200," in listing, listing)
        check(
            "the last arm is not listed",
            not any(
                ln.strip().startswith(("src/lib.rs:5 ", "src/lib.rs:13 ")) and "[ARM_" in ln
                for ln in listing.splitlines()
            ),
            listing,
        )
        code, out = run_gate(root, "--quiet")
        check("exit 1", code == 1, f"exit {code}\n{out[-3000:]}")
        for operator in ("ARM_DELETE", "ARM_WIDEN"):
            check(
                f"{operator} on the asserted-on arm is killed",
                tier_at(out, "src/lib.rs:3", operator) == "KILLED",
                out[-4000:],
            )
            check(
                f"{operator} on the arm the tests take and do not check is UNPINNED",
                tier_at(out, "src/lib.rs:11", operator) == "UNPINNED",
                out[-4000:],
            )
            for where in ("src/lib.rs:4", "src/lib.rs:12"):
                check(
                    f"{operator} at {where}, which the compiler refuses, is set aside",
                    tier_at(out, where, operator) == "SET ASIDE",
                    out[-4000:],
                )
        check("and the report counts the four", "set aside       4 " in out, out[:1500])


# A guarded arm led by a string literal and one led by a char literal, each
# asserted on for every value the tests pass, and each widened to `_ if loud`
# with the suite still green: no test passes a loud value outside the pattern.
# Every other arm mutation here is killed, and none is refused.
CRATE_LITERAL_ARMS = """pub fn str_arm(k: &str, loud: bool) -> u8 {
    match k {
        "a" => 1,
        "b" if loud => 2,
        _ => 0,
    }
}

pub fn char_arm(c: char, loud: bool) -> u8 {
    match c {
        'a' => 1,
        'b' if loud => 3,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_arm_is_asserted_on_and_no_quiet_value_reaches_a_guard() {
        assert_eq!(str_arm("a", true), 1);
        assert_eq!(str_arm("b", true), 2);
        assert_eq!(str_arm("b", false), 0);
        assert_eq!(str_arm("z", false), 0);
        assert_eq!(char_arm('a', true), 1);
        assert_eq!(char_arm('b', true), 3);
        assert_eq!(char_arm('b', false), 0);
        assert_eq!(char_arm('z', false), 0);
    }
}
"""


@case("END TO END: a guarded arm led by a string or char literal is widened and reported UNPINNED")
def _() -> None:
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        build_fixture_crate(root, CRATE_LITERAL_ARMS)
        code, listing = run_gate(root, "--quiet", "--list-only")
        check("list-only exits 0", code == 0, listing[-2000:])
        for where, rewrite in (("src/lib.rs:4", "_ if loud => 2,"), ("src/lib.rs:12", "_ if loud => 3,")):
            rows = listing.splitlines()
            at = [
                k
                for k, ln in enumerate(rows)
                if ln.strip().startswith(where + " fn ") and "[ARM_WIDEN]" in ln
            ]
            check(f"ARM_WIDEN listed at {where}", len(at) == 1, listing)
            check(
                f"the widening at {where} drops the literal and keeps the guard",
                at and rows[at[0] + 2].strip() == "+ " + rewrite,
                listing,
            )
        code, out = run_gate(root, "--quiet")
        check("exit 1", code == 1, f"exit {code}\n{out[-3000:]}")
        for where in ("src/lib.rs:4", "src/lib.rs:12"):
            check(
                f"the widening at {where} is UNPINNED",
                tier_at(out, where, "ARM_WIDEN") == "UNPINNED",
                out[-4000:],
            )
        check("nothing is set aside", "set aside       0 " in out, out[:1500])
        check("and the other eight are killed", "killed          8\n" in out, out[:1500])


@case("END TO END: a token swap is generated, run, and tiered by its own probe")
def _() -> None:
    # `op_token_swap` could be deleted outright with this file green, because no
    # case generated one.  Four of the gate's ten operators had no coverage at
    # all, and the one that mattered most was the probe those operators now take.
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        build_fixture_crate(root, CRATE_TOKEN_SWAP)
        code, listing = run_gate(root, "--quiet", "--list-only")
        check("list-only exits 0", code == 0, listing[-2000:])
        check(
            "one CMP_FLIP candidate per function",
            listing.count("[CMP_FLIP]") == 3,
            listing,
        )
        code, out = run_gate(root, "--quiet")
        check("exit 1", code == 1, f"exit {code}\n{out[-3000:]}")
        check(
            "the swap in the asserted-on function is killed",
            tier_of_operator(out, "compared_and_checked", "CMP_FLIP") == "KILLED",
            out[-3000:],
        )
        check(
            "the swap the tests execute and do not check is UNPINNED",
            tier_of_operator(out, "compared_but_unchecked", "CMP_FLIP") == "UNPINNED",
            out[-3000:],
        )
        check(
            "the swap in the uncalled function is UNREACHED",
            tier_of_operator(out, "compared_never_called", "CMP_FLIP") == "UNREACHED",
            out[-3000:],
        )


@case("END TO END: the report prints the probe's answer beside every survivor")
def _() -> None:
    # Without this a reader cannot tell a measured survivor from an unmeasured
    # one, and two operators landing on one line under opposite verdicts both
    # read as authoritative.
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        build_fixture_crate(root, CRATE_TOKEN_SWAP)
        code, out = run_gate(root, "--quiet")
        check("a survivor the tests reach says so", "reached by a test: yes" in out, out[-3000:])
        check("a survivor they do not reach says so", "reached by a test: no" in out, out[-3000:])
        survivors = [ln for ln in out.splitlines() if ln.strip().startswith("key:")]
        answers = [ln for ln in out.splitlines() if "reached by a test:" in ln]
        check(
            "one answer printed per survivor",
            len(answers) == len(survivors) == 4,
            (len(answers), len(survivors)),
        )


@case("END TO END: an unmeasured survivor is UNPROBED, never UNPINNED, and blocks")
def _() -> None:
    # Two refusals in one case, and they pull in opposite directions.  Round 1 put
    # unmeasured survivors in the blocking tier under a header asserting the tests
    # execute the line; that header is false and it stays fixed.  Round 2 made the
    # honest tier advisory, and a real defect shipped through it with the job
    # green — `.filter(|(kind, _, _)| *kind == "produces:")` in an asset graph's
    # collision gate, which the tests drive on every load.  "No probe compiles
    # here" is a fact about the line's syntax, not about the tests.
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        build_fixture_crate(root, CRATE_UNPROBEABLE)
        code, out = run_gate(root, "--quiet")
        check(
            "reported UNPROBED",
            tier_of_operator(out, "describe", "CMP_FLIP") == "UNPROBED",
            out[-3000:],
        )
        check(
            "and NOT under a header claiming the tests execute it",
            tier_of_operator(out, "describe", "CMP_FLIP") != "UNPINNED",
            out[-3000:],
        )
        check("the reason is printed", "not in statement position" in out, out[-2000:])
        check("an unmeasured survivor fails the job", code == 1, f"exit {code}\n{out[-3000:]}")
        check(
            "and the failure line counts it",
            "reachability was not measured" in out,
            out[-1000:],
        )
        # The way past it is the registry, the same door a measured equivalent uses.
        keys = [ln.split("key:")[1].strip() for ln in out.splitlines() if "key:" in ln]
        check("a key is printed to paste", len(keys) == 1, keys)
        (root / "scripts").mkdir(exist_ok=True)
        (root / "scripts" / "mutation-coverage-equivalents.txt").write_text(
            f"{keys[0]}\tsrc/lib.rs\tdescribe\tCMP_FLIP\t2026-08-20\tfixture ruling\n"
        )
        code, out = run_gate(root, "--quiet")
        check("a dated ruling clears it", code == 0, f"exit {code}\n{out[-3000:]}")
        check("and the ruling stays visible", "RULED EQUIVALENT" in out, out[-2000:])


@case("a diff that only DELETES lines says so instead of reading as a clean sweep")
def _() -> None:
    # `changed_lines` cannot represent a removed line, so a branch whose whole
    # change is deleting a guard produces no candidates.  Before this the run
    # printed "nothing to mutate" and exited 0, which is the same output as a
    # diff with nothing wrong in it.
    env = {
        **os.environ,
        "GIT_AUTHOR_NAME": "selftest",
        "GIT_AUTHOR_EMAIL": "selftest@example.invalid",
        "GIT_COMMITTER_NAME": "selftest",
        "GIT_COMMITTER_EMAIL": "selftest@example.invalid",
    }
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        (root / "src").mkdir()
        (root / "src" / "x.rs").write_text(
            "fn f(a: u8) -> u8 {\n    if a > 0 {\n        return 1;\n    }\n    0\n}\n"
        )
        for argv in (["init", "-q"], ["add", "-A"], ["commit", "-qm", "base"]):
            subprocess.run(["git", *argv], cwd=root, check=True, env=env, capture_output=True)
        # Take the guard away and change nothing else.
        (root / "src" / "x.rs").write_text("fn f(a: u8) -> u8 {\n    0\n}\n")

        check("the removal is what git sees", mc.deleted_line_count(root, "HEAD", None) == 3)
        check("and nothing was added to mutate", mc.changed_lines(root, "HEAD", None) == {})
        code, out = run_gate(root, "--quiet")
        check("still exits 0", code == 0, f"exit {code}\n{out[-1500:]}")
        check("but the report names the hole", "DELETED 3 line(s)" in out, out[-1500:])
        check("and says what covers it instead", "Code review" in out, out[-1500:])


@case("END TO END: a survivor no test executes is separated out, not listed beside it")
def _() -> None:
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        build_fixture_crate(root)
        code, out = run_gate(root, "--quiet")
        check(
            "uncalled function reported UNREACHED",
            tier_of(out, "never_called") == "UNREACHED",
            out[-3000:],
        )
        check(
            "and NOT in the tier that fails the job",
            tier_of(out, "never_called") != "UNPINNED",
            out[-3000:],
        )


@case("END TO END: UNREACHED alone exits 0, and --strict makes it exit 1")
def _() -> None:
    lib = CRATE_LIB.replace(
        "    fn executed_but_unchecked_is_called_and_never_asserted_on() {\n"
        "        let _ = executed_but_unchecked(b\"\");\n"
        "        let _ = executed_but_unchecked(b\"abc\");\n"
        "    }\n",
        "    fn executed_but_unchecked_is_called_and_never_asserted_on() {\n"
        "        assert_eq!(executed_but_unchecked(b\"\"), 99);\n"
        "        assert_eq!(executed_but_unchecked(b\"abc\"), 3);\n"
        "    }\n",
    )
    check("the fixture edit landed", "assert_eq!(executed_but_unchecked" in lib)
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        build_fixture_crate(root, lib)
        code, out = run_gate(root, "--quiet")
        check("unreached alone does not fail", code == 0, f"exit {code}\n{out[-3000:]}")
        check("but it is still reported", "UNREACHED" in out, out[-2000:])
        code, out = run_gate(root, "--quiet", "--strict")
        check("--strict fails on it", code == 1, f"exit {code}\n{out[-2000:]}")


@case("END TO END: a ruling moves a survivor out of the tier that fails the job")
def _() -> None:
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        build_fixture_crate(root)
        code, out = run_gate(root, "--quiet")
        check("fails before the ruling", code == 1, f"exit {code}")
        keys = [
            line.split("key:")[1].strip()
            for line in out.splitlines()
            if "key:" in line
        ]
        check("keys are printed for the reader to paste", keys, out[-2000:])
        # Rule every survivor the run reported, then re-run.
        (root / "scripts").mkdir(exist_ok=True)
        (root / "scripts" / "mutation-coverage-equivalents.txt").write_text(
            "".join(
                f"{k}\tsrc/lib.rs\tf\tGUARD\t2026-08-19\tfixture ruling\n" for k in keys
            )
        )
        code, out = run_gate(root, "--quiet")
        check("passes once ruled", code == 0, f"exit {code}\n{out[-3000:]}")
        check("and the ruling is still shown", "RULED EQUIVALENT" in out, out[-2000:])
        check("with its reason", "fixture ruling" in out, out[-2000:])


@case("END TO END: a baseline that ran no tests at all is a setup error, not a pass")
def _() -> None:
    # A runner with nothing to run prints the same green as one that ran
    # everything, and every mutation then "survives" for a reason that has
    # nothing to do with the code.  The exit code alone cannot tell the two
    # apart; the count can.
    lib = CRATE_LIB.split("#[cfg(test)]")[0]
    check("the fixture really has no tests", "#[test]" not in lib, lib[-200:])
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        build_fixture_crate(root, lib)
        code, out = run_gate(root, "--quiet")
        check("exit 3", code == 3, f"exit {code}\n{out[-2000:]}")
        check("says why", "executed zero tests" in out, out[-1500:])


@case("END TO END: the budget names what it did not reach instead of dropping it")
def _() -> None:
    # A cap that silently stops is worse than no cap: the report reads as a clean
    # sweep of the diff when most of the diff was never touched.
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        build_fixture_crate(root)
        code, out = run_gate(root, "--quiet", "--max-mutants", "1")
        check("NOT CHECKED section present", "NOT CHECKED" in out, out[-2500:])
        check("it says why", "budget" in out, out[-2500:])
        check("only one mutation ran", "mutations run     1 " in out, out[:900])
        uncapped = run_gate(root, "--quiet")[1]
        check(
            "and uncapped there is nothing left over",
            "NOT CHECKED" not in uncapped,
            uncapped[-2500:],
        )


@case("END TO END: a red baseline is a setup error, not a report")
def _() -> None:
    lib = CRATE_LIB + (
        "\n#[cfg(test)]\nmod broken {\n    #[test]\n"
        "    fn this_test_fails_on_purpose() {\n        assert_eq!(1, 2);\n    }\n}\n"
    )
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        build_fixture_crate(root, lib)
        code, out = run_gate(root, "--quiet")
        check("exit 3", code == 3, f"exit {code}\n{out[-2000:]}")
        check("says why", "not green before any mutation" in out, out[-1500:])


# --------------------------------------------------------------------------- #

def main() -> int:
    if shutil.which("cargo") is None:
        print("SETUP: cargo is not on PATH; the end-to-end cases cannot run.")
        return 3
    if not GATE.exists():
        print(f"SETUP: {GATE} is missing.")
        return 3
    print(f"mutation-coverage self-test: {len(CASES)} cases")
    for name, fn in CASES:
        try:
            fn()
        except Exception as exc:  # noqa: BLE001 - a case failing is the point
            FAILURES.append(f"{name}: {exc}")
            print(f"  FAIL  {name}")
            print(f"        {exc}")
        else:
            print(f"  ok    {name}")
    print()
    if FAILURES:
        print(f"FAIL: {len(FAILURES)} of {len(CASES)} cases failed.")
        return 1
    print(f"PASS: all {len(CASES)} cases passed.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
