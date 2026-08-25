#!/usr/bin/env bash
# Regression test for scripts/check-public-hygiene.sh — the gate's own credibility.
#
# A hygiene gate fails in two directions and both are silent:
#
#   * it stops matching (a broken pattern, a rule that errors, an allowlist entry
#     that no longer suppresses anything) and reports clean while blind;
#   * it starts matching honest prose and gets switched off within the week.
#
# So this file exercises both. It builds a throwaway git repository, drops the
# real checker into it, and asserts on exit codes and output:
#
#   1. every covered shape is caught, named, and located;
#   2. the path rule fires on a document path rooted outside the repository AND
#      stays silent on one rooted inside it. Both directions, and the silent one
#      is not optional: a rule only ever tested red can be loosened until it
#      matches everything and still look tested;
#   3. the tracked innocent-strings fixture produces silence;
#   4. a rule that cannot run is FATAL and names itself, never "clean" — proved
#      separately for the pattern rules and for the path rule, because they run
#      in two loops and so have two copies of that guard;
#   5. an allowlist entry that does not parse is fatal;
#   6. an allowlist entry that suppresses nothing is fatal;
#   7. a well-formed allowlist entry actually suppresses its match — including
#      match text containing a `#`, which the first cut of the format could not
#      express;
#   8. the card-slug rule stays silent on a long kebab run under vendor/**, and
#      fires on the byte-identical text anywhere else. Both directions, for the
#      same reason as 2: a vendor-only exclusion only ever tested silent could
#      quietly widen to swallow the whole tree.
#
# Every violating string below is assembled at runtime from harmless pieces
# (`printf '%s-%s' decision 69`), never written as a literal. This file is
# tracked, so a literal here would be a violation of the very rule it tests.
# For the path rule that constraint binds hardest: the path that prompted it is
# private, so it is not written here in any form, whole or in pieces. The cases
# below are fictional paths, which is all the rule reads — it matches a shape
# against this repository's own contents, never a name.
#
# Usage:  ./scripts/check-public-hygiene-selftest.sh
# Exit:   0 all assertions passed, 1 otherwise.

set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CHECK="$HERE/check-public-hygiene.sh"
FIXTURE="$HERE/public-hygiene-innocent-strings.txt"

[[ -x "$CHECK" ]] || {
	echo "selftest: $CHECK is missing or not executable" >&2
	exit 1
}

failures=0
TMPROOT="$(mktemp -d)"
trap 'rm -rf "$TMPROOT"' EXIT

ok() { printf '  ok    %s\n' "$1"; }
bad() {
	printf '  FAIL  %s\n' "$1"
	failures=$((failures + 1))
}

# A throwaway repo with the real checker and an empty allowlist.
new_repo() {
	local d
	d="$(mktemp -d "$TMPROOT/repo.XXXXXX")"
	git init -q "$d"
	mkdir -p "$d/scripts"
	cp "$CHECK" "$d/scripts/check-public-hygiene.sh"
	: >"$d/scripts/public-hygiene-allowlist.txt"
	printf '%s' "$d"
}

# run_gate <repo> -> prints combined output, returns the gate's exit code.
run_gate() {
	local d="$1"
	git -C "$d" add scripts subject.txt >/dev/null 2>&1
	(cd "$d" && ./scripts/check-public-hygiene.sh 2>&1)
}

# ---------------------------------------------------------------------------
# 1. Every covered shape is caught.
# ---------------------------------------------------------------------------
echo "covered shapes:"

# Each case: <expected label> then the printf format and its harmless pieces.
check_violation() {
	local desc="$1" label="$2" line="$3" d out rc
	d="$(new_repo)"
	printf '%s\n' "$line" >"$d/subject.txt"
	out="$(run_gate "$d")"
	rc=$?
	if [[ $rc -ne 1 ]]; then
		bad "$desc — expected exit 1, got $rc"
		printf '%s\n' "$out" | sed 's/^/        /'
		return
	fi
	if ! printf '%s\n' "$out" | grep -q "^subject.txt:1: $label: "; then
		bad "$desc — no '$label' violation reported at subject.txt:1"
		printf '%s\n' "$out" | sed 's/^/        /'
		return
	fi
	ok "$desc"
}

