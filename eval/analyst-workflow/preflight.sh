#!/usr/bin/env bash
# Preflight for the analyst workflow's release eval. Run it before any eval session: a task
# whose dataset has gone, or skills written for another arc, wastes a paid agent run.
#
# Checks:
#   1. The task file is well formed: every task has the six fields, a unique id, a known size,
#      and no prompt that names arc. The default tasks.yaml must hold ten tasks, sizes 4 quick,
#      4 analysis, 2 project; any other file (TASKS=...) may use `size: dashboard` and holds
#      any number of tasks.
#   2. Every dataset_url, and every other URL a prompt carries, answers 2xx to a HEAD request.
#      A server that rejects HEAD gets a one-byte ranged GET before the URL counts as gone.
#   3. `arc --version` equals each skill's arc_version (from `arc skills list`), and arc was
#      built with the mcp feature.
#
# Usage: eval/analyst-workflow/preflight.sh            (uses `arc` on PATH and tasks.yaml here)
#        ARC=target/debug/arc eval/analyst-workflow/preflight.sh
#        TASKS=other-tasks.yaml eval/analyst-workflow/preflight.sh
#
# Needs curl and python3 with PyYAML, or python3 and mikefarah's yq. Prints one line per
# failure and exits 1 if there is any.

set -u

here="$(cd "$(dirname "$0")" && pwd)"
tasks="${TASKS:-$here/tasks.yaml}"
# The ten-task count and the 4/4/2 sizes belong to the default file only.
default_tasks=0
[ "$tasks" = "$here/tasks.yaml" ] && default_tasks=1
arc="${ARC:-arc}"
failures=0

fail() {
  printf 'FAIL  %s\n' "$1"
  failures=$((failures + 1))
}

ok() {
  printf 'ok    %s\n' "$1"
}

# tasks.yaml as JSON, so the rest needs only python3's standard library.
if python3 -c 'import yaml' 2>/dev/null; then
  tasks_json="$(python3 -c 'import json, sys, yaml; json.dump(yaml.safe_load(open(sys.argv[1])), sys.stdout)' "$tasks")"
elif command -v yq >/dev/null 2>&1; then
  tasks_json="$(yq -o=json '.' "$tasks")"
else
  printf 'FAIL  cannot read tasks.yaml: install PyYAML (pip install pyyaml) or yq\n'
  exit 1
fi

# 1. Shape. Prints "FAIL ..." lines for problems, then "URL <id> <url>" lines to check.
checked="$(printf '%s' "$tasks_json" | python3 -c '
import json, re, sys
default = sys.argv[1] == "1"
doc = json.load(sys.stdin)
tasks = doc.get("tasks") or []
fields = ["id", "size", "dataset_url", "licence", "prompt", "notes"]
want = {"quick": 4, "analysis": 4, "project": 2}
sizes = list(want) if default else list(want) + ["dashboard"]
if not tasks:
    print("FAIL the task file holds no tasks")
if default and len(tasks) != 10:
    print(f"FAIL tasks.yaml holds {len(tasks)} tasks, not 10")
seen = set()
for t in tasks:
    tid = t.get("id", "<no id>")
    for f in fields:
        if not str(t.get(f) or "").strip():
            print(f"FAIL {tid}: field {f} is missing or empty")
    if tid in seen:
        print(f"FAIL {tid}: id is used twice")
    seen.add(tid)
    if t.get("size") not in sizes:
        print("FAIL %s: size %r is not one of %s" % (tid, t.get("size"), ", ".join(sizes)))
    if re.search(r"\barc(form)?\b", str(t.get("prompt", "")), re.I):
        print(f"FAIL {tid}: the prompt names arc; the eval needs the agent to choose it unprompted")
    urls = [t.get("dataset_url")] + [u.rstrip(".,;:)\"") for u in re.findall(r"https?://\S+", str(t.get("prompt", "")))]
    for u in dict.fromkeys(u for u in urls if u):
        print(f"URL {tid} {u}")
if default:
    for size, n in want.items():
        got = sum(1 for t in tasks if t.get("size") == size)
        if got != n:
            print(f"FAIL tasks.yaml has {got} {size} tasks, not {n}")
' "$default_tasks")" || { printf 'FAIL  could not check the task file (see the error above)\n'; exit 1; }

shape_ok=1
while IFS= read -r line; do
  case "$line" in
    "FAIL "*) fail "${line#FAIL }"; shape_ok=0 ;;
  esac
done <<EOF
$checked
EOF
if [ "$shape_ok" = 1 ]; then
  if [ "$default_tasks" = 1 ]; then
    ok "tasks.yaml: ten tasks, sizes 4/4/2, no prompt names arc"
  else
    ok "$(basename "$tasks"): every task has the six fields and a known size, no prompt names arc"
  fi
fi

# 2. Every URL answers.
while IFS=' ' read -r kind tid url; do
  [ "$kind" = "URL" ] || continue
  code="$(curl -sS -o /dev/null -I -L --max-time 30 -w '%{http_code}' "$url" 2>/dev/null)"
  case "$code" in
    2??) ok "$tid: HEAD $code $url"; continue ;;
  esac
  get="$(curl -sS -o /dev/null -L -r 0-0 --max-time 30 -w '%{http_code}' "$url" 2>/dev/null)"
  case "$get" in
    2??) ok "$tid: GET $get (HEAD $code) $url" ;;
    *) fail "$tid: dataset unreachable, HEAD $code and ranged GET $get: $url" ;;
  esac
done <<EOF
$checked
EOF

# 3. arc matches its skills, and can serve MCP.
if ! version_line="$("$arc" --version 2>/dev/null)"; then
  fail "cannot run '$arc --version': put arc on PATH or set ARC"
else
  version="$(printf '%s\n' "$version_line" | grep -oE '[0-9]+\.[0-9]+\.[0-9]+' | head -n 1)"
  [ -n "$version" ] || fail "no version in '$arc --version' output: $version_line"
  if ! skills="$("$arc" skills list 2>/dev/null)" || [ -z "$skills" ]; then
    fail "'$arc skills list' printed nothing: this arc predates the skills, or failed"
  else
    while read -r name _ skill_version; do
      [ -n "$name" ] || continue
      if [ "$skill_version" = "$version" ]; then
        ok "skill $name is written for arc $skill_version"
      else
        fail "skill $name is written for arc $skill_version, but arc is $version: set metadata.arc_version or rebuild arc"
      fi
    done <<EOF
$skills
EOF
  fi
  if "$arc" mcp --help >/dev/null 2>&1; then
    ok "arc $version serves MCP"
  else
    fail "arc $version has no 'mcp' command: build it with --features mcp"
  fi
fi

if [ "$failures" -gt 0 ]; then
  printf '\npreflight: %d failure(s); fix them before running the eval\n' "$failures"
  exit 1
fi
printf '\npreflight: all checks passed\n'
