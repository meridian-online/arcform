#!/usr/bin/env bash
# arc-runner: the hosted runner's entrypoint. One run of one pinned Protocol:
#
#   inputs    read RUN_ID, RUN_TOKEN, PROTOCOL_REF and the two service URLs
#   exchange  trade the one-time token for a short-lived storage credential
#   fetch     fetch the Protocol's repository at its pinned commit
#   run       `arc run` as the unprivileged user `protocol`, renewing the
#             credential in the background while it runs
#   upload    write the run log where the exchange's reply said
#
# It exits 0 when the run and the upload succeed. Otherwise it exits with the
# code of the first step that failed, and its last line names that step:
#
#   10 inputs   11 exchange   12 fetch   13 run   14 upload   15 interrupted
#
# The credential, the renewal token and the run log stay in a directory only
# root can read. No Protocol step sees them: steps run as `protocol`, with an
# environment built from nothing (see as_protocol).
set -euo pipefail
umask 077

readonly STATE=/run/arc-runner
readonly LOG="$STATE/run.log"
readonly PROTOCOL_USER=protocol
readonly PROTOCOL_HOME=/home/protocol
readonly WORK="$PROTOCOL_HOME/protocol"
readonly SAFE_PATH=/usr/local/bin:/usr/bin:/bin

# Characters each value may hold. Tokens and credential parts go into curl's
# configuration on stdin, so a value that could hold a quote, a backslash or a
# newline is refused before it gets there.
readonly TOKEN_RE='^[A-Za-z0-9._~+/=-]{1,4096}$'
readonly RUN_ID_RE='^[A-Za-z0-9][A-Za-z0-9_-]{0,127}$'
readonly NAME_RE='[A-Za-z0-9][A-Za-z0-9._-]{0,99}'
readonly REF_RE="^($NAME_RE)/($NAME_RE)@([0-9a-f]{40})\$"
readonly BUCKET_RE='^[a-z0-9][a-z0-9.-]{1,62}$'
readonly REGION_RE='^[a-z0-9-]{1,32}$'
readonly PREFIX_RE='^([A-Za-z0-9_.-]+/)+$'

log_open=0      # 1 once run.log exists; 0 again once its upload has begun
finishing=0     # 1 once finish has begun
step=inputs     # the step under way, and its exit code
step_code=10
renew_pid=""
arc_pid=""
tail_pid=""
interrupted=0

say() {
  if [[ $log_open == 1 ]]; then
    printf 'arc-runner: %s\n' "$*" | tee -a "$LOG" >&2
  else
    printf 'arc-runner: %s\n' "$*" >&2
  fi
}

# as_protocol CMD...: run CMD as `protocol`, with no capabilities, no way to
# gain privileges, and an environment holding only what protocol_cmd lists.
# protocol_cmd is set once the inputs are read.
protocol_cmd=()
as_protocol() {
  "${protocol_cmd[@]}" "$@"
}

