#!/usr/bin/env bash
# shellcheck disable=SC2030,SC2031 # Test cases intentionally isolate exported state in subshells.
# Regression test for the debug-bundle library.
#
# The library's whole job happens on the failure path, which is the path a green
# run never takes: a bundle that is silently empty, unredacted, or unbounded
# looks exactly like a healthy one until someone needs it. So each property is
# driven here against synthetic processes rather than a real relay, which keeps
# it fast enough to run on every change under test/.
#
#     ./bundle-test.sh
set -euo pipefail

DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
ROOT=$(mktemp -d)
trap 'rm -rf "$ROOT"' EXIT

command -v python3 >/dev/null 2>&1 || {
    echo "bundle: python3 is required (use the nix dev shell)" >&2
    exit 1
}

failures=0
ok() { printf '  ok    %s\n' "$1"; }
bad() {
    printf '  FAIL  %s\n' "$1"
    failures=$((failures + 1))
}
# check <description> <command...>: the command is the assertion.
check() {
    local desc=$1
    shift
    if "$@" >/dev/null 2>&1; then ok "$desc"; else bad "$desc"; fi
}

mode() {
    local result
    if result=$(stat -c %a "$1" 2>/dev/null); then
        printf '%s\n' "$result"
    else
        stat -f %Lp "$1"
    fi
}

# Each case runs in its own subshell so a `bundle_init` cannot leak into the
# next one, under its own artifacts root unless CASE_ROOT shares one. The
# bundle's path lands in $ROOT/<name>.path for the caller to inspect.
run_case() {
    local name=$1 body=$2
    (
        set -euo pipefail
        export MOQ_QA_ARTIFACTS="${CASE_ROOT:-$ROOT/$name.d}"
        # shellcheck source=/dev/null
        source "$DIR/bundle.sh"
        bundle_init selftest
        bundle_rerun just test bundle
        printf '%s\n' "$BUNDLE_DIR" >"$ROOT/$name.path"
        "$body"
    ) >"$ROOT/$name.out" 2>&1 || true
    cat "$ROOT/$name.path"
}

# ── a passing run leaves nothing behind ─────────────────────────────────────
pass_case() { bundle_finish 0; }
bundle=$(run_case pass pass_case)
check "a passing run deletes its bundle" test ! -d "$bundle"

keep_case() { bundle_finish 0; }
bundle=$(MOQ_QA_KEEP=1 run_case keep keep_case)
check "MOQ_QA_KEEP retains a passing run" test -f "$bundle/manifest.json"

# Harnesses such as WASM change directories after initialization. A relative
# artifact root must keep naming the original directory throughout finalization.
relative_cwd="$ROOT/relative-cwd"
mkdir -p "$relative_cwd"
(
    cd "$relative_cwd"
    export MOQ_QA_ARTIFACTS=qa
    # shellcheck source=/dev/null
    source "$DIR/bundle.sh"
    bundle_init selftest
    printf '%s\n' "$BUNDLE_DIR" >"$ROOT/relative.path"
    cd "$DIR"
    bundle_finish 1 || true
) >"$ROOT/relative.out" 2>&1
bundle=$(<"$ROOT/relative.path")
check "a relative artifact root resolves absolutely" test "${bundle#/}" != "$bundle"
check "directory changes preserve a relative-root bundle" test -f "$bundle/manifest.json"

# ── a failing run leaves a described bundle ─────────────────────────────────
fail_case() {
    bundle_endpoint relay "http://127.0.0.1:4443" moq-lite-05
    bundle_capability browser-network "HAR only; WebTransport is invisible to it"
    bundle_result "rust -> rust" fail 20 "no data before the timeout"
    bundle_note $'diagnostic:\033[31mred'
    printf 'relay is up\n' >"$BUNDLE_WORK/relay.log"
    bundle_finish 1
}
bundle=$(run_case fail fail_case)
manifest="$bundle/manifest.json"
check "a failing run retains its bundle" test -f "$manifest"
check "a bundle is private to its owner" test "$(mode "$bundle")" = 700
check "per-process logs are retained" test -f "$bundle/work/relay.log"
check "tool versions are recorded" test -f "$bundle/versions.txt"
check "a teardown command is written" test -f "$bundle/teardown.sh"
if python3 -c 'import json,sys; json.load(open(sys.argv[1]))' "$manifest" >/dev/null 2>&1; then
    ok "the manifest is valid JSON"
else
    bad "the manifest is valid JSON"
fi
check "JSON control bytes are escaped" grep -q '\\u001b' "$manifest"
for field in '"rerun"' '"run_id"' '"commit"' 'moq-lite-05' 'rust -> rust' 'WebTransport is invisible' 'core-dumps'; do
    if grep -q -- "$field" "$manifest"; then ok "the manifest records $field"; else bad "the manifest records $field"; fi
done

