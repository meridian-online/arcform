#!/usr/bin/env bash
# Public-hygiene gate: stop private planning identifiers reaching this public repo.
#
# This repository is public. The planning that drives it is not. Identifiers that
# only resolve inside the private planning tracker — decision records, task ids,
# milestone ids, acceptance-criterion shorthand, document ids, card ids and
# titles, spec criterion ids, choice records, dated spec names, review findings,
# vault links and paths — are meaningless to anyone reading this repo and leak
# the shape of private work. This gate runs in CI and is meant to be the first
# job to go red.
#
# THE SHAPES ARE NOT IN THIS FILE. scripts/public-hygiene-rules.txt holds them,
# one "<label>|<PCRE>" per line, and scripts/check-history-hygiene.sh, where a
# repository carries it, reads the same file for commit messages and pull
# request text. One list, so a shape added for one surface reaches the other.
#
# This file, its rules, its innocent-strings fixture and its self-test are the
# same bytes in every public repository that carries them. Change them at the
# canonical copy, not here; the allowlist is the one per-repository file.
#
# Usage (no arguments, from anywhere inside the repo):
#
#   ./scripts/check-public-hygiene.sh
#
# Exit codes:
#   0  clean
#   1  one or more violations found
#   2  the gate could not run correctly — a broken rule, a missing or malformed
#      rules file, a malformed or stale allowlist entry, a git without PCRE, or
#      no perl. ALWAYS a hard failure: a gate that cannot run must never look
#      like a gate that passed.
#
# Its own regression test is scripts/check-public-hygiene-selftest.sh.
#
# What this covers, and what it does NOT
# --------------------------------------
# COVERED: the content of TRACKED files in the current checkout, via `git grep`,
# one line at a time and then each line joined to the next (see "Wrapped
# identifiers" below). Ignored and untracked files are invisible by
# construction, so a dirty working tree cannot make the gate cry wolf.
#
# NOT COVERED: commit messages and pull request text (the history gate's job
# where a repository carries one), review comments, branch names, issues,
# releases, and everything else that lives on the forge rather than in the
# repository.
#
# On false positives
# ------------------
# A gate that cries wolf gets disabled within a week, which is worse than no
# gate. scripts/public-hygiene-innocent-strings.txt is a tracked fixture of
# innocent strings that look like identifiers, and it is scanned like any other
# tracked file, so a pattern loosened until it bites honest prose turns the gate
# red on its own fixture. If a pattern flags something legitimate, fix the
# pattern and add the string to the fixture. The allowlist is for genuine
# content that must stay, not for papering over a loose pattern.
set -uo pipefail

REPO_ROOT="$(git rev-parse --show-toplevel)" || {
	echo "check-public-hygiene: not inside a git repository" >&2
	exit 2
}
cd "$REPO_ROOT" || exit 2

RULES_FILE="scripts/public-hygiene-rules.txt"
ALLOWLIST="scripts/public-hygiene-allowlist.txt"

# The rules use PCRE lookarounds, which git only offers when built with PCRE.
# git grep exits 0 on a match, 1 on none, and >1 on an error such as "cannot use
# Perl-compatible regexes when not compiled with USE_LIBPCRE".
git grep -qP -e 'zzzz(?<!qqqq)' -- . >/dev/null 2>&1
pcre_rc=$?
if [[ $pcre_rc -gt 1 ]]; then
	echo "check-public-hygiene: this git cannot run PCRE patterns (git grep -P exited $pcre_rc)" >&2
	exit 2
fi
# The wrapped-identifier pass runs the same patterns in perl, whose regex
# dialect is the one PCRE copies for every construct the rules use.
if ! command -v perl >/dev/null 2>&1; then
	echo "check-public-hygiene: perl is not on PATH, so wrapped identifiers cannot be checked" >&2
	exit 2
fi

# ---------------------------------------------------------------------------
# Rules.
# ---------------------------------------------------------------------------
if [[ ! -f "$RULES_FILE" ]]; then
	echo "check-public-hygiene: $RULES_FILE is missing — the gate has no rules to run" >&2
	exit 2
