#!/usr/bin/env bash
# Public-hygiene gate: stop private planning identifiers reaching this public repo.
#
# This repository is public. The planning that drives it is not. Identifiers that
# only resolve inside the private planning tracker — decision records, task ids,
# milestone ids, acceptance-criterion shorthand, document ids, card ids, spec AC
# ids — are meaningless to anyone reading this repo and leak the shape of private
# work. They have reached main repeatedly under a convention-only rule. This is
# the enforcement: it runs in CI, and it is meant to be the first job to go red.
#
# Usage (no arguments, from anywhere inside the repo):
#
#   ./scripts/check-public-hygiene.sh
#
# Exit codes:
#   0  clean
#   1  one or more violations found
#   2  the gate could not run correctly — a broken rule, a malformed allowlist
#      entry, a stale allowlist entry, or a git without PCRE. ALWAYS a hard
#      failure: a gate that cannot run must never look like a gate that passed.
#
# Its own regression test is scripts/check-public-hygiene-selftest.sh.
#
# What this covers, and what it does NOT
# --------------------------------------
# COVERED: the content of TRACKED files in the current checkout, via `git grep`.
# Build artifacts, target/, node modules and anything else ignored are invisible
# by construction, so a dirty working tree full of generated files can never make
# the gate cry wolf.
#
# NOT COVERED, and you have to watch these yourself:
#   * commit messages,
#   * PR titles, PR descriptions and review comments,
#   * branch names,
#   * anything in git history that is no longer in the current tree,
#   * issues, releases, wiki, and everything else that lives on the forge rather
#     than in the repository.
# Those are real leak vectors here — historically among the commonest — and
# nothing in this file inspects them.
#
# ---------------------------------------------------------------------------
# On false positives
# ---------------------------------------------------------------------------
# A gate that cries wolf gets disabled within a week, which is worse than no
# gate. Every pattern below was measured against the real tree before it was
# committed, and scripts/public-hygiene-innocent-strings.txt is a tracked fixture
# of innocent-but-similar-looking strings the gate must stay silent on. That
# fixture is scanned like any other tracked file, so a pattern that starts biting
# real-world prose or Rust turns the gate red on its own fixture.
#
# On case: every rule is case-INSENSITIVE except `bare-ticket-ref`, and that one
# exception is deliberate — a lowercase t-plus-two-digits is a common substring
# of hex digests and identifiers, and matching it would drown the gate. Every
# other shape leaks in prose, where sentence-initial capitalisation is the most
# likely form, so anything that is case-sensitive there is a hole by default.
#
# The non-obvious guards, and what they are protecting:
#
# * Bare ticket refs. A bare capital-T-plus-digits collides head-on with Rust
#   generic parameters. Guards: TWO OR MORE digits (real ticket ids here are
#   two-digit; generic params are effectively always single-digit), and
#   lookarounds excluding the type positions generics appear in — after `<`,
#   `,`, `&`, `(`, after a colon-space and after an arrow-space; and before `>`,
#   `,`, `:`, `(`. Excluding a preceding digit is what keeps ISO timestamps
#   (`...-20T10:30:00`) out. A single-digit ticket ref is caught only in its
#   parenthesised form, and that form requires the `(` not to follow an
#   identifier, so a Rust `Fn(T<NN>)` is left alone.
#   Known residual: a multi-digit generic in a tuple type's non-first position
#   still fires. That was the price of catching a comma-separated list of ticket
#   refs in prose, which is the commoner leak.
#
# * Milestone ids. The bare form needs TWO OR MORE digits and rejects a
#   preceding `/`, `-` or `_`. That is what keeps URL path segments
#   (`https://example.com/m-2/spec`), prose like `Matrix cell m-1`, and CSS
#   custom properties in the token pipeline (`--m-accent-bg`, `--m-2`) out. The
#   single-digit form is caught only when the word "milestone" is right there.
#
# * Acceptance criteria. Case-INSENSITIVE, but a match may not be followed by a
#   hex digit — that is what keeps lockfile git revisions out, where a rev can
#   end `...ac#4afea48...`.
#
# * Spec AC ids. Deliberately NOT guarded against being part of a larger
#   identifier: test-function names and temp-dir literals that embed a spec AC id
#   are exactly the leak, and they are being renamed rather than exempted. The
#   `[-_]` before `ac` is load-bearing for a different reason: without it the
#   pattern matches inside hex digests (`fbac503`, `dedcac1`), of which this tree
#   has hundreds.
#
# * Card ids. Three or four digits required; the workflow vocabulary word "card"
#   and the literal UI cards in the renderer carry no number and are left alone.
#
# If a pattern flags something legitimate, FIX THE PATTERN and add the innocent
# string to the fixture. The allowlist is for genuine content that must stay,
# not for papering over a bad regex.
# ---------------------------------------------------------------------------

