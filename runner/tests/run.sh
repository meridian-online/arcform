#!/usr/bin/env bash
# The runner image's acceptance tests. Each test starts a fresh stub (stub/stub.py:
# a credential exchange, an S3-compatible store and a git forge) on a private
# Docker network, runs the image once against it with `docker run --platform
# linux/arm64`, and checks the exit code, the stub's record and the run log the
# image uploaded. Nothing leaves the machine.
#
#   runner/tests/run.sh [IMAGE]     IMAGE defaults to arc-runner
#
# Exits 0 when every test passes.
set -euo pipefail

image=${1:-arc-runner}
here=$(cd "$(dirname "$0")" && pwd)
stub_image=arc-runner-stub
id=$$
net="arc-runner-test-$id"
stub="arc-runner-stub-$id"
out=$(mktemp -d)

cleanup() {
  docker rm -f "$stub" >/dev/null 2>&1 || true
  docker network rm "$net" >/dev/null 2>&1 || true
  rm -rf "$out"
}
trap cleanup EXIT

docker build --quiet -f "$here/stub/Dockerfile" -t "$stub_image" "$here" >/dev/null
docker network create "$net" >/dev/null

passed=0
failed=0
current=""
problems=()

expect() { # expect DESCRIPTION COMMAND...: record a failed expectation
  local what=$1
  shift
  if ! "$@"; then
    problems+=("$current: $what")
  fi
}

start_stub() { # start_stub RUN_ID TOKEN LIFETIME [EXCHANGE_FAILS]
  docker rm -f "$stub" >/dev/null 2>&1 || true
  docker run --detach --name "$stub" --network "$net" --network-alias stub \
    -e STUB_RUN_ID="$1" -e STUB_RUN_TOKEN="$2" -e STUB_LIFETIME="$3" \
    -e STUB_EXCHANGE_FAILS="${4:-0}" \
    "$stub_image" >/dev/null
  for _ in $(seq 1 50); do
    if docker exec "$stub" python3 -I /stub/stub.py --state >/dev/null 2>&1; then return 0; fi
    sleep 0.2
  done
  echo "the stub did not start" >&2
  docker logs "$stub" >&2
  exit 1
}

state() { docker exec "$stub" python3 -I /stub/stub.py --state; }

run_image() { # run_image RUN_ID TOKEN PROTOCOL_REF: the exit code; output in $out/<test>.out
  local code=0
  docker run --rm --platform linux/arm64 --network "$net" \
    -e RUN_ID="$1" -e RUN_TOKEN="$2" -e PROTOCOL_REF="$3" \
    -e CREDENTIAL_EXCHANGE_URL=http://stub:8080 \
    -e PROTOCOL_FORGE_URL=http://stub:8080/git \
    -e ARC_RUNNER_INSECURE_HTTP=1 \
    "$image" >"$out/$current.out" 2>&1 || code=$?
  echo "$code"
}

start_detached() { # start_detached RUN_ID TOKEN PROTOCOL_REF [FORGE_URL]: the container's id
  docker run --detach --platform linux/arm64 --network "$net" \
    -e RUN_ID="$1" -e RUN_TOKEN="$2" -e PROTOCOL_REF="$3" \
    -e CREDENTIAL_EXCHANGE_URL=http://stub:8080 \
    -e PROTOCOL_FORGE_URL="${4:-http://stub:8080/git}" \
    -e ARC_RUNNER_INSECURE_HTTP=1 \
    "$image"
}

stop_when() { # stop_when CID TEXT: once the container's output shows TEXT, stop it as ECS would; sets code and stopped_in
  local cid=$1 text=$2 started
  for _ in $(seq 1 100); do
    docker logs "$cid" 2>&1 | grep -qF -- "$text" && break
    sleep 0.1
  done
  started=$(date +%s)
  docker stop --time 30 "$cid" >/dev/null
  stopped_in=$(($(date +%s) - started))
  code=$(docker wait "$cid")
  docker logs "$cid" >"$out/$current.out" 2>&1
  docker rm "$cid" >/dev/null
}

uploaded_log() { # uploaded_log RUN_ID: the run log the image wrote, or nothing
  state | jq -r --arg k "sub_test/runs/$1/run.log" '.objects[$k] // empty'
}