# require_url NAME VALUE: the value is an https URL, or http when the test-only
# ARC_RUNNER_INSECURE_HTTP=1 is set.
require_url() {
  local name=$1 value=$2
  if [[ $value =~ ^https://[^[:space:]\"\\]+$ ]]; then return 0; fi
  if [[ ${ARC_RUNNER_INSECURE_HTTP:-} == 1 && $value =~ ^http://[^[:space:]\"\\]+$ ]]; then
    return 0
  fi
  return 1
}

stop_renewal() {
  if [[ -n $renew_pid ]]; then
    kill "$renew_pid" 2>/dev/null || true
    wait "$renew_pid" 2>/dev/null || true
    renew_pid=""
  fi
}

# finish CODE STEP [DETAIL]: upload the log if there is one, then exit. CODE 0
# is success; otherwise STEP is named on the last line.
finish() {
  local code=$1 step=$2 detail=${3:-}
  local upload_ok=1
  finishing=1
  if [[ $code != 0 ]]; then
    say "failed at $step${detail:+: $detail}"
  else
    say "run succeeded"
  fi
  if [[ $log_open == 1 ]]; then
    log_open=0
    if ! upload_log; then
      upload_ok=0
      printf 'arc-runner: failed at upload: the run log was not written\n' >&2
    fi
  fi
  stop_renewal
  if [[ $code == 0 && $upload_ok == 0 ]]; then
    exit 14
  fi
  exit "$code"
}

# shellcheck disable=SC2317,SC2329 # invoked by the trap below
on_signal() {
  interrupted=1
  if [[ -n $arc_pid ]]; then
    kill -TERM "$arc_pid" 2>/dev/null || true
    # arc has ten seconds to stop before it is killed.
    (
      sleep 10
      kill -KILL "$arc_pid" 2>/dev/null || true
    ) &
  else
    finish 15 interrupted "stopped before the Protocol ran"
  fi
}
trap on_signal TERM INT

# A command that fails under `set -e` outside the checks above still ends in
# finish, so the log is uploaded and the exit code names the step.
# shellcheck disable=SC2317,SC2329 # invoked by the trap below
on_exit() {
  local rc=$?
  if [[ $finishing == 0 && $log_open == 1 ]]; then
    finish "$step_code" "$step" "an unexpected error (status $rc)"
  fi
}
trap on_exit EXIT

# post_json URL TOKEN OUT: POST with a bearer token, the reply body to OUT.
# Prints the HTTP status; fails when no reply came. The token may be good for
# one use only, so the POST is repeated only when curl could not resolve or
# connect to the host, and so sent nothing; never after a timeout or an error
# answer, when the server may already have spent the token.
post_json() {
  local url=$1 token=$2 out=$3 attempt rc status
  for attempt in 1 2 3 4; do
    rc=0
    status=$(printf 'header = "Authorization: Bearer %s"\n' "$token" |
      curl --config - --silent --show-error --output "$out" --write-out '%{http_code}' \
        --connect-timeout 10 --max-time 30 \
        -X POST -H 'Accept: application/json' "$url") || rc=$?
    case $rc in
      0) printf '%s' "$status"; return 0 ;;
      6 | 7) ((attempt < 4)) && sleep $((attempt * 2)) ;;
      *) return 1 ;;
    esac
  done
  return 1
}

# store_credential REPLY: check the credential in an exchange or renewal
# reply and make it the current one. The renewal token is the exchange's and
# lasts the run: a renewal reply carries a credential and nothing else.
store_credential() {
  local reply=$1 field value expires
  for field in access_key_id secret_access_key session_token; do
    value=$(jq -r --arg f "$field" '.credential[$f] // empty' "$reply")
    [[ $value =~ $TOKEN_RE ]] || { say "the reply's credential.$field is missing or malformed"; return 1; }
  done
  expires=$(jq -r '.credential.expires_at // empty' "$reply")
  if [[ -z $expires ]] || ! date -u -d "$expires" +%s >/dev/null 2>&1; then
    say "the reply's credential.expires_at is not a time"
    return 1
  fi
  jq '.credential' "$reply" >"$STATE/credential.json.new"
  mv -f "$STATE/credential.json.new" "$STATE/credential.json"
}

# renew_once: 0 renewed, 2 try again later, 3 refused (the run is over as far
# as the control plane is concerned). 408 and 429 are worth trying again.
renew_once() {
  local status
  status=$(post_json "$EXCHANGE/v1/runs/$RUN_ID/credentials/renew" "$(<"$STATE/renewal_token")" "$STATE/renew.json") || return 2
  case $status in
    2??) store_credential "$STATE/renew.json" || return 2 ;;
    408 | 429) return 2 ;;
    4??) return 3 ;;
    *) return 2 ;;
  esac
}

# renew_loop: renew once two thirds of the current credential's life has
# passed; back off on errors; stop when the control plane refuses.
renew_loop() {
  trap - TERM INT EXIT
  local expires left wait backoff=2 rc
  while :; do
    expires=$(date -u -d "$(jq -r '.expires_at' "$STATE/credential.json")" +%s)
    left=$((expires - $(date -u +%s)))
    wait=$((left * 2 / 3))
    ((wait < 1)) && wait=1
    sleep "$wait"
    while :; do
      rc=0
      renew_once || rc=$?
      case $rc in
        0) backoff=2; break ;;
        3) say "credential renewal refused; the run's credential will lapse"; return 0 ;;
        *) say "credential renewal failed; retrying in ${backoff}s"
           sleep "$backoff"; ((backoff < 30)) && backoff=$((backoff * 2)) ;;
      esac
    done
  done
}