set -uo pipefail

# Run from the repo root regardless of where the caller invoked us.
REPO_ROOT="$(git rev-parse --show-toplevel)" || {
	echo "check-public-hygiene: not inside a git repository" >&2
	exit 2
}
cd "$REPO_ROOT" || exit 2

ALLOWLIST="scripts/public-hygiene-allowlist.txt"

# The rules use PCRE lookarounds, which git only offers when it was built with
# PCRE. Fail loudly rather than silently matching nothing — a gate that quietly
# no-ops is the failure mode this whole file exists to prevent.
# git grep exits 0 on a match, 1 on no match, and >1 on an error such as "cannot
# use Perl-compatible regexes when not compiled with USE_LIBPCRE".
git grep -qP -e 'zzzz(?<!qqqq)' -- . >/dev/null 2>&1
pcre_rc=$?
if [[ $pcre_rc -gt 1 ]]; then
	echo "check-public-hygiene: this git cannot run PCRE patterns (git grep -P exited $pcre_rc)" >&2
	echo "    install a git built with PCRE, or the gate cannot run" >&2
	exit 2
fi

# ---------------------------------------------------------------------------
# Rules: "<label>|<PCRE>". Anything git grep -P accepts.
#
# Several labels carry more than one pattern. Matches are deduplicated per
# (label, file, line), so one offending line is reported once per rule even when
# two of that rule's patterns fire on it.
#
# A pattern may never match a `|`: the allowlist format is pipe-separated and
# relies on matched text being unable to collide with the separator. Everything
# else the patterns can emit — `#`, `-`, `_`, spaces, parentheses — is fine.
# ---------------------------------------------------------------------------
RULES=(
	'private-decision-record|(?i)(?<![A-Za-z0-9])decision[-_][0-9]+'
	'planning-task-id|(?i)(?<![A-Za-z0-9])task[-_][0-9]+'
	'bare-ticket-ref|(?<![-_A-Za-z0-9<,:&(])(?<!: )(?<!-> )T[0-9]{2,3}(?![-_A-Za-z0-9>,:(])'
	'bare-ticket-ref|(?<![-_A-Za-z0-9>])\(T[0-9]{1,3}\)'
	'milestone-id|(?i)(?<![-_A-Za-z0-9/])m-[0-9]{2,}(?![-_A-Za-z0-9])'
	'milestone-id|(?i)(?<![A-Za-z0-9])milestones?[ _-](?:m[-_]?)?[0-9]+(?![-_A-Za-z0-9])'
	'acceptance-criterion|(?i)(?<![A-Za-z0-9])ac ?#[0-9]+(?![0-9a-f])'
	'private-doc-id|(?i)(?<![A-Za-z0-9])doc[-_][0-9]+(?![A-Za-z])'
	'planning-card-id|(?i)(?<![A-Za-z0-9])cards?[ _-]#?[0-9]{3,4}(?![0-9])'
	'spec-ac-id|(?i)[a-z]{2,6}[-_]ac[-_]?[0-9]+[a-z]?(?![0-9])'
	'spec-ac-id|(?i)(?<![A-Za-z0-9])ac[-_][0-9]+[a-z]?(?![0-9])'
)