fi
declare -a RULES=()
rules_lineno=0
bad_rules=0
while IFS= read -r raw || [[ -n "$raw" ]]; do
	rules_lineno=$((rules_lineno + 1))
	line="${raw%$'\r'}"
	[[ -z "${line//[[:space:]]/}" ]] && continue
	[[ "${line#"${line%%[![:space:]]*}"}" == \#* ]] && continue
	if [[ "$line" != *"|"* ]]; then
		echo "check-public-hygiene: $RULES_FILE:$rules_lineno: expected '<label>|<pattern>'" >&2
		echo "    $line" >&2
		bad_rules=1
		continue
	fi
	r_label="${line%%|*}"
	r_pattern="${line#*|}"
	if [[ -z "$r_label" || -z "$r_pattern" ]]; then
		echo "check-public-hygiene: $RULES_FILE:$rules_lineno: label and pattern are both required" >&2
		bad_rules=1
		continue
	fi
	RULES+=("$r_label|$r_pattern")
done <"$RULES_FILE"
if [[ $bad_rules -ne 0 ]]; then
	exit 2
fi
if [[ ${#RULES[@]} -eq 0 ]]; then
	echo "check-public-hygiene: $RULES_FILE declares no rules — an empty gate reports clean" >&2
	exit 2
fi

# Paths a label does not scan, as "<label> <pathspec>" pairs. card-slug skips
# vendored source: upstream documentation URLs carry long kebab-case runs that
# nobody here wrote, and a leak needs someone here to have written it.
RULE_EXTRA_EXCLUDES=(
	'card-slug vendor/**'
)

# ---------------------------------------------------------------------------
# The path rule: a document path rooted in another checkout.
#
# Every rule in the rules file matches a planning identifier. A relative path to
# a `.md` document in a sibling checkout carries none, resolves on the author's
# disk and nowhere else, and when that checkout is private it discloses its
# layout. The pattern cannot decide this alone, so it lives here rather than in
# the rules file: a hit is cleared when its leading segment is a top-level entry
# of this repository, or when it resolves, from the directory of the file that
# wrote it, to a tracked file.
#
# Only `.md`: without the extension the pattern collides with MIME types and
# with module paths in prose. `$`, `{` and `}` are in the lookbehind because an
# interpolated path has a variable where its root segment should be.
# ---------------------------------------------------------------------------
PATH_LABEL='cross-repo-doc-path'
PATH_PATTERN='(?i)(?<![-_A-Za-z0-9/.:${}])(?:\.{1,2}/)*[A-Za-z0-9_.][A-Za-z0-9_.-]*(?:/[A-Za-z0-9_.-]+)+\.md(?![A-Za-z0-9])'

# ---------------------------------------------------------------------------
# Allowlist.
#
# One entry per line, THREE pipe-separated fields:
#
#     <tracked/file/path> | <exact offending text> | <why this is legitimate>
#
# All three are required. Anything that does not parse is exit 2, and so is an
# entry that suppresses nothing, so a stale allowlist cannot rot into coverage.
# Line numbers are not part of an entry; they drift on every edit. Blank lines
# and whole-line `#` comments are ignored.
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
		line="${raw%$'\r'}"
		trimmed="$(trim "$line")"
		[[ -z "$trimmed" ]] && continue
		[[ "$trimmed" == \#* ]] && continue
		# Count the separators rather than reading into an array: `read -r -a`
		# drops a trailing empty field, so an entry with no reason would arrive
		# as two fields and be reported as malformed instead of reasonless.
		seps="${line//[^|]/}"
		if [[ ${#seps} -ne 2 ]]; then
			echo "check-public-hygiene: $ALLOWLIST:$lineno: expected 3 '|'-separated fields, got $((${#seps} + 1))" >&2
			echo "    $line" >&2
			bad_allow=1
			continue
		fi
		rest="${line#*|}"
		a_path="$(trim "${line%%|*}")"
		a_text="$(trim "${rest%%|*}")"
		a_why="$(trim "${rest#*|}")"
		if [[ -z "$a_path" || -z "$a_text" || -z "$a_why" ]]; then
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

# Returns 0, and marks the entry used, when this file/text pair is allowlisted.
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
# Newline-delimited "<label>:<file>:<line>" keys already reported. A string, not
# an associative array: macOS ships bash 3.2, which has no `declare -A`.
seen_keys=""

work="$(mktemp -d)" || exit 2
trap 'rm -rf "$work"' EXIT
hits="$work/hits"
errs="$work/errs"
tracked="$work/tracked"
toplevel="$work/toplevel"
git ls-files >"$tracked" || exit 2
sed 's|/.*||' "$tracked" | sort -u >"$toplevel" || exit 2

# Runs one pattern over the tracked files into $hits. The exit code is checked
# before the output is read: a broken pattern makes git grep exit 128 and print
# nothing, which without this check reads exactly like "no violations".
scan_or_die() {
	local label="$1" pattern="$2" rc errline
	shift 2
	git grep -PIn -o -e "$pattern" -- . ":(exclude)$ALLOWLIST" "$@" >"$hits" 2>"$errs"
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

report() {
	local label="$1" file="$2" line="$3" text="$4" key src
	key="$label:$file:$line"
	case $'\n'"$seen_keys" in
	*$'\n'"$key"$'\n'*) return 0 ;;
	esac
	seen_keys="$seen_keys$key"$'\n'
	violations=$((violations + 1))
	printf '%s:%s: %s: %s\n' "$file" "$line" "$label" "$text"
	src="$(sed -n "${line}p" -- "$file" 2>/dev/null)"
	[[ -n "$src" ]] && printf '    | %s\n' "$src"
	return 0
}

# The pathspecs a label skips, one per line.
excludes_for() {
	local entry
	for entry in "${RULE_EXTRA_EXCLUDES[@]}"; do
		[[ "${entry%% *}" == "$1" ]] && printf '%s\n' "${entry#* }"
	done
}

for rule in "${RULES[@]}"; do
	label="${rule%%|*}"
	pattern="${rule#*|}"
	extra=()
	while IFS= read -r spec; do
		[[ -n "$spec" ]] && extra+=(":(exclude)$spec")
	done < <(excludes_for "$label")
	scan_or_die "$label" "$pattern" ${extra[@]+"${extra[@]}"}
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

# ---------------------------------------------------------------------------
# Wrapped identifiers.
#
# git grep matches one line at a time, so an identifier whose word and number
# sit either side of a line break — prose reflowed to a column, a doc comment
# continued on the next `///` line — is invisible to every rule above. This pass
# joins each line to the next with one space, after stripping the next line's
# indentation and one leading comment marker (`//`, `///`, `//!`, `#`, `*`,
# `--`, `;`, `>`, `<!--`), and runs every rule over the pair. A match is reported
# only when it starts on the first line and ends on the second: one that fits on
# a single line belongs to the scan above. The report names the first line.
# ---------------------------------------------------------------------------
: >"$work/rules.tsv"
: >"$work/skip"
for rule in "${RULES[@]}"; do
	label="${rule%%|*}"
	printf '%s\t%s\n' "$label" "${rule#*|}" >>"$work/rules.tsv"
	while IFS= read -r spec; do
		[[ -z "$spec" ]] && continue
		git ls-files -- "$spec" | sed "s|^|$label	|" >>"$work/skip" || exit 2
	done < <(excludes_for "$label")
done

git grep -z -I -l -e '' -- . ":(exclude)$ALLOWLIST" >"$work/textfiles" 2>"$errs"
rc=$?
if [[ $rc -gt 1 ]]; then
	echo "check-public-hygiene: could not list the tracked text files (git grep exited $rc)" >&2
	exit 2
fi

perl -e '
	use strict;
	use warnings;
	my ($rules_file, $skip_file) = @ARGV;
	my (@rules, %skip);
	open my $rf, "<", $rules_file or die "cannot read $rules_file\n";
	while (<$rf>) {
		chomp;
		my ($label, $pattern) = split /\t/, $_, 2;
		my $re = eval { qr/$pattern/ } or die "rule $label does not compile in perl: $@";
		push @rules, [$label, $re];
	}
	open my $sf, "<", $skip_file or die "cannot read $skip_file\n";
	while (<$sf>) { chomp; $skip{$_} = 1; }
	my @files = do { local $/ = "\0"; map { s/\0\z//r } <STDIN> };
	my $marker = qr{(?://[/!]?|#+|\*|--|;+|>|<!--)};
	for my $file (@files) {
		open my $fh, "<", $file or next;
		my @lines = <$fh>;
		close $fh;
		for my $i (0 .. $#lines - 1) {
			(my $first = $lines[$i]) =~ s/\s+\z//;
			(my $second = $lines[$i + 1]) =~ s/\s+\z//;
			$second =~ s/\A\s*(?:$marker[ \t]*)?//;
			next if $first eq "" || $second eq "";
			my $joined = "$first $second";
			my $cut = length $first;
			for my $rule (@rules) {
				my ($label, $re) = @$rule;
				next if $skip{"$label\t$file"};
				while ($joined =~ /$re/g) {
					my ($start, $end) = ($-[0], $+[0]);
					if ($start < $cut && $end > $cut + 1) {
						printf "%s:%d:%s|%s\n", $file, $i + 1, $label,
							substr($joined, $start, $end - $start);
					}
					pos($joined) = $start + 1 if $end == $start;
				}
			}
		}
	}
' "$work/rules.tsv" "$work/skip" <"$work/textfiles" >"$hits" 2>"$errs"
rc=$?
if [[ $rc -ne 0 ]]; then
	echo "check-public-hygiene: the wrapped-identifier pass failed (perl exited $rc)" >&2
	while IFS= read -r errline; do
		[[ -n "$errline" ]] && echo "    $errline" >&2
	done <"$errs"
	exit 2
fi
while IFS= read -r hit; do
	[[ -z "$hit" ]] && continue
	file="${hit%%:*}"
	rest="${hit#*:}"
	line="${rest%%:*}"
	rest="${rest#*:}"
	label="${rest%%|*}"
	text="${rest#*|}"
	if is_allowed "$file" "$text"; then
		allowed=$((allowed + 1))
		continue
	fi
	report "$label" "$file" "$line" "$text"
done <"$hits"

# ---------------------------------------------------------------------------
# The path rule's scan.
# ---------------------------------------------------------------------------

# Normalises a relative path against a base directory, printing the result or
# failing when `..` climbs above the repository root.
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

scan_or_die "$PATH_LABEL" "$PATH_PATTERN"
while IFS= read -r hit; do
	[[ -z "$hit" ]] && continue
	file="${hit%%:*}"
	rest="${hit#*:}"
	line="${rest%%:*}"
	text="${rest#*:}"
	root_rel="$(resolve_rel "" "$text")" || root_rel=""
	lead="${root_rel%%/*}"
	if [[ -n "$lead" ]] && grep -Fxq -- "$lead" "$toplevel"; then
		continue
	fi
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

# A stale allowlist entry says "this exact text in this exact file is fine" and,
# once the text has gone, suppresses nothing while still looking like coverage.
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
	echo "If a match is genuinely legitimate, first make the pattern more precise in"
	echo "$RULES_FILE and add the innocent string to"
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