upload_log() {
  local cred="$STATE/credential.upload.json" sealed="$STATE/run.log.sealed" akid secret session hash url
  # Upload a copy, which the renewal loop cannot append to meanwhile, without
  # the terminal colour codes arc writes.
  sed 's/\x1b\[[0-9;?]*[ -\/]*[@-~]//g' "$LOG" >"$sealed" || return 1
  # Sign with one credential throughout: the renewal loop may replace
  # credential.json between reads, and it renames a whole file into place, so
  # a copy holds one credential.
  cp "$STATE/credential.json" "$cred" || return 1
  akid=$(jq -r '.access_key_id' "$cred") || return 1
  secret=$(jq -r '.secret_access_key' "$cred") || return 1
  session=$(jq -r '.session_token' "$cred") || return 1
  hash=$(sha256sum "$sealed" | cut -d' ' -f1) || return 1
  url="$STORAGE_ENDPOINT/$STORAGE_BUCKET/${LOG_PREFIX}run.log"
  {
    printf 'user = "%s:%s"\n' "$akid" "$secret"
    printf 'header = "x-amz-security-token: %s"\n' "$session"
  } | curl --config - --fail --silent --show-error --max-time 120 --retry 3 \
        --aws-sigv4 "aws:amz:$STORAGE_REGION:s3" \
        -H "x-amz-content-sha256: $hash" \
        -H 'Content-Type: text/plain; charset=utf-8' \
        --upload-file "$sealed" --output /dev/null "$url" >&2 || return 1
  printf 'arc-runner: wrote the run log to %s/%s\n' "$STORAGE_BUCKET" "${LOG_PREFIX}run.log" >&2
}

# ---- inputs -------------------------------------------------------------------

[[ $(id -u) == 0 ]] || { say "failed at inputs: the entrypoint must start as root"; exit 10; }
install -d -m 0700 "$STATE"

RUN_ID=${RUN_ID:-}
PROTOCOL_REF=${PROTOCOL_REF:-}
EXCHANGE=${CREDENTIAL_EXCHANGE_URL:-}
FORGE=${PROTOCOL_FORGE_URL:-}
run_token=${RUN_TOKEN:-}
# The token leaves the environment before anything else starts.
unset RUN_TOKEN

[[ $RUN_ID =~ $RUN_ID_RE ]] || { say "failed at inputs: RUN_ID is missing or malformed"; exit 10; }
[[ $run_token =~ $TOKEN_RE ]] || { say "failed at inputs: RUN_TOKEN is missing or malformed"; exit 10; }
if [[ $PROTOCOL_REF =~ $REF_RE ]]; then
  namespace=${BASH_REMATCH[1]}
  name=${BASH_REMATCH[2]}
  commit=${BASH_REMATCH[3]}
else
  say "failed at inputs: PROTOCOL_REF must be <namespace>/<name>@<full commit>"
  exit 10
fi
if [[ $namespace == *..* || $name == *..* || $name == *.git ]]; then
  say "failed at inputs: PROTOCOL_REF names an invalid repository"
  exit 10
fi
require_url CREDENTIAL_EXCHANGE_URL "$EXCHANGE" || { say "failed at inputs: CREDENTIAL_EXCHANGE_URL is missing or not https"; exit 10; }
require_url PROTOCOL_FORGE_URL "$FORGE" || { say "failed at inputs: PROTOCOL_FORGE_URL is missing or not https"; exit 10; }
EXCHANGE=${EXCHANGE%/}
FORGE=${FORGE%/}

protocol_cmd=(
  setpriv --reuid="$PROTOCOL_USER" --regid="$PROTOCOL_USER" --init-groups
  --no-new-privs --inh-caps=-all --bounding-set=-all
  env -i HOME="$PROTOCOL_HOME" PATH="$SAFE_PATH" LANG=C.UTF-8 NO_COLOR=1
  RUN_ID="$RUN_ID" PROTOCOL_REF="$PROTOCOL_REF"
)

# ---- exchange -----------------------------------------------------------------

step=exchange
step_code=11
reply="$STATE/exchange.json"
status=$(post_json "$EXCHANGE/v1/runs/$RUN_ID/credentials" "$run_token" "$reply") ||
  { say "failed at exchange: no reply from the credential exchange"; exit 11; }
