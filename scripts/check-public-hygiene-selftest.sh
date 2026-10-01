#!/usr/bin/env bash
# Regression test for scripts/check-public-hygiene.sh.
#
# Builds scratch git repositories laid out like a repository that carries the
# gate, copies in the gate, its rules file and its innocent-strings fixture from
# the directory this file sits in, plants one string per case, and asserts the
# gate's exit code and what it reports. Run it from anywhere:
#
#   ./scripts/check-public-hygiene-selftest.sh [--self-test]
#
# `--self-test` is accepted and changes nothing, so a runner that hands the flag
# to every proof it runs can run this one. Any other argument exits 2.
#
# Exit 0 when every case holds, 1 when any fails, naming each.
#
# The planted strings are written here with a `^` inside them, removed when the
# case file is written, so this file carries no identifier the gate would catch
# in it: the gate scans this file like any other tracked file.
set -uo pipefail

case "${1:-}" in
"" | --self-test) ;;
*)
	echo "selftest: unknown argument '$1'; usage: $0 [--self-test]" >&2
	exit 2
	;;
esac

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
GATE="$HERE/check-public-hygiene.sh"
RULES="$HERE/public-hygiene-rules.txt"
INNOCENT="$HERE/public-hygiene-innocent-strings.txt"
for f in "$GATE" "$RULES" "$INNOCENT"; do
	[[ -f "$f" ]] || { echo "selftest: $f is missing" >&2; exit 1; }
done

work="$(mktemp -d)" || exit 1
trap 'rm -rf "$work"' EXIT
failures=0
checks=0

check() {
	checks=$((checks + 1))
	if [[ "$2" != "ok" ]]; then
		echo "FAIL: $1" >&2
		[[ -n "${3:-}" && -f "$3" ]] && sed 's/^/    /' "$3" | head -20 >&2
		failures=$((failures + 1))
	fi
}

# new_repo <dir> [rules]: a scratch repository holding the gate, a rules file and
# the innocent-strings fixture under scripts/.
new_repo() {
	local d="$1" rules="${2:-$RULES}"
	rm -rf "$d"
	mkdir -p "$d/scripts"
	cp "$GATE" "$d/scripts/check-public-hygiene.sh"
	cp "$rules" "$d/scripts/public-hygiene-rules.txt"
	cp "$INNOCENT" "$d/scripts/public-hygiene-innocent-strings.txt"
	git -C "$d" init -q
}

# run_gate <dir>: runs the gate there; output in <dir>.out, exit code in $rc.
run_gate() {
	(cd "$1" && git add -A . >/dev/null 2>&1 && bash scripts/check-public-hygiene.sh) >"$1.out" 2>&1
	rc=$?
}

# ---------------------------------------------------------------------------
# The cases: "<label>|<text>", `^` removed and `\n` read as a line break. One
# per rule line, in the order of the rules file, each chosen so that no other
# line with the same label catches it; then the path rule; then the wrapped
# cases, whose word and number sit on two lines.
# ---------------------------------------------------------------------------
CASES=(
	'private-decision-record|see deci^sion-12 for why'
	'planning-task-id|blocked on ta^sk-42 today'
	'bare-ticket-ref|fixed in T^12 last week'
	'bare-ticket-ref|fixed last week (T^7)'
	'milestone-id|slipped to m-^27 again'
	'milestone-id|the mile^stone 4 date'
	'acceptance-criterion|meets A^C #3 now'
	'acceptance-criterion|meets A^C3 now'
	'private-doc-id|as do^c-7 says'
	'planning-card-id|ca^rd 1234 is open'
	'spec-ac-id|the test_load_a^c3 test'
	'spec-ac-id|see a^c-3 there'
	'card-slug|per fix-the-loader-^when-the-column-moves'
	'card-noun-reference|as this ca^rd says'
	'vault-wikilink|see [[some-^note]]'
	'vault-ledger-path|under decisions/one-^two-three'
	'planning-choice-id|per choi^ce 0012'
	'dated-spec-slug|spec 2026-06-20-sur^vey-detection-and-load'
	'review-finding-ref|review-spec find^ing 3 covers it'
	'review-finding-ref|as agreed (find^ing 7).'
	'cross-repo-doc-path|see ../other/notes.m^d'
	'planning-choice-id|consumes it as a library (choi^ce\n0012): the rest'
	'planning-choice-id|/// which rung was chosen (choi^ce\n/// 0004); the rest'
	'milestone-id|# the work slipped past mile^stone\n# 12 and on'
)