rerun_env_case() {
    # shellcheck source=/dev/null
    source "$DIR/harness.sh"
    export MOQ_QA_ARTIFACTS="$ROOT/artifacts with spaces"
    harness_env_array
    bundle_rerun env "${HARNESS_ENV[@]}" just test smoke
    bundle_finish 1
}
bundle=$(MOQ_TEST_PORT_BASE=5540 MOQ_TEST_PORTS="$ROOT/ports with spaces" \
    MOQ_QA_KEEP=1 MOQ_QA_RETAIN=1 MOQ_QA_QLOG=1 MOQ_QA_STACKS=0 \
    MOQ_QA_LOG_CAP=123 MOQ_QA_FILE_CAP=456 MOQ_QA_STACK_MAX=7 MOQ_QA_STACK_TIMEOUT=9 \
    run_case rerun-env rerun_env_case)
check_rerun_arg() {
    python3 -c 'import json, shlex, sys
rerun = json.load(open(sys.argv[1]))["rerun"]
assert sys.argv[2] in shlex.split(rerun)' "$1" "$2"
}
check "recorded reruns preserve the automatic port base" check_rerun_arg \
    "$bundle/manifest.json" "MOQ_TEST_PORT_BASE=5540"
check "recorded reruns preserve the port reservation root" check_rerun_arg \
    "$bundle/manifest.json" "MOQ_TEST_PORTS=$ROOT/ports with spaces"
check "recorded reruns preserve the artifact root" check_rerun_arg \
    "$bundle/manifest.json" "MOQ_QA_ARTIFACTS=$ROOT/artifacts with spaces"
for assignment in \
    MOQ_QA_KEEP=1 MOQ_QA_RETAIN=1 MOQ_QA_QLOG=1 MOQ_QA_STACKS=0 \
    MOQ_QA_LOG_CAP=123 MOQ_QA_FILE_CAP=456 MOQ_QA_STACK_MAX=7 MOQ_QA_STACK_TIMEOUT=9; do
    check "recorded reruns preserve $assignment" check_rerun_arg "$bundle/manifest.json" "$assignment"
done

# ── credentials never reach the bundle ──────────────────────────────────────
# A JWT-shaped token, a token query parameter, an Authorization header, and URL
# credentials: the four shapes a harness log can carry one in. The harnesses run
# anonymous by design, so this is the floor under that, not a substitute for it.
secret_case() {
    {
        echo "connecting to http://127.0.0.1:4443/demo?jwt=eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJzbW9rZSJ9.c2lnbmF0dXJl"
        echo "authorization: Bearer eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJzbW9rZSJ9.c2lnbmF0dXJl"
        echo "dialing https://smoke:hunter2@relay.example/anon"
        echo "x-api-key: totally-not-a-secret-value"
        # An opaque bearer credential: the secret is the second word, so a
        # pattern that stops at the first space ships it.
        echo "Authorization: Bearer opaque-bearer-credential"
        echo 'Authorization: Bearer "opaque-quoted-bearer-secret"'
        echo 'Cookie: session=\"opaque-plain-escaped-secret\"'
        echo '> Authorization: Bearer opaque-prefixed-request-secret'
        echo '< Set-Cookie: session=opaque-prefixed-response-secret'
        echo "Proxy-Authorization: Basic opaque-proxy-credential"
        echo 'set-cookie: session=opaque-cookie-value; Path=/'
        # Upper case, because BSD sed has no case-insensitive substitution and a
        # pattern that only matches lower case would pass every check above.
        echo "GET /watch?TOKEN=opaque-query-credential"
        echo "redirect https://example.test/callback#access_token=opaque-fragment-credential"
        cat <<'HAR'
{
  "name": "Authorization",
  "value": "Bearer opaque-har-credential"
}
HAR
    } >"$BUNDLE_WORK/client.log"
    printf '%s\n' '{"Authorization":"Bearer \"opaque-keyed-json-secret\""}' \
        >"$BUNDLE_WORK/keyed.json"
    printf '%s\n' '{"first":"Authorization: Bearer opaque-json-note-secret","second":"Cookie: opaque-json-cookie-secret","status":"kept"}' \
        >"$BUNDLE_WORK/note.json"
    cat >"$BUNDLE_TRACE_LIVE/cookies.har" <<'HAR'
{
  "log": {
    "entries": [{
      "request": {
        "headers": [
          {"name": "Cookie", "value": "session=\"opaque-escaped-cookie-secret\""}
        ],
        "cookies": [
          {"name": "session", "value": "opaque-cookie-object-secret"}
        ],
        "queryString": [
          {"name": "token", "value": "opaque-har-query-secret"}
        ],
        "postData": {"params": [
          {"name": "access_token", "value": "opaque-har-form-secret"}
        ]}
      }
    }]
  }
}
HAR
    printf '%s\n' '{"headers":[{"name":"Accept","value":"text/html"},{"name":"Authorization","value":"Bearer opaque-compact-har-secret"},{"name":"Cookie","value":"opaque-compact-cookie-secret"}]}' \
        >"$BUNDLE_TRACE_LIVE/compact.har"
    printf 'request?token=opaque-trace-secret\n' >"$BUNDLE_TRACE_LIVE/browser.trace.zip"
    printf '\211PNG\r\n\032\nrendered token=opaque-screenshot-secret\n' >"$BUNDLE_TRACE_LIVE/browser.png"
    bundle_finish 1
}
bundle=$(run_case secret secret_case)
for leak in eyJhbGciOiJIUzI1NiJ9 hunter2 totally-not-a-secret-value \
    opaque-bearer-credential opaque-quoted-bearer-secret opaque-plain-escaped-secret opaque-prefixed-request-secret \
    opaque-prefixed-response-secret opaque-proxy-credential \
    opaque-cookie-value opaque-query-credential opaque-fragment-credential opaque-har-credential opaque-escaped-cookie-secret \
    opaque-cookie-object-secret opaque-compact-har-secret opaque-compact-cookie-secret \
    opaque-keyed-json-secret opaque-json-note-secret opaque-json-cookie-secret \
    opaque-har-query-secret opaque-har-form-secret \
    opaque-trace-secret opaque-screenshot-secret; do
    if grep -rq -- "$leak" "$bundle"; then bad "the redactor removes $leak"; else ok "the redactor removes $leak"; fi