# The mirror of check_violation: the gate must have nothing at all to say.
check_clean() {
	local desc="$1" line="$2" d out rc
	d="$(new_repo)"
	printf '%s\n' "$line" >"$d/subject.txt"
	out="$(run_gate "$d")"
	rc=$?
	if [[ $rc -ne 0 ]]; then
		bad "$desc — expected exit 0, got $rc"
		printf '%s\n' "$out" | sed 's/^/        /'
		return
	fi
	ok "$desc"
}

check_violation "decision record"           private-decision-record "$(printf 'as recorded in %s-%s.' decision 69)"
check_violation "task id"                   planning-task-id        "$(printf 'tracked as %s-%s.' task 42)"
check_violation "bare ticket ref"           bare-ticket-ref         "$(printf 'shipped under %s%s last week.' T 48)"
check_violation "parenthesised ticket ref"  bare-ticket-ref         "$(printf 'shipped (%s%s) last week.' T 7)"
check_violation "milestone id, capitalised" milestone-id            "$(printf '%s %s%s is the release milestone.' Milestone M -14)"
check_violation "milestone id, lowercase"   milestone-id            "$(printf 'slipped past %s%s entirely.' m -14)"
check_violation "milestone id, word form"   milestone-id            "$(printf 'the %s %s target.' 'milestone' '3')"
check_violation "acceptance criterion"      acceptance-criterion    "$(printf 'satisfies %s%s%s.' AC '#' 2)"
check_violation "acceptance criterion, lc"  acceptance-criterion    "$(printf 'satisfies %s%s%s.' ac '#' 2)"
# The bare form — no space, no "#" at all — added after measurement showed it
# is how this is written everywhere. Assembled as two FLUSH %s with nothing
# between them, so the digits land immediately against the letters at runtime;
# the raw source below has "AC" and "2" as separate, space-separated printf
# arguments, which is what proves the bare pattern requires no internal space
# rather than merely tolerating one.
check_violation "acceptance criterion, bare" acceptance-criterion   "$(printf 'satisfies %s%s.' AC 2)"
check_violation "document id"               private-doc-id          "$(printf 'see %s-%s for the plan.' doc 1)"
check_violation "card id"                   planning-card-id        "$(printf 'shipped %s %s.' 'card' '0015')"
check_violation "spec AC id, bare"          spec-ac-id              "$(printf 'covered by %s-%s.' ac 09)"
check_violation "spec AC id, prefixed"      spec-ac-id              "$(printf 'covered by %s-%s%s.' clg ac 09)"
# The two shapes the gate used to exempt on purpose: a spec AC id embedded in a
# Rust identifier, and one embedded in a temp-dir literal. They are renamed now,
# not exempted, so the gate must see them.
check_violation "spec AC id in a fn name"   spec-ac-id              "$(printf 'fn %s_%s%s_logs_the_command() {}' clg ac 09)"
check_violation "spec AC id in a literal"   spec-ac-id              "$(printf 'let dir = "bf-%s-%s%s-1234";' clg ac 09)"

# A full card slug — a long kebab-case title, not a number — in the shape it is
# actually written: seven words here, well past the SIX-segment floor the rule
# requires. The words are ordinary, space-separated printf arguments; only the
# format string supplies the hyphens, so the raw source line never itself
# contains a run of hyphen-joined words for the rule to trip on.
check_violation "card slug"                 card-slug               "$(printf '%s-%s-%s-%s-%s-%s-%s.' widening the hygiene gate catches more shapes)"

# A determiner directly against the singular noun for a tracker item, with no
# number and no slug — the shape that leaked in a runtime AssertionError and a
# committed JSON value in the same session that found the gaps above. Each
# piece is quoted on its own so the raw source line never contains the two
# words adjacent to each other; only the assembled runtime string does.
check_violation "card noun reference"       card-noun-reference     "$(printf 'consult %s %s for background.' 'the' 'card')"

