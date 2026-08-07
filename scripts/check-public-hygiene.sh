#!/usr/bin/env bash
# Public-hygiene gate: stop private planning references reaching this public repo.
#
# This repository is public. The planning that drives it is not. Two shapes leak.
#
# Identifiers that only resolve inside the private planning tracker — decision
# records, task ids, milestone ids, acceptance-criterion shorthand, document ids,
# card ids, spec AC ids — are meaningless to anyone reading this repo and leak the
# shape of private work. They have reached main repeatedly under a convention-only
# rule.
#
# Document paths rooted in another checkout are the second shape, and every rule
# for the first shape misses them because they carry no number. See the
# cross-repo-doc-path rule below for what it matches and what it cost.
#
# This is the enforcement: it runs in CI, and it is meant to be the first job to
# go red.
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
# The path rule: a document path rooted in another checkout.
#
# Every rule above matches a NUMBERED planning identifier. A path into a sibling
# repository carries no number, so all eleven of them exit 0 on it — which is how
# a module doc-comment on main came to point at a markdown file inside the
# project's private planning repo, and stayed there. The deletion was one line;
# without this rule the next doc-comment reopens the hole.
#
# WHAT IT MATCHES, stated without naming anything: a relative path to a `.md`
# document that does not belong to this repository. Such a path resolves on the
# author's disk and nowhere else. It is a dead pointer for every reader of a
# public crate, and when the checkout it names is private it discloses that
# repository's internal layout on top of being useless.
#
# "Does not belong to this repository" is decided by two readings, because prose
# uses both and either one landing inside the repo means the pointer is about
# this repo:
#
#   * ROOT-RELATIVE — read from the repository root, is the leading segment a
#     top-level entry of this repository? The whole path is deliberately NOT
#     required to resolve. Requiring that would redden on `vendor/`, where a
#     partial upstream copy keeps a README linking to documentation that was
#     never vendored (measured 2026-08-07: one such link), and on any doc that
#     has been renamed since it was cited. Neither is a leak, and a gate that
#     cries wolf is off within the week. The root segment is the part that
#     distinguishes "another checkout" from "a file that moved".
#
#   * FILE-RELATIVE — read from the directory of the file that wrote it, does it
#     resolve to a TRACKED file? This is the reading a genuine relative markdown
#     link needs, and it must be exact: without the tracked-file test, any path
#     written from inside `src/` would appear to live under `src/` and pass.
#     That hole is the leak this rule exists for.
#
# WHY ONLY `.md`. Dropping the extension makes the pattern collide with MIME
# types — `application/vnd.apache.parquet` and its kin, 5 of them in this tree —
# and with every `crate/module` path written in prose. A document pointer is the
# shape that leaked and the shape that has no business being cross-repo.
#
# WHAT THE LOOKBEHIND IS FOR. `$`, `{` and `}` join the usual path characters
# there because an INTERPOLATED path has a variable name where its root segment
# should be, and no rule can say whether `$d/scripts/guide.md` or
# `format!("{dir}/notes.md")` points inside this repo. The first draft of this
# rule went red on this repository's own self-test for exactly that reason.
#
# Measured on the tree of 2026-08-07: 4 matches, 3 of them resolving in-repo.
# ---------------------------------------------------------------------------
PATH_LABEL='cross-repo-doc-path'
PATH_PATTERN='(?<![-_A-Za-z0-9/.:${}])(?:\.{1,2}/)*[A-Za-z0-9_.][A-Za-z0-9_.-]*(?:/[A-Za-z0-9_.-]+)+\.md(?![A-Za-z0-9])'

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

		# Split on `|` into exactly three fields, counting the separators
		# rather than reading into an array: `read -r -a` DISCARDS a trailing
		# empty field, so `path | text |` — an entry whose author could not
		# think of a reason, which is precisely the shape the next check
		# exists to refuse — arrived as two fields and was reported as a
		# malformed line instead of a reasonless one.
		seps="${line//[^|]/}"
		if [[ ${#seps} -ne 2 ]]; then
			echo "check-public-hygiene: $ALLOWLIST:$lineno: expected 3 '|'-separated fields, got $((${#seps} + 1))" >&2
			echo "    $line" >&2
			echo "    format: <tracked/file/path> | <exact offending text> | <why this is legitimate>" >&2
			bad_allow=1
			continue
		fi
		rest="${line#*|}"
		a_path="$(trim "${line%%|*}")"
		a_text="$(trim "${rest%%|*}")"
		a_reason="$(trim "${rest#*|}")"
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

# Normalise "<base>/<ref>" into a repository-relative path, collapsing `.` and
# `..` textually — no filesystem access, so it is identical on CI and on a
# developer's machine, and a path that does not exist still normalises.
#
# Returns 1, printing nothing, when the reference climbs above the repository
# root: that is outside this repository by definition and there is nothing left
# to compare.
resolve_rel() {
	local rest="${1:+$1/}$2" seg stack=""
	while [[ -n "$rest" ]]; do
		seg="${rest%%/*}"
		if [[ "$seg" == "$rest" ]]; then
			rest=""
		else
			rest="${rest#*/}"
		fi
		case "$seg" in
		'' | '.') continue ;;
		'..')
			[[ -z "$stack" ]] && return 1
			if [[ "$stack" == */* ]]; then
				stack="${stack%/*}"
			else
				stack=""
			fi
			;;
		*) stack="${stack:+$stack/}$seg" ;;
		esac
	done
	printf '%s' "$stack"
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
tracked="$(mktemp)" || exit 2
toplevel="$(mktemp)" || exit 2
trap 'rm -f "$hits" "$errs" "$tracked" "$toplevel"' EXIT

# The two membership tests the path rule needs, taken once from the index so the
# rule sees exactly the files `git grep` scans.
git ls-files >"$tracked" || exit 2
sed 's|/.*||' "$tracked" | sort -u >"$toplevel" || exit 2

# Run one pattern over the tree into "$hits", or die naming the rule.
#
# -I skips binary files, -n gives line numbers, -o prints just the match.
#
# The allowlist is excluded from the scan: by construction it quotes the exact
# text it is waving through, so scanning it would make every entry
# self-violating. It is the one file with that property — the checker script
# itself is scanned (its patterns contain no literal ids).
#
# The exit code is checked BEFORE the output is read, and it is checked per rule.
# git grep exits 0 on a match, 1 on no match, and >1 on an error — a broken
# pattern exits 128 and prints nothing to stdout, which without this check reads
# exactly like "no violations" and lets the gate report clean while blind.
# Anything above 1 is fatal and names the rule.
scan_or_die() {
	local label="$1" pattern="$2" rc errline
	git grep -PIn -o -e "$pattern" -- . ":(exclude)$ALLOWLIST" >"$hits" 2>"$errs"
	rc=$?
	[[ $rc -le 1 ]] && return 0
	echo "check-public-hygiene: RULE FAILED TO RUN — '$label' (git grep exited $rc)" >&2
	echo "    pattern: $pattern" >&2
	while IFS= read -r errline; do
		[[ -n "$errline" ]] && echo "    $errline" >&2
	done <"$errs"
	echo "    the gate cannot report clean while a rule is broken — fix the pattern" >&2
	exit 2
}

# Report one violation, at most once per (rule, file, line).
report() {
	local label="$1" file="$2" line="$3" text="$4" key src
	key="$label:$file:$line"
	case $'\n'"$seen_keys" in
	*$'\n'"$key"$'\n'*) return 0 ;;
	esac
	seen_keys="$seen_keys$key"$'\n'

	violations=$((violations + 1))
	printf '%s:%s: %s: %s\n' "$file" "$line" "$label" "$text"
	# Show the offending source line so the fix is obvious without opening the
	# file. `sed -n Np` is cheap and the file is tracked, so it exists.
	src="$(sed -n "${line}p" -- "$file" 2>/dev/null)"
	[[ -n "$src" ]] && printf '    | %s\n' "$src"
	return 0
}

for rule in "${RULES[@]}"; do
	label="${rule%%|*}"
	pattern="${rule#*|}"
	scan_or_die "$label" "$pattern"

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

		report "$label" "$file" "$line" "$text"
	done <"$hits"
done

# The path rule. Same reporting and the same allowlist as the patterns above; it
# needs its own loop because whether a match is a violation depends on the tree,
# not on the text, and no PCRE can ask that question.
scan_or_die "$PATH_LABEL" "$PATH_PATTERN"

while IFS= read -r hit; do
	[[ -z "$hit" ]] && continue
	file="${hit%%:*}"
	rest="${hit#*:}"
	line="${rest%%:*}"
	text="${rest#*:}"

	# Read from the repository root: is the leading segment one of ours?
	root_rel="$(resolve_rel "" "$text")" || root_rel=""
	lead="${root_rel%%/*}"
	if [[ -n "$lead" ]] && grep -Fxq -- "$lead" "$toplevel"; then
		continue
	fi

	# Read from the directory of the citing file: does it hit a tracked file?
	dir="${file%/*}"
	[[ "$dir" == "$file" ]] && dir=""
	here_rel="$(resolve_rel "$dir" "$text")" || here_rel=""
	if [[ -n "$here_rel" ]] && grep -Fxq -- "$here_rel" "$tracked"; then
		continue
	fi

	if is_allowed "$file" "$text"; then
		allowed=$((allowed + 1))
		continue
	fi

	report "$PATH_LABEL" "$file" "$line" "$text"
done <"$hits"

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
	echo "check-public-hygiene: FAILED — $violations private planning reference(s) in tracked files."
	echo
	echo "A planning identifier resolves only inside the private tracker; a document path"
	echo "rooted in another checkout resolves only on the author's disk. Neither means"
	echo "anything to a reader of this repo, and both must not appear in a public one."
	echo "Delete the pointer and, if it carried meaning, replace it with the actual"
	echo "rationale in plain English."
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