done
check "redaction keeps escaped-string HAR valid" python3 -c \
    'import json,sys; json.load(open(sys.argv[1]))' "$bundle/trace/cookies.har"
check "redaction keeps compact multi-header HAR valid" python3 -c \
    'import json,sys; json.load(open(sys.argv[1]))' "$bundle/trace/compact.har"
check "redaction keeps escaped keyed-header JSON valid" python3 -c \
    'import json,sys; json.load(open(sys.argv[1]))' "$bundle/work/keyed.json"
check "redaction keeps embedded plain-header JSON valid" python3 -c \
    'import json,sys; data=json.load(open(sys.argv[1])); assert data["status"] == "kept"' \
    "$bundle/work/note.json"
check "an uploadable bundle excludes Playwright trace archives" test ! -f "$bundle/trace/browser.trace.zip"
check "a withheld Playwright trace keeps its identity" grep -q '^sha256: ' \
    "$bundle/trace/browser.trace.zip.withheld"
check "an uploadable bundle excludes screenshots" test ! -f "$bundle/trace/browser.png"
check "a withheld screenshot keeps its identity" grep -q '^sha256: ' \
    "$bundle/trace/browser.png.withheld"
check "redaction keeps the endpoint readable" grep -q "127.0.0.1:4443" "$bundle/work/client.log"
check "redaction ships the file it rewrote" test -f "$bundle/work/client.log"
check "a clean sweep leaves no failure marker" test ! -f "$bundle/REDACTION-FAILED.txt"

malformed_har_case() {
    printf '%s\n' '{"cookies":[{"name":"session","value":"opaque-malformed-har-secret"}' \
        >"$BUNDLE_TRACE_LIVE/malformed.har"
    printf '%s\n' '{"cookies":["opaque-malformed-cookie-entry-secret"]}' \
        >"$BUNDLE_TRACE_LIVE/malformed-cookie.har"
    bundle_finish 1
}
bundle=$(run_case malformed-har malformed_har_case)
check "a malformed HAR is withheld instead of partially redacted" \
    test -f "$bundle/trace/malformed.har.withheld"
check "a malformed HAR cookie entry is withheld" \
    test -f "$bundle/trace/malformed-cookie.har.withheld"
if grep -rq -- opaque-malformed-har-secret "$bundle"; then
    bad "a malformed HAR exposes no cookie value"
else
    ok "a malformed HAR exposes no cookie value"
fi
if grep -rq -- opaque-malformed-cookie-entry-secret "$bundle"; then
    bad "a malformed cookie entry exposes no value"
else
    ok "a malformed cookie entry exposes no value"
fi

snapshot_failure_case() {
    printf 'Authorization: Bearer partial-snapshot-secret\n' >"$BUNDLE_TRACE_LIVE/browser.har"
    # Simulate cp observing a live capture and then losing it to Playwright's
    # close-time rename. The partial destination must never become uploadable.
    # shellcheck disable=SC2329 # bundle_finish invokes this test double indirectly.
    cp() {
        printf 'Authorization: Bearer partial-snapshot-secret\n' >"$3/partial.har"
        return 1
    }
    bundle_finish 1
}
bundle=$(run_case snapshot-failure snapshot_failure_case)
check "a failed live snapshot leaves an explicit marker" \
    test -f "$bundle/trace/SNAPSHOT-FAILED.txt"
if grep -rq -- partial-snapshot-secret "$bundle"; then
    bad "a failed live snapshot exposes no partial capture"
else
    ok "a failed live snapshot exposes no partial capture"
fi