begin() {
  current=$1
  problems=()
}

end() {
  if ((${#problems[@]} == 0)); then
    passed=$((passed + 1))
    echo "ok      $current"
  else
    failed=$((failed + 1))
    echo "FAILED  $current"
    printf '          %s\n' "${problems[@]}"
    echo "        --- the image's output:"
    sed 's/^/        | /' "$out/$current.out"
  fi
}

contains() { grep -qF -- "$2" <<<"$1"; }
lacks() { ! grep -qF -- "$2" <<<"$1"; }
equals() { [[ $1 == "$2" ]]; }
at_least() { (($1 >= $2)); }

# Commits are made when the stub image is built; read them once.
start_stub probe probe-token 900
pinned=$(state | jq -r '.commits.sample[0]')
head_commit=$(state | jq -r '.commits.sample[1]')
fails_second=$(state | jq -r '.commits["fails-second"][0]')
prints_env=$(state | jq -r '.commits["prints-env"][0]')
slow=$(state | jq -r '.commits.slow[0]')

# ---------------------------------------------------------------------------
begin runs_pinned_protocol
run="run-pinned-1"
token=$(openssl rand -hex 24)
start_stub "$run" "$token" 900
code=$(run_image "$run" "$token" "test/sample@$pinned")
log=$(uploaded_log "$run")
expect "exit code 0, not $code" equals "$code" 0
expect "one exchange" equals "$(state | jq '.exchanges')" 1
expect "the run log is written under sub_test/runs/$run/" contains "$(state | jq -r '.puts[].key')" "sub_test/runs/$run/run.log"
expect "no upload refused" equals "$(state | jq '.rejected_puts | length')" 0
expect "the pinned commit ran" contains "$log" "sample - the pinned commit ran"
expect "the branch head did not run" lacks "$log" "the branch head ran"
expect "the head commit differs from the pinned one" lacks "$pinned" "$head_commit"
expect "the log names the run and the reference" contains "$log" "run $run: test/sample@$pinned"
expect "the log holds no terminal colour codes" lacks "$log" $'\e['
end

# ---------------------------------------------------------------------------
begin fails_named_step
run="run-fails-1"
token=$(openssl rand -hex 24)
start_stub "$run" "$token" 900
code=$(run_image "$run" "$token" "test/fails-second@$fails_second")
log=$(uploaded_log "$run")
expect "exit code 13 (the run step), not $code" equals "$code" 13
expect "the run log is written" contains "$log" "arc-runner:"
expect "the first step ran" contains "$log" "first - ok"
expect "the log names the failing step" contains "$log" "failed at run: step 'second' failed"
expect "the third step did not run" lacks "$log" "third - should never run"
expect "the image's last line names the failing step" contains "$(grep 'failed at' "$out/$current.out" | tail -n 1)" "step 'second' failed"
end

# ---------------------------------------------------------------------------
begin token_and_credential_kept_out_of_step_env
run="run-env-1"
token=$(openssl rand -hex 24)
start_stub "$run" "$token" 900
code=$(run_image "$run" "$token" "test/prints-env@$prints_env")
log=$(uploaded_log "$run")
expect "exit code 0, not $code" equals "$code" 0
expect "the step printed its environment" contains "$log" "RUN_ID=$run"
expect "the step ran as protocol" contains "$log" "HOME=/home/protocol"
expect "the one-time token is not in the log" lacks "$log" "$token"
expect "RUN_TOKEN is not in the step's environment" lacks "$log" "RUN_TOKEN"
while read -r secret; do
  expect "an issued credential part or renewal token is not in the log" lacks "$log" "$secret"
done < <(state | jq -r '.issued[] | .access_key_id, .secret_access_key, .session_token' && state | jq -r '.renewal_tokens[]')
end

# ---------------------------------------------------------------------------
begin renews_credential_while_running
run="run-slow-1"
token=$(openssl rand -hex 24)
# Credentials last 4 seconds; the Protocol runs for 7.
start_stub "$run" "$token" 4
code=$(run_image "$run" "$token" "test/slow@$slow")
log=$(uploaded_log "$run")
expect "exit code 0, not $code" equals "$code" 0
expect "at least one renewal, not $(state | jq '.renewals')" at_least "$(state | jq '.renewals')" 1
expect "the log was signed with a renewed credential" equals \
  "$(state | jq -r '[.puts[].access_key_id] | map(. != $first) | all' --arg first "$(state | jq -r '.issued[0].access_key_id')")" true
expect "the step finished" contains "$log" "slow - done"
expect "no renewal refused" equals "$(state | jq '.renewal_refusals')" 0
end

# ---------------------------------------------------------------------------
begin uploads_the_log_when_stopped
run="run-stop-1"
token=$(openssl rand -hex 24)
start_stub "$run" "$token" 900
# Stop it once its seven-second step has started, as a Spot interruption would.
stop_when "$(start_detached "$run" "$token" "test/slow@$slow")" "[1/1]"
log=$(uploaded_log "$run")
expect "exit code 15 (interrupted), not $code" equals "$code" 15
expect "the log is written and names the interruption" contains "$log" "failed at interrupted: stopped during run"
expect "the step did not finish" lacks "$log" "slow - done"
expect "it stopped within ten seconds, not $stopped_in" at_least 10 "$stopped_in"
end

# ---------------------------------------------------------------------------
begin uploads_the_log_when_stopped_during_fetch
run="run-stall-1"
token=$(openssl rand -hex 24)
start_stub "$run" "$token" 900
# This forge never answers, so the fetch is under way when the stop comes.
stop_when "$(start_detached "$run" "$token" "test/sample@$pinned" http://stub:8080/stall)" "run $run:"
log=$(uploaded_log "$run")
expect "exit code 15 (interrupted), not $code" equals "$code" 15
expect "the log is written and names the fetch" contains "$log" "failed at interrupted: stopped during fetch"
expect "it stopped within ten seconds, not $stopped_in" at_least 10 "$stopped_in"
end

# ---------------------------------------------------------------------------
begin refuses_a_reference_without_a_full_commit
run="run-ref-1"
token=$(openssl rand -hex 24)
start_stub "$run" "$token" 900
code=$(run_image "$run" "$token" "test/sample@main")
expect "exit code 10 (inputs), not $code" equals "$code" 10
expect "the token is not spent" equals "$(state | jq '.exchanges')" 0
expect "the output names the reference" contains "$(cat "$out/$current.out")" "PROTOCOL_REF must be"
code=$(run_image "$run" "$token" "test/sample@$(openssl rand -hex 32)")
expect "a 64-digit commit exits 10 (inputs), not $code" equals "$code" 10
expect "the token is still not spent" equals "$(state | jq '.exchange_posts')" 0
end

# ---------------------------------------------------------------------------
begin does_not_repeat_an_exchange_that_was_answered
run="run-once-1"
token=$(openssl rand -hex 24)
# The exchange spends the token, then answers 503.
start_stub "$run" "$token" 900 503
code=$(run_image "$run" "$token" "test/sample@$pinned")
expect "exit code 11 (exchange), not $code" equals "$code" 11
expect "one exchange request, not $(state | jq '.exchange_posts')" equals "$(state | jq '.exchange_posts')" 1
end

# ---------------------------------------------------------------------------
begin fails_fetch_of_an_unknown_commit
run="run-fetch-1"
token=$(openssl rand -hex 24)
start_stub "$run" "$token" 900
missing=$(openssl rand -hex 20)
code=$(run_image "$run" "$token" "test/sample@$missing")
log=$(uploaded_log "$run")
expect "exit code 12 (fetch), not $code" equals "$code" 12
expect "the log names the fetch" contains "$log" "failed at fetch: could not fetch test/sample at $missing"
end

# ---------------------------------------------------------------------------
begin refuses_a_spent_token
run="run-spent-1"
token=$(openssl rand -hex 24)
start_stub "$run" "$token" 900
first=$(run_image "$run" "$token" "test/sample@$pinned")
code=$(run_image "$run" "$token" "test/sample@$pinned")
expect "the first run exits 0, not $first" equals "$first" 0
expect "the second exits 11 (exchange), not $code" equals "$code" 11
expect "the stub refused one exchange" equals "$(state | jq '.exchange_refusals')" 1
end

echo
echo "$passed passed, $failed failed"
((failed == 0))