write_cases() {
	local d="$1" i=0 entry text
	mkdir -p "$d/cases"
	for entry in "${CASES[@]}"; do
		i=$((i + 1))
		text="${entry#*|}"
		printf '%b\n' "${text//^/}" >"$d/cases/$(printf '%02d' "$i").txt"
	done
}

# "<file> <label>" for every report line in a gate's output, sorted.
reported_pairs() {
	sed -nE 's/^([^ :]+):[0-9]+: ([a-z-]+): .*/\1 \2/p' "$1" | sort -u
}

# 1. The canonical files alone, with no allowlist, are clean: the gate, its
#    rules, the fixture and this file name nothing the rules catch.
r="$work/clean"
new_repo "$r"
cp "${BASH_SOURCE[0]}" "$r/scripts/check-public-hygiene-selftest.sh"
run_gate "$r"
[[ $rc -eq 0 ]] && res=ok || res=bad
check "the canonical files alone exit 0 (got $rc)" "$res" "$r.out"

# 2. Every case is reported, on line 1 of its file, under its label.
r="$work/cases"
new_repo "$r"
write_cases "$r"
run_gate "$r"
[[ $rc -eq 1 ]] && res=ok || res=bad
check "the planted cases exit 1 (got $rc)" "$res" "$r.out"
i=0
for entry in "${CASES[@]}"; do
	i=$((i + 1))
	f="cases/$(printf '%02d' "$i").txt"
	label="${entry%%|*}"
	grep -q "^$f:1: $label: " "$r.out" && res=ok || res=bad
	check "case $i is reported as $f:1: $label" "$res" "$r.out"
done
cp "$r.out" "$work/full.out"

# 3. Every rule line is necessary: without it, some case is no longer reported
#    under its label. A line added with no case of its own fails here.
reported_pairs "$work/full.out" >"$work/full.pairs"
n=0
while IFS= read -r raw || [[ -n "$raw" ]]; do
	n=$((n + 1))
	[[ -z "${raw//[[:space:]]/}" || "${raw#"${raw%%[![:space:]]*}"}" == \#* ]] && continue
	awk -v skip="$n" 'NR != skip' "$RULES" >"$work/rules-minus"
	r="$work/minus"
	new_repo "$r" "$work/rules-minus"
	write_cases "$r"
	run_gate "$r"
	reported_pairs "$r.out" >"$work/minus.pairs"
	cmp -s "$work/full.pairs" "$work/minus.pairs" && res=bad || res=ok
	check "rules line $n has a case no other line of its label catches: ${raw%%|*}" "$res"
done <"$RULES"

# 4. A malformed rules line and a pattern that does not compile each stop the
#    gate with exit 2 rather than letting it report clean.
r="$work/malformed"
printf 'a-label-with-no-pattern\n' >"$work/rules-bad"
new_repo "$r" "$work/rules-bad"
run_gate "$r"
[[ $rc -eq 2 ]] && res=ok || res=bad
check "a rules line with no '|' exits 2 (got $rc)" "$res" "$r.out"

printf 'broken|(?<!unclosed\n' >"$work/rules-bad"
new_repo "$r" "$work/rules-bad"
run_gate "$r"
[[ $rc -eq 2 ]] && res=ok || res=bad
check "a pattern that does not compile exits 2 (got $rc)" "$res" "$r.out"

# 5. An allowlist entry suppresses exactly its text; a stale one exits 2.
r="$work/allow"
new_repo "$r"
printf '%s\n' "per choi^ce 0012" | tr -d '^' >"$r/kept.txt"
printf 'kept.txt | %s | quoted from an upstream changelog\n' "$(cat "$r/kept.txt" | sed 's/^per //')" >"$r/scripts/public-hygiene-allowlist.txt"
run_gate "$r"
[[ $rc -eq 0 ]] && grep -q 'allowlisted' "$r.out" && res=ok || res=bad
check "an allowlisted match is suppressed and counted (got $rc)" "$res" "$r.out"
rm "$r/kept.txt"
run_gate "$r"
[[ $rc -eq 2 ]] && res=ok || res=bad
check "a stale allowlist entry exits 2 (got $rc)" "$res" "$r.out"

# 6. The path rule clears a path rooted at a top-level entry of the repository.
r="$work/path"
new_repo "$r"
printf 'see scripts/guide.m^d\n' | tr -d '^' >"$r/doc.txt"
run_gate "$r"
[[ $rc -eq 0 ]] && res=ok || res=bad
check "a path rooted in this repository is clean (got $rc)" "$res" "$r.out"

if [[ $failures -gt 0 ]]; then
	echo "check-public-hygiene-selftest: $failures of $checks check(s) failed" >&2
	exit 1
fi
echo "check-public-hygiene-selftest: all $checks checks passed"
exit 0