qlog_completion_failure_case() {
    printf '\036{"time":1}\npartial' >"$BUNDLE_QLOG_LIVE/bad.sqlog"
    printf '\036{"time":2}\n' >"$BUNDLE_QLOG_LIVE/good.sqlog"
    # shellcheck disable=SC2329 # bundle_finish invokes this test double indirectly.
    mv() {
        if [[ "$1" == *bad.sqlog.complete ]]; then
            return 1
        fi
        command mv "$@"
    }
    bundle_finish 1
}
bundle=$(run_case qlog-completion-failure qlog_completion_failure_case)
check "a failed qlog completion leaves an explicit marker" \
    test -f "$bundle/qlog/SNAPSHOT-FAILED.txt"
check "a later qlog cannot mask an earlier completion failure" \
    test ! -f "$bundle/qlog/good.sqlog"

split_secret_case() {
    printf 'Authorization: Bearer ' >"$BUNDLE_WORK/long-secret.log"
    head -c 1000 /dev/zero | tr '\0' x >>"$BUNDLE_WORK/long-secret.log"
    printf '\n' >>"$BUNDLE_WORK/long-secret.log"
    bundle_finish 1
}
bundle=$(MOQ_QA_LOG_CAP=256 run_case split-secret split_secret_case)
check "redaction precedes log truncation" python3 -c \
    'import sys; assert "x" * 100 not in open(sys.argv[1]).read()' "$bundle/work/long-secret.log"

# ── logs are bounded, with both ends kept ───────────────────────────────────
bound_case() {
    {
        echo "FIRST-LINE"
        head -c 200000 /dev/zero | tr '\0' 'x'
        echo
        echo "LAST-LINE"
    } >"$BUNDLE_WORK/huge.log"
    head -c 200000 /dev/zero >"$BUNDLE_WORK/capture.bin"
    bundle_finish 1
}
bundle=$(MOQ_QA_LOG_CAP=4096 MOQ_QA_FILE_CAP=4096 run_case bound bound_case)
size=$(wc -c <"$bundle/work/huge.log" | tr -d '[:space:]')
check "an oversized log respects the exact cap" test "$size" -le 4096
check "bounding keeps the head" grep -q FIRST-LINE "$bundle/work/huge.log"
check "bounding keeps the tail" grep -q LAST-LINE "$bundle/work/huge.log"
check "an oversized capture is dropped" test ! -f "$bundle/work/capture.bin"
check "an oversized capture leaves its identity" test -f "$bundle/work/capture.bin.omitted"

decimal_cap_case() {
    head -c 9 /dev/zero >"$BUNDLE_WORK/capture.bin"
    bundle_finish 1
}
bundle=$(MOQ_QA_FILE_CAP=08 run_case decimal-cap decimal_cap_case)
check "leading-zero caps are applied as decimal" test -f "$bundle/work/capture.bin.omitted"

qlog_cap_case() {
    local i
    for ((i = 0; i < 20; i++)); do
        printf '\036{"time":%d,"name":"transport:packet_sent"}\n' "$i"
    done >"$BUNDLE_QLOG_LIVE/relay.sqlog"
    printf '\036{"time":20,"name":"partial' >>"$BUNDLE_QLOG_LIVE/relay.sqlog"
    bundle_finish 1
}
bundle=$(MOQ_QA_LOG_CAP=128 MOQ_QA_FILE_CAP=8192 run_case qlog-cap qlog_cap_case)
count=$(LC_ALL=C tr -cd '\036' <"$bundle/qlog/relay.sqlog" | wc -c | tr -d '[:space:]')
check "qlog byte caps preserve complete JSON-SEQ records" test "$count" -eq 20
# shellcheck disable=SC2016 # Positional parameters expand in the child shell.
check "a live qlog snapshot drops its incomplete final record" \
    sh -c '! grep -q partial "$1"' _ "$bundle/qlog/relay.sqlog"

manifest_cap_case() {
    bundle_note "the manifest must remain structured even below its own size"
    bundle_finish 1
}
bundle=$(MOQ_QA_LOG_CAP=128 run_case manifest-cap manifest_cap_case)
check "a small log cap leaves valid manifest JSON" python3 -c \
    'import json,sys; json.load(open(sys.argv[1]))' "$bundle/manifest.json"

if (
    export MOQ_QA_ARTIFACTS="$ROOT/invalid-cap.d" MOQ_QA_LOG_CAP=bogus
    # shellcheck source=/dev/null
    source "$DIR/bundle.sh"
    bundle_init selftest
) >/dev/null 2>&1; then
    bad "a malformed log cap is refused"
else
    ok "a malformed log cap is refused"
fi
check "a malformed cap retains no partial bundle" test ! -d "$ROOT/invalid-cap.d"

if (
    export MOQ_QA_ARTIFACTS="$ROOT/invalid-stack.d" MOQ_QA_STACK_MAX=bogus
    # shellcheck source=/dev/null
    source "$DIR/bundle.sh"
    bundle_init selftest
) >/dev/null 2>&1; then
    bad "a malformed stack cap is refused"
else
    ok "a malformed stack cap is refused"
fi
check "a malformed stack cap retains no partial bundle" test ! -d "$ROOT/invalid-stack.d"