# ---------------------------------------------------------------------------
# 2. The path rule, both directions.
#
# A throwaway repo's only top-level entries are `scripts` and `subject.txt`, so
# any other leading segment is foreign to it — the same relation the gate applies
# to the real tree, at a size small enough to state in one sentence.
# ---------------------------------------------------------------------------
echo "document paths rooted outside the repository:"

check_violation "foreign path, backticked" cross-repo-doc-path \
	"$(printf 'Design spec: `%s/%s/%s.md`.' elsewhere notes typed-things)"
check_violation "foreign path, bare prose" cross-repo-doc-path \
	"$(printf 'see %s/%s.md for the rationale' otherproject rationale)"
check_violation "foreign path, climbing out of the root" cross-repo-doc-path \
	"$(printf 'see %s/%s/%s.md' '..' sibling design)"

# The header commits every rule but `bare-ticket-ref` to case-insensitivity and
# calls a case-sensitive rule "a hole by default". Before `(?i)` was added to
# PATH_PATTERN these three exited 0 while the lowercase spelling was caught, so
# they pin the promise the file makes about itself rather than a preference.
check_violation "foreign path, uppercase extension" cross-repo-doc-path \
	"$(printf 'see %s/%s.MD for the rationale' otherproject rationale)"
check_violation "foreign path, title-case extension" cross-repo-doc-path \
	"$(printf 'see %s/%s.Md for the rationale' otherproject rationale)"
check_violation "foreign path, mixed-case extension" cross-repo-doc-path \
	"$(printf 'see %s/%s.mD for the rationale' otherproject rationale)"

echo "document paths rooted inside the repository:"

check_clean "a path under a directory this repo has" \
	"$(printf 'see %s/%s.md' scripts guide)"
check_clean "a deeper path under one of ours" \
	"$(printf 'see %s/%s/%s.md' scripts inner guide)"
check_clean "a documentation URL is not a repository path" \
	"$(printf 'see https://example.com/%s/%s.md' docs guide)"
check_clean "a bare filename is not a path" \
	"$(printf 'see %s.md at the root' README)"

# A relative link written from inside a subdirectory at a file that is really
# there. This is the case the leading-segment reading alone gets wrong: read from
# the repository root the path climbs out of the repository entirely, and only
# resolving it against the citing file's own directory shows it landing on a
# tracked file. Delete that second reading and this case goes red.
d="$(new_repo)"
mkdir -p "$d/scripts/inner"
: >"$d/scripts/guide.md"
: >"$d/subject.txt"
printf 'see %s\n' "$(printf '%s/%s/%s.md' '../..' scripts guide)" >"$d/scripts/inner/ref.txt"
out="$(run_gate "$d")"
rc=$?
if [[ $rc -eq 0 ]]; then
	ok "a relative link resolving to a tracked file stays silent"
else
	bad "a valid relative link tripped the gate (exit $rc)"
	printf '%s\n' "$out" | sed 's/^/        /'
fi

# The same shape pointing at a file that is NOT there resolves to nothing under
# either reading, which is what a path into another checkout looks like from
# here. Without this the case above could be passing because the rule stopped
# matching rather than because the link is good.
d="$(new_repo)"
mkdir -p "$d/scripts/inner"
: >"$d/subject.txt"
printf 'see %s\n' "$(printf '%s/%s/%s.md' '../..' scripts guide)" >"$d/scripts/inner/ref.txt"
out="$(run_gate "$d")"
rc=$?
if [[ $rc -eq 1 ]] && printf '%s\n' "$out" | grep -q "^scripts/inner/ref.txt:1: cross-repo-doc-path: "; then
	ok "the same relative link with no file behind it is a violation"
else
	bad "a relative link resolving to nothing was not caught (exit $rc)"
	printf '%s\n' "$out" | sed 's/^/        /'
fi

echo "card slugs, at and below the threshold:"