# ---------------------------------------------------------------------------
# Allowlist.
#
# Format, one entry per line, THREE pipe-separated fields:
#
#     <tracked/file/path> | <exact offending text> | <why this is legitimate>
#
# `|` is the separator precisely because no rule pattern can ever match a `|`,
# so the offending text — which routinely contains `#`, `-` and `_` — can never
# collide with the separator. All three fields are required and none may be
# empty. Anything that does not parse is a hard error (exit 2), so the escape
# hatch cannot be used silently or reached by accident.
#
# Line numbers are deliberately NOT part of an entry — they drift on every edit.
# Instead every entry must MATCH SOMETHING: an entry that suppresses nothing is
# also a hard error, so a stale allowlist cannot rot into fake coverage.
#
# Blank lines and whole-line `#` comments are ignored.
# ---------------------------------------------------------------------------
declare -a ALLOW_PATH=()
declare -a ALLOW_TEXT=()
declare -a ALLOW_LINENO=()
declare -a ALLOW_HITS=()

trim() {
	local s="$1"
	s="${s#"${s%%[![:space:]]*}"}"
	s="${s%"${s##*[![:space:]]}"}"
	printf '%s' "$s"
}

if [[ -f "$ALLOWLIST" ]]; then
	lineno=0
	bad_allow=0
	while IFS= read -r raw || [[ -n "$raw" ]]; do
		lineno=$((lineno + 1))
		# Strip a trailing CR, in case someone edits on Windows.
		line="${raw%$'\r'}"
		trimmed="$(trim "$line")"
		[[ -z "$trimmed" ]] && continue
		[[ "$trimmed" == \#* ]] && continue

		# Split on `|` into exactly three fields.
		IFS='|' read -r -a fields <<<"$line"
		if [[ ${#fields[@]} -ne 3 ]]; then
			echo "check-public-hygiene: $ALLOWLIST:$lineno: expected 3 '|'-separated fields, got ${#fields[@]}" >&2
			echo "    $line" >&2
			echo "    format: <tracked/file/path> | <exact offending text> | <why this is legitimate>" >&2
			bad_allow=1
			continue
		fi
		a_path="$(trim "${fields[0]}")"
		a_text="$(trim "${fields[1]}")"
		a_reason="$(trim "${fields[2]}")"
		if [[ -z "$a_path" || -z "$a_text" || -z "$a_reason" ]]; then
			echo "check-public-hygiene: $ALLOWLIST:$lineno: path, text and explanation are all required" >&2
			echo "    $line" >&2
			bad_allow=1
			continue
		fi
		ALLOW_PATH+=("$a_path")
		ALLOW_TEXT+=("$a_text")
		ALLOW_LINENO+=("$lineno")
		ALLOW_HITS+=(0)
	done <"$ALLOWLIST"
	if [[ $bad_allow -ne 0 ]]; then
		exit 2
	fi
fi

ALLOW_COUNT=${#ALLOW_PATH[@]}

# Returns 0 — and marks the entry used — when this file/text pair is allowlisted.
is_allowed() {
	local file="$1" text="$2" i
	for ((i = 0; i < ALLOW_COUNT; i++)); do
		if [[ "${ALLOW_PATH[$i]}" == "$file" && "${ALLOW_TEXT[$i]}" == "$text" ]]; then
			ALLOW_HITS[i]=$((ALLOW_HITS[i] + 1))
			return 0
		fi
	done
	return 1
}

# ---------------------------------------------------------------------------
# Scan.
# ---------------------------------------------------------------------------
violations=0
allowed=0
# Newline-delimited "<label>:<file>:<line>" keys already reported. A plain string
# rather than an associative array on purpose: macOS still ships bash 3.2, which
# has no `declare -A`, and this gate has to run on a developer's machine as
# readily as on CI.
seen_keys=""

hits="$(mktemp)" || exit 2
errs="$(mktemp)" || exit 2
trap 'rm -f "$hits" "$errs"' EXIT

for rule in "${RULES[@]}"; do
	label="${rule%%|*}"
	pattern="${rule#*|}"

	# -I skips binary files, -n gives line numbers, -o prints just the match.
	#
	# The allowlist is excluded from the scan: by construction it quotes the
	# exact text it is waving through, so scanning it would make every entry
	# self-violating. It is the one file with that property — the checker script
	# itself is scanned (its patterns contain no literal ids).
	#
	# The exit code is checked BEFORE the output is read, and it is checked per
	# rule. git grep exits 0 on a match, 1 on no match, and >1 on an error — a
	# broken pattern exits 128 and prints nothing to stdout, which without this
	# check reads exactly like "no violations" and lets the gate report clean
	# while blind. Anything above 1 is fatal and names the rule.
	git grep -PIn -o -e "$pattern" -- . ":(exclude)$ALLOWLIST" >"$hits" 2>"$errs"
	grep_rc=$?
	if [[ $grep_rc -gt 1 ]]; then
		echo "check-public-hygiene: RULE FAILED TO RUN — '$label' (git grep exited $grep_rc)" >&2
		echo "    pattern: $pattern" >&2
		while IFS= read -r errline; do
			[[ -n "$errline" ]] && echo "    $errline" >&2
		done <"$errs"
		echo "    the gate cannot report clean while a rule is broken — fix the pattern" >&2
		exit 2
	fi

	while IFS= read -r hit; do
		[[ -z "$hit" ]] && continue
		file="${hit%%:*}"
		rest="${hit#*:}"
		line="${rest%%:*}"
		text="${rest#*:}"

		if is_allowed "$file" "$text"; then
			allowed=$((allowed + 1))
			continue
		fi

		key="$label:$file:$line"
		case $'\n'"$seen_keys" in
		*$'\n'"$key"$'\n'*) continue ;;
		esac
		seen_keys="$seen_keys$key"$'\n'

		violations=$((violations + 1))
		printf '%s:%s: %s: %s\n' "$file" "$line" "$label" "$text"
		# Show the offending source line so the fix is obvious without opening
		# the file. `sed -n Np` is cheap and the file is tracked, so it exists.
		src="$(sed -n "${line}p" -- "$file" 2>/dev/null)"
		[[ -n "$src" ]] && printf '    | %s\n' "$src"
	done <"$hits"
done

# A stale allowlist entry is a hole nobody is watching: it says "this exact text
# in this exact file is fine", and once the text has moved or gone it suppresses
# nothing while still looking like coverage. Hard error.
stale=0
for ((i = 0; i < ALLOW_COUNT; i++)); do
	if [[ ${ALLOW_HITS[$i]} -eq 0 ]]; then
		echo "check-public-hygiene: $ALLOWLIST:${ALLOW_LINENO[$i]}: stale entry — it suppresses nothing" >&2
		echo "    ${ALLOW_PATH[$i]} | ${ALLOW_TEXT[$i]}" >&2
		stale=1
	fi
done
if [[ $stale -ne 0 ]]; then
	echo "    the text or the file has changed. Correct the entry, or delete it." >&2
	exit 2
fi

if [[ $violations -gt 0 ]]; then
	echo
	echo "check-public-hygiene: FAILED — $violations private planning identifier(s) in tracked files."
	echo
	echo "These identifiers only resolve inside the private planning tracker and must not"
	echo "appear in a public repo. Delete the pointer and, if it carried meaning, replace it"
	echo "with the actual rationale in plain English."
	echo
	echo "If a match is genuinely legitimate, first try to make the pattern more precise in"
	echo "scripts/check-public-hygiene.sh, adding the innocent string to"
	echo "scripts/public-hygiene-innocent-strings.txt so it stays fixed. Only if that is"
	echo "impossible, add a line to $ALLOWLIST in the form:"
	echo
	echo "    path/to/file | <exact matched text> | why this is legitimate"
	exit 1
fi

if [[ $allowed -gt 0 ]]; then
	echo "check-public-hygiene: clean ($allowed allowlisted match(es))."
else
	echo "check-public-hygiene: clean."
fi
exit 0