if (
    export MOQ_QA_ARTIFACTS="$ROOT/invalid-stack-timeout.d" MOQ_QA_STACK_TIMEOUT=bogus
    # shellcheck source=/dev/null
    source "$DIR/bundle.sh"
    bundle_init selftest
) >/dev/null 2>&1; then
    bad "a malformed stack timeout is refused"
else
    ok "a malformed stack timeout is refused"
fi
check "a malformed stack timeout retains no partial bundle" test ! -d "$ROOT/invalid-stack-timeout.d"

if (
    export MOQ_QA_ARTIFACTS="$ROOT/zero-stack-timeout.d" MOQ_QA_STACK_TIMEOUT=0
    # shellcheck source=/dev/null
    source "$DIR/bundle.sh"
    bundle_init selftest
) >/dev/null 2>&1; then
    bad "a zero stack timeout is refused"
else
    ok "a zero stack timeout is refused"
fi
check "a zero stack timeout retains no partial bundle" test ! -d "$ROOT/zero-stack-timeout.d"

if (
    export MOQ_QA_ARTIFACTS="$ROOT/invalid-retain.d" MOQ_QA_RETAIN=0
    # shellcheck source=/dev/null
    source "$DIR/bundle.sh"
    bundle_init selftest
) >/dev/null 2>&1; then
    bad "a malformed retain toggle is refused"
else
    ok "a malformed retain toggle is refused"
fi
check "a malformed retain toggle retains no partial bundle" test ! -d "$ROOT/invalid-retain.d"

sleep 120 &
identity_first=$!
sleep 120 &
identity_second=$!
first_start=$("$DIR/process-start.py" "$identity_first")
second_start=$("$DIR/process-start.py" "$identity_second")
check "same-second processes have distinct birth identities" test "$first_start" != "$second_start"
kill "$identity_first" "$identity_second" 2>/dev/null || true
wait "$identity_first" "$identity_second" 2>/dev/null || true

# ── a hung process is described before it is killed ─────────────────────────
# The stack itself may be unavailable (ptrace_scope, no debugger, a hardened
# runtime); the requirement is that the attempt is recorded and does not block.
stack_case() {
    sleep 120 &
    hung=$!
    bundle_process hung "$hung"
    bundle_stack hung "$hung"
    kill -KILL "$hung" 2>/dev/null || true
    wait "$hung" 2>/dev/null || true
    bundle_finish 1
}
started=$SECONDS
bundle=$(run_case stack stack_case)
check "stack capture is bounded" test "$((SECONDS - started))" -lt 90
check "the hung process is described" test -s "$bundle/stacks/hung.txt"
# shellcheck disable=SC2016 # Positional parameters expand in the child shell.
check "a direct process stack is not called a harness shell" sh -c \
    '! grep -q "this is the harness shell" "$1"' _ "$bundle/stacks/hung.txt"
check "the owned process is recorded" grep -q '"name": "hung"' "$bundle/manifest.json"

wrapper_stack_case() {
    sleep 120 &
    hung=$!
    bundle_process wrapper "$hung"
    bundle_stack wrapper "$hung" wrapper
    kill -KILL "$hung" 2>/dev/null || true
    wait "$hung" 2>/dev/null || true
    bundle_finish 1
}
bundle=$(run_case wrapper-stack wrapper_stack_case)
check "a childless wrapper stack identifies the harness shell" \
    grep -q "this is the harness shell" "$bundle/stacks/wrapper.txt"

recycled_stack_case() {
    sleep 120 &
    hung=$!
    bundle_process recycled "$hung"
    printf 'Mon Jan  1 00:00:00 1900' >"$BUNDLE_META/process-starts/$hung"
    bundle_stack recycled "$hung"
    kill -KILL "$hung" 2>/dev/null || true
    wait "$hung" 2>/dev/null || true
    bundle_finish 1
}
bundle=$(run_case recycled-stack recycled_stack_case)
check "a recycled process receives no debugger attachment" test ! -e "$bundle/stacks/recycled.txt"

