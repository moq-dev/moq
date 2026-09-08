#!/usr/bin/env bash
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
        echo "Proxy-Authorization: Basic opaque-proxy-credential"
        echo 'set-cookie: session=opaque-cookie-value; Path=/'
        # Upper case, because BSD sed has no case-insensitive substitution and a
        # pattern that only matches lower case would pass every check above.
        echo "GET /watch?TOKEN=opaque-query-credential"
        cat <<'HAR'
{
  "name": "Authorization",
  "value": "Bearer opaque-har-credential"
}
HAR
    } >"$BUNDLE_WORK/client.log"
    cat >"$BUNDLE_TRACE_LIVE/cookies.har" <<'HAR'
{
  "log": {
    "entries": [{
      "request": {
        "headers": [
          {"name": "Cookie", "value": "session=\"opaque-escaped-cookie-secret\""}
        ]
      }
    }]
  }
}
HAR
    printf 'request?token=opaque-trace-secret\n' >"$BUNDLE_TRACE_LIVE/browser.trace.zip"
    printf '\211PNG\r\n\032\nrendered token=opaque-screenshot-secret\n' >"$BUNDLE_TRACE_LIVE/browser.png"
    bundle_finish 1
}
bundle=$(run_case secret secret_case)
for leak in eyJhbGciOiJIUzI1NiJ9 hunter2 totally-not-a-secret-value \
    opaque-bearer-credential opaque-proxy-credential opaque-cookie-value opaque-query-credential \
    opaque-har-credential opaque-escaped-cookie-secret opaque-trace-secret opaque-screenshot-secret; do
    if grep -rq -- "$leak" "$bundle"; then bad "the redactor removes $leak"; else ok "the redactor removes $leak"; fi
done
check "redaction keeps escaped-string HAR valid" python3 -c \
    'import json,sys; json.load(open(sys.argv[1]))' "$bundle/trace/cookies.har"
check "an uploadable bundle excludes Playwright trace archives" test ! -f "$bundle/trace/browser.trace.zip"
check "a withheld Playwright trace keeps its identity" grep -q '^sha256: ' \
    "$bundle/trace/browser.trace.zip.withheld"
check "an uploadable bundle excludes screenshots" test ! -f "$bundle/trace/browser.png"
check "a withheld screenshot keeps its identity" grep -q '^sha256: ' \
    "$bundle/trace/browser.png.withheld"
check "redaction keeps the endpoint readable" grep -q "127.0.0.1:4443" "$bundle/work/client.log"
check "redaction ships the file it rewrote" test -f "$bundle/work/client.log"
check "a clean sweep leaves no failure marker" test ! -f "$bundle/REDACTION-FAILED.txt"

snapshot_failure_case() {
    printf 'Authorization: Bearer partial-snapshot-secret\n' >"$BUNDLE_TRACE_LIVE/browser.har"
    # Simulate cp observing a live capture and then losing it to Playwright's
    # close-time rename. The partial destination must never become uploadable.
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
check "a direct process stack is not called a harness shell" sh -c \
    '! grep -q "this is the harness shell" "$1"' _ "$bundle/stacks/hung.txt"
check "the owned process is recorded" grep -q '"name": "hung"' "$bundle/manifest.json"

wrapper_stack_case() {
    sleep 120 &
    hung=$!
    bundle_stack wrapper "$hung" wrapper
    kill -KILL "$hung" 2>/dev/null || true
    wait "$hung" 2>/dev/null || true
    bundle_finish 1
}
bundle=$(run_case wrapper-stack wrapper_stack_case)
check "a childless wrapper stack identifies the harness shell" \
    grep -q "this is the harness shell" "$bundle/stacks/wrapper.txt"

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
with open(sys.argv[2], "wb", buffering=0) as written:
 written.write(b"done\n")
while True:
 time.sleep(120)' "$ROOT/continue" "$ROOT/written" '?token=teardown-secret' &
    mine=$!
    exec 9>&-
    exec 10>&-
    bundle_process mine "$mine"
    # A changed birth identity stands in for a PID the kernel has recycled:
    # teardown has to leave the current owner alone.
    bundle_process stranger "$$"
    printf 'Mon Jan  1 00:00:00 1900' >"$BUNDLE_META/process-starts/$$"
    mkfifo "$ROOT/wrapper-go"
    set -m
    bash -c 'sleep 120 & echo $! >"$1"; IFS= read -r _ <"$2"' _ "$ROOT/wrapper-child" "$ROOT/wrapper-go" &
    wrapper=$!
    set +m
    bundle_process wrapper "$wrapper"
    printf 'go\n' >"$ROOT/wrapper-go"
    wait "$wrapper"
    printf '%s\n' "$mine" >"$BUNDLE_DIR/mine.pid"
    printf '%s\n' "$wrapper" >"$BUNDLE_DIR/wrapper.pid"
    cp "$ROOT/wrapper-child" "$BUNDLE_DIR/wrapper-child.pid"
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
check "a retained session is documented" test -f "$bundle/session.md"
check "the session names a debugger attach command" grep -q "lldb -p" "$bundle/session.md"
check "the teardown script contains no recorded secret" sh -c '! grep -q teardown-secret "$1"' _ \
    "$bundle/teardown.sh"
check "surviving groups carry a member birth identity" \
    grep -q "reap_group $wrapper " "$bundle/teardown.sh"
live=$(sed -n 's/^Live captures continue in `\([^`]*\)`.*/\1/p' "$bundle/session.md")
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
    if grep -q 'killing verified member' <<<"$output"; then
        ok "teardown verifies a surviving group before signaling it"
    else
        bad "teardown verifies a surviving group before signaling it"
    fi
    if grep -q 'reused by another process' <<<"$output"; then
        ok "teardown skips a recycled pid"
    else
        bad "teardown skips a recycled pid"
    fi
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
    HARNESS_RUN="$BUNDLE_WORK"
    harness_port retained 4557
    harness_spawn retained "$BUNDLE_WORK/retained.log" sleep 120
    harness_retain_ports
    bundle_finish 1
}
port_root="$ROOT/ports"
bundle=$(MOQ_QA_RETAIN=1 MOQ_TEST_PORTS="$port_root" run_case retained-port retained_port_case)
check "a retained session marks its port reservation" test -f "$port_root/4557/retained"
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

unowned_port_case() {
    # shellcheck source=/dev/null
    source "$DIR/harness.sh"
    HARNESS_RUN="$BUNDLE_WORK"
    harness_port unowned 4558
    if harness_retain_ports; then
        return 1
    fi
    harness_release_ports
    bundle_finish 1
}
bundle=$(MOQ_QA_RETAIN=1 MOQ_TEST_PORTS="$port_root" run_case unowned-port unowned_port_case)
check "a retained failure without live processes releases its port" test ! -d "$port_root/4558"
live=$(sed -n 's/^Live captures continue in `\([^`]*\)`.*/\1/p' "$bundle/session.md")
check "a live evidence directory is private to its owner" test "$(mode "$live")" = 700

if ((failures > 0)); then
    echo "bundle: $failures checks failed" >&2
    exit 1
fi
echo "bundle: all checks passed"