# Five segments — one short of the SIX the rule requires — stays silent. Words
# are ordinary space-separated printf arguments; the format string alone
# supplies the hyphens, same convention as the "card slug" violation above.
check_clean "a five-segment kebab run is one short of a card slug" \
	"$(printf '%s-%s-%s-%s-%s value' alpha bravo charlie delta echo)"

# The determiner rule requires the noun immediately after "this"/"that"/"the";
# the plural must stay silent without an explicit negative lookahead.
check_clean "the plural noun is not a singular-card reference" \
	"$(printf 'consult %s %s for background.' 'the' 'cards')"

# ---------------------------------------------------------------------------
# 8. card-slug's vendor/** exclusion, both directions. `run_gate`'s helper
# only stages `scripts` and `subject.txt`, so these two cases stage `vendor`
# themselves rather than going through it.
# ---------------------------------------------------------------------------
echo "card slugs, the vendor/** exclusion:"

slug_text="$(printf '%s-%s-%s-%s-%s-%s-%s' upstream reference doc path for the feature)"

d="$(new_repo)"
mkdir -p "$d/vendor/somecrate/src"
printf 'see %s\n' "$slug_text" >"$d/vendor/somecrate/src/lib.rs"
: >"$d/subject.txt"
git -C "$d" add scripts subject.txt vendor >/dev/null 2>&1
out="$(cd "$d" && ./scripts/check-public-hygiene.sh 2>&1)"
rc=$?
if [[ $rc -eq 0 ]]; then
	ok "a long kebab run under vendor/ stays silent"
else
	bad "a long kebab run under vendor/ tripped the gate (exit $rc)"
	printf '%s\n' "$out" | sed 's/^/        /'
fi

d="$(new_repo)"
printf 'see %s\n' "$slug_text" >"$d/subject.txt"
out="$(run_gate "$d")"
rc=$?
if [[ $rc -eq 1 ]] && printf '%s\n' "$out" | grep -q "^subject.txt:1: card-slug: "; then
	ok "the byte-identical text outside vendor/ is caught"
else
	bad "the same text outside vendor/ was not caught (exit $rc)"
	printf '%s\n' "$out" | sed 's/^/        /'
fi

# ---------------------------------------------------------------------------
# 3. The innocent-strings fixture stays silent.
# ---------------------------------------------------------------------------
echo "innocent strings:"
d="$(new_repo)"
cp "$FIXTURE" "$d/subject.txt"
out="$(run_gate "$d")"
rc=$?
if [[ $rc -eq 0 ]]; then
	ok "fixture of innocent strings produces no violations"
else
	bad "fixture of innocent strings tripped the gate (exit $rc)"
	printf '%s\n' "$out" | sed 's/^/        /'
fi

# ---------------------------------------------------------------------------
# 4. A rule that cannot run is fatal and names itself.
# ---------------------------------------------------------------------------
echo "broken rule:"
d="$(new_repo)"
printf '%s\n' "$(printf 'as recorded in %s-%s.' decision 69)" >"$d/subject.txt"
# Corrupt exactly one rule's pattern into something PCRE rejects: an unclosed
# group. git grep then exits 128 and prints nothing to stdout — indistinguishable
# from "no violations" unless the exit code is checked per rule.
perl -0pi -e 's/\Qdecision[-_][0-9]+\E/decision-[0-9]+(/' "$d/scripts/check-public-hygiene.sh"
out="$(run_gate "$d")"
rc=$?
if [[ $rc -eq 2 ]] && printf '%s\n' "$out" | grep -q "RULE FAILED TO RUN" &&
	printf '%s\n' "$out" | grep -q "private-decision-record"; then
	ok "a broken pattern is fatal and names the rule"
else
	bad "a broken pattern did not fail loudly (exit $rc)"
	printf '%s\n' "$out" | sed 's/^/        /'
fi