# ── teardown reaps only what the run owned ──────────────────────────────────
teardown_case() {
    # The argument exercises a credential-shaped command without exposing it in
    # teardown.sh. Ownership is checked by process birth identity instead.
    : >"$BUNDLE_WORK/live.log"
    : >"$BUNDLE_QLOG_LIVE/live.qlog"
    mkfifo "$ROOT/continue" "$ROOT/written"
    exec 9>>"$BUNDLE_WORK/live.log"
    exec 10>>"$BUNDLE_QLOG_LIVE/live.qlog"
    python3 -c 'import os,sys,time
with open(sys.argv[1], "rb", buffering=0) as ready:
 ready.read(1)
os.write(9, b"still running\n")
os.write(10, b"still running\n")
with open(os.path.join(sys.argv[3], "late.log"), "w") as late:
 late.write("Authorization: Bearer opaque-late-retained-secret\n")
with open(sys.argv[2], "wb", buffering=0) as written:
 written.write(b"done\n")
while True:
 time.sleep(120)' "$ROOT/continue" "$ROOT/written" "$BUNDLE_WORK" '?token=teardown-secret' &
    mine=$!
    exec 9>&-
    exec 10>&-
    bundle_process mine "$mine"
    # A changed birth identity stands in for a PID the kernel has recycled:
    # teardown has to leave the current owner alone.
    bundle_process stranger "$$"
    printf 'Mon Jan  1 00:00:00 1900' >"$BUNDLE_META/process-starts/$$"
    mkfifo "$ROOT/wrapper-go" "$ROOT/wrapper-ready" "$ROOT/unowned-ready"
    set -m
    bash -c 'sleep 120 & echo $! >"$1"; echo ready >"$3"; IFS= read -r _ <"$2"' \
        _ "$ROOT/wrapper-child" "$ROOT/wrapper-go" "$ROOT/wrapper-ready" >/dev/null 2>&1 &
    wrapper=$!
    set +m
    read -r _ <"$ROOT/wrapper-ready"
    bundle_process wrapper "$wrapper"
    set -m
    bash -c 'sleep 120 & echo $! >"$1"; echo ready >"$2"; wait' \
        _ "$ROOT/unowned-child" "$ROOT/unowned-ready" >/dev/null 2>&1 &
    unowned=$!
    set +m
    read -r _ <"$ROOT/unowned-ready"
    bundle_process unowned "$unowned"
    printf 'Mon Jan  1 00:00:00 1900' >"$BUNDLE_META/process-starts/$unowned"
    printf '%s\n' "$mine" >"$BUNDLE_DIR/mine.pid"
    printf '%s\n' "$wrapper" >"$BUNDLE_DIR/wrapper.pid"
    cp "$ROOT/wrapper-child" "$BUNDLE_DIR/wrapper-child.pid"
    printf '%s\n' "$unowned" >"$BUNDLE_DIR/unowned.pid"
    cp "$ROOT/unowned-child" "$BUNDLE_DIR/unowned-child.pid"
    bundle_retain_session
    bundle_finish 1
}
# `kill -0` is not "is it running": it succeeds on a zombie, and the case above
# exits the parent that would have reaped this one, so on a host whose PID 1
# does not adopt orphans the killed sleep stays visible forever. Read the state
# instead, and treat an unreaped corpse as gone.
running() {
    local state
    state=$(ps -o state= -p "$1" 2>/dev/null | tr -d '[:space:]')
    [[ -n "$state" && "$state" != Z* ]]
}

bundle=$(MOQ_QA_RETAIN=1 run_case teardown teardown_case)
mine=$(<"$bundle/mine.pid")
wrapper=$(<"$bundle/wrapper.pid")
wrapper_child=$(<"$bundle/wrapper-child.pid")
unowned=$(<"$bundle/unowned.pid")
check "a retained session is documented" test -f "$bundle/session.md"
check "the session names a debugger attach command" grep -q "lldb -p" "$bundle/session.md"
# shellcheck disable=SC2016 # The sed expression matches literal Markdown backticks.
live=$(sed -n 's/^Live captures continue in `\([^`]*\)`.*/\1/p' "$bundle/session.md")
private_teardown="$live/teardown.sh"
# shellcheck disable=SC2016 # Positional parameters expand in the child shell.
check "the teardown script contains no recorded secret" sh -c '! grep -q teardown-secret "$1"' _ \
    "$bundle/teardown.sh"
check "surviving groups carry a member birth identity" \
    grep -q "reap_group $wrapper " "$private_teardown"
# shellcheck disable=SC2016 # Positional parameters expand in the child shell.
check "a group without a verified leader acquires no witness" sh -c \
    '! grep -q "reap_group $1 " "$2"' _ "$unowned" "$private_teardown"
# shellcheck disable=SC2016 # Positional parameters expand in the child shell.
check "a process without a verified birth identity is omitted from the session" sh -c \
    '! grep -q "pid $1 " "$2"' _ "$unowned" "$bundle/session.md"
check "retained logs live outside the uploadable bundle" test -d "$live"
printf '\036{"time":1,"name":"transport:connection_started"}\n' >"$live/qlog/later.sqlog"
check "future retained qlogs stay outside the uploadable bundle" test ! -e "$bundle/qlog/later.sqlog"
printf 'request?token=late-browser-secret\n' >"$live/trace/later.har"
check "future retained browser captures stay outside the uploadable bundle" \
    test ! -e "$bundle/trace/later.har"
before_log=$(wc -c <"$live/work/live.log" | tr -d '[:space:]')
before_qlog=$(wc -c <"$live/qlog/live.qlog" | tr -d '[:space:]')
if python3 -c 'import signal,sys
signal.signal(signal.SIGALRM, lambda *_: (_ for _ in ()).throw(TimeoutError("FIFO handshake timed out")))
signal.alarm(10)
with open(sys.argv[1], "w") as ready:
 ready.write("continue\n")
with open(sys.argv[2]) as written:
 written.readline()
' "$ROOT/continue" "$ROOT/written"; then
    ok "the retained process completed its handshake"
else
    bad "the retained process completed its handshake"
