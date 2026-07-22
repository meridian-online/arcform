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
#   2. the tracked innocent-strings fixture produces silence;
#   3. a rule that cannot run is FATAL and names itself, never "clean";
#   4. an allowlist entry that does not parse is fatal;
#   5. an allowlist entry that suppresses nothing is fatal;
#   6. a well-formed allowlist entry actually suppresses its match — including
#      match text containing a `#`, which the first cut of the format could not
#      express.
#
# Every violating string below is assembled at runtime from harmless pieces
# (`printf '%s-%s' decision 69`), never written as a literal. This file is
# tracked, so a literal here would be a violation of the very rule it tests.
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

check_violation "decision record"           private-decision-record "$(printf 'as recorded in %s-%s.' decision 69)"
check_violation "task id"                   planning-task-id        "$(printf 'tracked as %s-%s.' task 42)"
check_violation "bare ticket ref"           bare-ticket-ref         "$(printf 'shipped under %s%s last week.' T 48)"
check_violation "parenthesised ticket ref"  bare-ticket-ref         "$(printf 'shipped (%s%s) last week.' T 7)"
check_violation "milestone id, capitalised" milestone-id            "$(printf '%s %s%s is the release milestone.' Milestone M -14)"
check_violation "milestone id, lowercase"   milestone-id            "$(printf 'slipped past %s%s entirely.' m -14)"
check_violation "milestone id, word form"   milestone-id            "$(printf 'the %s %s target.' 'milestone' '3')"
check_violation "acceptance criterion"      acceptance-criterion    "$(printf 'satisfies %s%s%s.' AC '#' 2)"
check_violation "acceptance criterion, lc"  acceptance-criterion    "$(printf 'satisfies %s%s%s.' ac '#' 2)"
check_violation "document id"               private-doc-id          "$(printf 'see %s-%s for the plan.' doc 1)"
check_violation "card id"                   planning-card-id        "$(printf 'shipped %s %s.' 'card' '0015')"
check_violation "spec AC id, bare"          spec-ac-id              "$(printf 'covered by %s-%s.' ac 09)"
check_violation "spec AC id, prefixed"      spec-ac-id              "$(printf 'covered by %s-%s%s.' clg ac 09)"
# The two shapes the gate used to exempt on purpose: a spec AC id embedded in a
# Rust identifier, and one embedded in a temp-dir literal. They are renamed now,
# not exempted, so the gate must see them.
check_violation "spec AC id in a fn name"   spec-ac-id              "$(printf 'fn %s_%s%s_logs_the_command() {}' clg ac 09)"
check_violation "spec AC id in a literal"   spec-ac-id              "$(printf 'let dir = "bf-%s-%s%s-1234";' clg ac 09)"

# ---------------------------------------------------------------------------
# 2. The innocent-strings fixture stays silent.
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
# 3. A rule that cannot run is fatal and names itself.
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

# ---------------------------------------------------------------------------
# 4-6. Allowlist behaviour.
# ---------------------------------------------------------------------------
echo "allowlist:"

# The match text for an acceptance criterion contains a `#`. Under a format that
# split on the first `#`, this entry silently parsed as nonsense and suppressed
# nothing. It must round-trip.
ac_text="$(printf '%s%s%s' AC '#' 2)"
ac_line="$(printf 'satisfies %s.' "$ac_text")"

# 4. Unparseable entry.
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

# 5. Stale entry — well formed, matches nothing.
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

# 6. Legitimate entry — must actually suppress, including the `#` in the text.
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