run_token=""
[[ $status == 2?? ]] || { say "failed at exchange: the credential exchange answered $status"; exit 11; }
store_credential "$reply" || { say "failed at exchange: the reply did not hold a usable credential"; exit 11; }
renewal_token=$(jq -r '.renewal_token // empty' "$reply")
[[ $renewal_token =~ $TOKEN_RE ]] || { say "failed at exchange: the reply's renewal_token is missing or malformed"; exit 11; }
printf '%s' "$renewal_token" >"$STATE/renewal_token"
renewal_token=""

STORAGE_ENDPOINT=$(jq -r '.storage.endpoint // empty' "$reply")
STORAGE_BUCKET=$(jq -r '.storage.bucket // empty' "$reply")
STORAGE_REGION=$(jq -r '.storage.region // "auto"' "$reply")
LOG_PREFIX=$(jq -r '.run_log.prefix // empty' "$reply")
rm -f "$reply"
require_url storage.endpoint "$STORAGE_ENDPOINT" || { say "failed at exchange: storage.endpoint is missing or not https"; exit 11; }
STORAGE_ENDPOINT=${STORAGE_ENDPOINT%/}
[[ $STORAGE_BUCKET =~ $BUCKET_RE ]] || { say "failed at exchange: storage.bucket is missing or malformed"; exit 11; }
[[ $STORAGE_REGION =~ $REGION_RE ]] || { say "failed at exchange: storage.region is malformed"; exit 11; }
if [[ ! $LOG_PREFIX =~ $PREFIX_RE || /$LOG_PREFIX == */../* || /$LOG_PREFIX == */./* || $LOG_PREFIX != */runs/"$RUN_ID"/ && $LOG_PREFIX != runs/"$RUN_ID"/ ]]; then
  say "failed at exchange: run_log.prefix must be a folder ending runs/$RUN_ID/"
  exit 11
fi

# From here on the run has somewhere to write its log.
: >"$LOG"
log_open=1
say "run $RUN_ID: $PROTOCOL_REF"

renew_loop &
renew_pid=$!

# ---- fetch --------------------------------------------------------------------

step=fetch
step_code=12
repo_url="$FORGE/$namespace/$name.git"
install -d -o "$PROTOCOL_USER" -g "$PROTOCOL_USER" -m 0755 "$WORK"
cd "$WORK"
if ! as_protocol git init --quiet "$WORK" >>"$LOG" 2>&1 ||
  ! as_protocol git -C "$WORK" fetch --quiet --depth 1 --no-tags "$repo_url" "$commit" >>"$LOG" 2>&1 ||
  ! as_protocol git -C "$WORK" -c advice.detachedHead=false checkout --quiet --detach FETCH_HEAD >>"$LOG" 2>&1; then
  finish 12 fetch "could not fetch $namespace/$name at $commit"
fi
head=$(as_protocol git -C "$WORK" rev-parse 'HEAD^{commit}') || finish 12 fetch "could not read the checkout's HEAD"
[[ $head == "$commit" ]] || finish 12 fetch "fetched $head, not the pinned $commit"
[[ -f $WORK/arcform.yaml ]] || finish 12 fetch "no arcform.yaml at the repository's root"
say "fetched $namespace/$name at $commit"

# ---- run ----------------------------------------------------------------------

step=run
step_code=13
offset=$(($(stat -c %s "$LOG") + 1))
# A simple command, not the function, so that $! is the process that becomes
# arc (setpriv and env each exec the next) and a signal sent to it reaches arc.
"${protocol_cmd[@]}" arc run >>"$LOG" 2>&1 &
arc_pid=$!
# Stream the run's output to standard output, which the operator's logs keep.
tail -c "+$offset" -f --pid="$arc_pid" "$LOG" &
tail_pid=$!

arc_status=0
while :; do
  rc=0
  wait "$arc_pid" || rc=$?
  # A trapped signal ends the wait early, while arc is still running or not yet
  # reaped, and its /proc entry is still there; waiting again reaps it.
  [[ -e /proc/$arc_pid ]] && continue
  arc_status=$rc
  break
done
arc_pid=""
wait "$tail_pid" 2>/dev/null || true

if [[ $interrupted == 1 ]]; then
  finish 15 interrupted "the run was stopped"
fi
if [[ $arc_status != 0 ]]; then
  failed_step=$(grep -o "step '[^']*' failed" "$LOG" | tail -n 1 || true)
  finish 13 run "${failed_step:+$failed_step; }arc exited $arc_status"
fi
finish 0 run