# The path rule scans in a loop of its own, so the guard above is duplicated
# code and gets its own proof. Corrupt the tail of its pattern into an unclosed
# group: git grep then exits 128 having printed nothing, and a gate that reads
# the output before the exit code calls that clean.
d="$(new_repo)"
printf '%s\n' "$(printf 'see %s/%s/%s.md' elsewhere notes design)" >"$d/subject.txt"
perl -0pi -e 's/\Q\.md(?![A-Za-z0-9])\E/\\.md(/' "$d/scripts/check-public-hygiene.sh"
out="$(run_gate "$d")"
rc=$?
if [[ $rc -eq 2 ]] && printf '%s\n' "$out" | grep -q "RULE FAILED TO RUN" &&
	printf '%s\n' "$out" | grep -q "cross-repo-doc-path"; then
	ok "a broken path pattern is fatal and names the rule"
else
	bad "a broken path pattern did not fail loudly (exit $rc)"
	printf '%s\n' "$out" | sed 's/^/        /'
fi

# ---------------------------------------------------------------------------
# 5-7. Allowlist behaviour.
# ---------------------------------------------------------------------------
echo "allowlist:"

# The match text for an acceptance criterion contains a `#`. Under a format that
# split on the first `#`, this entry silently parsed as nonsense and suppressed
# nothing. It must round-trip.
ac_text="$(printf '%s%s%s' AC '#' 2)"
ac_line="$(printf 'satisfies %s.' "$ac_text")"

# 5. Unparseable entry.
d="$(new_repo)"
printf '%s\n' "$ac_line" >"$d/subject.txt"
printf 'subject.txt | %s\n' "$ac_text" >"$d/scripts/public-hygiene-allowlist.txt"
out="$(run_gate "$d")"
rc=$?
if [[ $rc -eq 2 ]] && printf '%s\n' "$out" | grep -q "expected 3 '|'-separated fields"; then
	ok "an entry that does not parse is a hard error"
else
	bad "an unparseable allowlist entry was not rejected (exit $rc)"
	printf '%s\n' "$out" | sed 's/^/        /'
fi

# 5b. Parses, but the explanation is empty.
d="$(new_repo)"
printf '%s\n' "$ac_line" >"$d/subject.txt"
printf 'subject.txt | %s |\n' "$ac_text" >"$d/scripts/public-hygiene-allowlist.txt"
out="$(run_gate "$d")"
rc=$?
if [[ $rc -eq 2 ]] && printf '%s\n' "$out" | grep -q "all required"; then
	ok "an entry with no written reason is a hard error"
else
	bad "a reasonless allowlist entry was not rejected (exit $rc)"
	printf '%s\n' "$out" | sed 's/^/        /'
fi

# 6. Stale entry — well formed, matches nothing.
d="$(new_repo)"
printf '%s\n' "$ac_line" >"$d/subject.txt"
printf 'subject.txt | %s | quoted from a spec that has since moved\n' \
	"$(printf '%s%s%s' AC '#' 3)" >"$d/scripts/public-hygiene-allowlist.txt"
out="$(run_gate "$d")"
rc=$?
if [[ $rc -eq 2 ]] && printf '%s\n' "$out" | grep -q "stale entry"; then
	ok "an entry that suppresses nothing is a hard error"
else
	bad "a stale allowlist entry was not rejected (exit $rc)"
	printf '%s\n' "$out" | sed 's/^/        /'
fi

# 7. Legitimate entry — must actually suppress, including the `#` in the text.
d="$(new_repo)"
printf '%s\n' "$ac_line" >"$d/subject.txt"
printf 'subject.txt | %s | quoted verbatim from a public upstream issue\n' \
	"$ac_text" >"$d/scripts/public-hygiene-allowlist.txt"
out="$(run_gate "$d")"
rc=$?
if [[ $rc -eq 0 ]] && printf '%s\n' "$out" | grep -q "1 allowlisted match"; then
	ok "a well-formed entry suppresses its match, '#' and all"
else
	bad "a legitimate allowlist entry did not suppress (exit $rc)"
	printf '%s\n' "$out" | sed 's/^/        /'
fi

echo
if [[ $failures -eq 0 ]]; then
	echo "check-public-hygiene-selftest: all assertions passed."
	exit 0
fi
echo "check-public-hygiene-selftest: $failures assertion(s) failed."
exit 1