fi
after_log=$(wc -c <"$live/work/live.log" | tr -d '[:space:]')
after_qlog=$(wc -c <"$live/qlog/live.qlog" | tr -d '[:space:]')
check "retained logs continue after bundling" test "$after_log" -gt "$before_log"
check "retained qlogs continue after bundling" test "$after_qlog" -gt "$before_qlog"
check "late path-based writes stay in the live tree" \
    grep -q opaque-late-retained-secret "$live/work/late.log"
if grep -rq -- opaque-late-retained-secret "$bundle"; then
    bad "late path-based writes stay outside the uploadable bundle"
else
    ok "late path-based writes stay outside the uploadable bundle"
fi
if running "$mine"; then
    ok "a retained session leaves its processes running"
    output=$(bash "$bundle/teardown.sh" 2>&1 || true)
    if running "$mine"; then
        bad "teardown reaps the run's process"
        printf '%s\n' "$output" >&2
    else
        ok "teardown reaps the run's process"
    fi
    if running "$wrapper_child"; then
        bad "teardown reaps a surviving process group"
        kill -KILL "$wrapper_child" 2>/dev/null || true
    else
        ok "teardown reaps a surviving process group"
    fi
    if grep -Eq 'killing verified member|recorded member .* is gone or reused' <<<"$output"; then
        ok "teardown rechecks a surviving group witness"
    else
        bad "teardown rechecks a surviving group witness"
    fi
    if grep -q 'reused by another process' <<<"$output"; then
        ok "teardown skips a recycled pid"
    else
        bad "teardown skips a recycled pid"
    fi
    kill -KILL -- -"$unowned" 2>/dev/null || true
    wait "$unowned" 2>/dev/null || true
    kill -KILL "$mine" 2>/dev/null || true
else
    bad "a retained session leaves its processes running"
fi

# ── a diagnostic rerun never overwrites the original failure ────────────────
again_case() { bundle_finish 1; }
export CASE_ROOT="$ROOT/shared"
first=$(run_case first again_case)
second=$(run_case second again_case)
unset CASE_ROOT
check "a rerun writes a new bundle" test "$first" != "$second"
check "a rerun keeps the original bundle" test -f "$first/manifest.json"
check "a rerun keeps its own bundle" test -f "$second/manifest.json"

# ── retained sessions keep their reserved ports ─────────────────────────────
retained_port_case() {
    # shellcheck source=/dev/null
    source "$DIR/harness.sh"
    # shellcheck disable=SC2034 # Sourced harness functions consume this path.
    HARNESS_RUN="$BUNDLE_WORK"
    harness_port retained 4557
    harness_spawn retained "$BUNDLE_WORK/retained.log" sleep 120
    harness_retain_ports
    bundle_finish 1
}
port_root="$ROOT/ports"
bundle=$(MOQ_QA_RETAIN=1 MOQ_TEST_PORTS="$port_root" run_case retained-port retained_port_case)
check "a retained session marks its port reservation" test -f "$port_root/4557/retained"
# shellcheck disable=SC2016 # The sed expression matches literal Markdown backticks.
live=$(sed -n 's/^Live captures continue in `\([^`]*\)`.*/\1/p' "$bundle/session.md")
check "a live evidence directory is private to its owner" test "$(mode "$live")" = 700
if (
    # shellcheck source=/dev/null
    source "$DIR/harness.sh"
    HARNESS_RUN="$ROOT/contender"
    export MOQ_TEST_PORTS="$port_root"
    status=0
    harness_port contender 4557 || status=$?
    [[ "$status" -ne 0 ]]
) >/dev/null 2>&1; then
    ok "a retained session keeps its port reservation"
else
    bad "a retained session keeps its port reservation"
fi
bash "$bundle/teardown.sh" >/dev/null
check "retained teardown releases its port reservation" test ! -d "$port_root/4557"

sensitive_path_case() {
    # shellcheck source=/dev/null
    source "$DIR/harness.sh"
    # shellcheck disable=SC2034 # Sourced harness functions consume this path.
    HARNESS_RUN="$BUNDLE_WORK"
    harness_port sensitive 4560
    harness_spawn sensitive "$BUNDLE_WORK/sensitive.log" sleep 120
    harness_retain_ports
    bundle_finish 1
}
CASE_ROOT="$ROOT/token=opaque-artifact-path/artifacts"
bundle=$(MOQ_QA_RETAIN=1 MOQ_TEST_PORTS="$ROOT/secret=opaque-reservation-path/ports" \
    run_case sensitive-path sensitive_path_case)
unset CASE_ROOT
if grep -Erq -- 'opaque-artifact-path|opaque-reservation-path' "$bundle"; then
    bad "the uploadable bundle excludes sensitive teardown paths"
else
    ok "the uploadable bundle excludes sensitive teardown paths"
fi
bash "$bundle/teardown.sh" >/dev/null

leaderless_case() {
    # shellcheck source=/dev/null
    source "$DIR/harness.sh"
    # shellcheck disable=SC2034 # Sourced harness functions consume this path.
    HARNESS_RUN="$BUNDLE_WORK"
    harness_port leaderless 4559
    mkfifo "$ROOT/leaderless-ready"
    # shellcheck disable=SC2016 # The child shell expands its own positional parameters.
    harness_spawn leaderless "$BUNDLE_WORK/leaderless.log" bash -c \
        'sleep 120 & printf "%s\n" "$!" >"$1"' _ "$ROOT/leaderless-ready"
    leader=$HARNESS_PID
    read -r child <"$ROOT/leaderless-ready"
    wait "$leader" 2>/dev/null || true
    harness_retain_ports
    printf '%s\n' "$child" >"$BUNDLE_DIR/leaderless-child.pid"
    bundle_finish 1
}
bundle=$(MOQ_QA_RETAIN=1 MOQ_TEST_PORTS="$port_root" run_case leaderless leaderless_case)
child=$(<"$bundle/leaderless-child.pid")
check "a verified witness retains a leaderless group" test -f "$bundle/session.md"
check "a leaderless retained group keeps its port reservation" test -f "$port_root/4559/retained"
if running "$child"; then
    ok "a leaderless child survives for retained debugging"
else
    bad "a leaderless child survives for retained debugging"
fi
bash "$bundle/teardown.sh" >/dev/null
if running "$child"; then
    bad "retained teardown reaps a leaderless child"
else
    ok "retained teardown reaps a leaderless child"
fi
check "leaderless teardown releases its port reservation" test ! -d "$port_root/4559"

unowned_port_case() {
    # shellcheck source=/dev/null
    source "$DIR/harness.sh"
    # shellcheck disable=SC2034 # Sourced harness functions consume this path.
    HARNESS_RUN="$BUNDLE_WORK"
    harness_port unowned 4558
    harness_spawn stopped "$BUNDLE_WORK/stopped.log" true
    leader=$HARNESS_PID
    wait "$leader" 2>/dev/null || true
    if harness_retain_ports; then
        harness_reap_all
        return 1
    fi
    harness_reap_all
    harness_release_ports
    printf '%s\n' "$BUNDLE_LIVE" >"$BUNDLE_DIR/live.path"
    bundle_finish 1
}
bundle=$(MOQ_QA_RETAIN=1 MOQ_TEST_PORTS="$port_root" run_case unowned-port unowned_port_case)
check "a retained failure without an owned survivor releases its port" test ! -d "$port_root/4558"
check "a retained failure without an owned survivor has no session" test ! -e "$bundle/session.md"
live=$(<"$bundle/live.path")
check "a retained failure without an owned survivor removes its live tree" test ! -d "$live"

# A retained fixture may keep writing only into the private live tree. Auxiliary stack watchdogs
# target the uploadable snapshot, so they must be gone before its final redaction pass completes.
auxiliary_case() {
    # shellcheck source=/dev/null
    source "$DIR/harness.sh"
    # shellcheck disable=SC2034 # Sourced harness functions consume this path.
    HARNESS_RUN="$BUNDLE_WORK"
    harness_spawn retained "$BUNDLE_WORK/retained.log" sleep 120
    retained=$HARNESS_PID
    mkfifo "$ROOT/auxiliary-ready" "$ROOT/auxiliary-go"
    # shellcheck disable=SC2016 # Positional parameters expand in the child shell.
    harness_spawn_auxiliary late-stack - bash -c \
        'printf "ready\n" >"$1"; IFS= read -r _ <"$2"; printf "%s\n" "Authorization: Bearer late-watchdog-secret" >"$3/stacks/late.txt"' \
        _ "$ROOT/auxiliary-ready" "$ROOT/auxiliary-go" "$BUNDLE_DIR"
    auxiliary=$HARNESS_PID
    read -r _ <"$ROOT/auxiliary-ready"
    harness_reap_auxiliaries
    harness_retain_ports
    printf '%s\n' "$retained" >"$BUNDLE_DIR/retained.pid"
    printf '%s\n' "$auxiliary" >"$BUNDLE_DIR/auxiliary.pid"
    bundle_finish 1
}
bundle=$(MOQ_QA_RETAIN=1 run_case auxiliary auxiliary_case)
check "retained finalization reaps auxiliary writers" test ! -e "$bundle/stacks/late.txt"
if grep -rq -- late-watchdog-secret "$bundle"; then
    bad "retained finalization prevents post-redaction writes"
else
    ok "retained finalization prevents post-redaction writes"
fi
auxiliary=$(<"$bundle/auxiliary.pid")
if running "$auxiliary"; then
    bad "retained finalization stops auxiliary processes"
else
    ok "retained finalization stops auxiliary processes"
fi
retained=$(<"$bundle/retained.pid")
if running "$retained"; then
    ok "reaping auxiliaries preserves the retained fixture"
else
    bad "reaping auxiliaries preserves the retained fixture"
fi
bash "$bundle/teardown.sh" >/dev/null

if ((failures > 0)); then
    echo "bundle: $failures checks failed" >&2
    exit 1
fi
echo "bundle: all checks passed"
