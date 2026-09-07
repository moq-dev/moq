#!/usr/bin/env bash
# Debug bundles for the cross-language QA harnesses. Sourced, never executed.
#
# A failed end-to-end run used to leave nothing behind: every harness ran in a
# `mktemp -d` it deleted on exit, so the logs, the relay's view, and the browser
# state that explain the failure died with the process. What reached the reader
# was a tail piped through `sed`, and what reached CI was not even that.
#
# This gives every run one directory. The harness writes its scratch there
# instead of into a temp dir, so the evidence accumulates as a side effect of
# running rather than as a capture step that can be skipped. On success the
# directory is deleted; on failure it is kept, bounded, redacted, and described
# by a manifest, next to the exact command that reproduces the run.
#
#     source "$WORKSPACE/test/lib/bundle.sh"
#     bundle_init smoke
#     bundle_rerun just test smoke --publishers rust
#     TMP="$BUNDLE_WORK"
#     ...
#     trap 'bundle_finish $?' EXIT
#
# Every run gets its own directory, so re-running to investigate never
# overwrites the bundle that captured the original failure.
#
# Knobs (all optional):
#   MOQ_QA_STACK_MAX  processes to dump per stack capture (default 8)
#   MOQ_QA_ARTIFACTS  root for bundles (default $WORKSPACE/target/qa)
#   MOQ_QA_KEEP=1     keep the bundle even when the run passes
#   MOQ_QA_RETAIN=1   on failure, leave the run's processes alive for debugging
#   MOQ_QA_QLOG=1     build the relay with `--features qlog` and capture traces
#   MOQ_QA_LOG_CAP    per-text-file byte budget (default 2 MiB, 0 disables)
#   MOQ_QA_FILE_CAP   per-binary-file byte budget (default 8 MiB, 0 disables)
#   MOQ_QA_STACKS=0   skip stack capture entirely
#
# Deliberately not collected: packet payloads and core dumps. Both carry far
# more than the failure needs and neither is safe to upload by default, so they
# stay an explicit local opt-in (see test/README.md).

# shellcheck disable=SC2034  # these are consumed by the sourcing harness

BUNDLE_LIB_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
BUNDLE_WORKSPACE=$(cd "$BUNDLE_LIB_DIR/../.." && pwd)

# Set by bundle_init.
BUNDLE_DIR=""     # the run's directory
BUNDLE_WORK=""    # retained scratch: logs, configs, captures
BUNDLE_SCRATCH="" # never retained: compiled fixtures, plugin registries
BUNDLE_TRACE=""   # browser traces, HARs, screenshots
BUNDLE_QLOG=""    # relay qlog traces, when captured
BUNDLE_META=""    # jsonl fragments assembled into manifest.json
BUNDLE_HARNESS=""
BUNDLE_RUN_ID=""

_bundle_have() { command -v "$1" >/dev/null 2>&1; }

# JSON string escaping, enough for the paths, commands, and log lines recorded
# here. Bash 3.2 has no printf %q that emits JSON, so this is by hand.
_bundle_json() {
    local s=$1
    s=${s//\\/\\\\}
    s=${s//\"/\\\"}
    s=${s//$'\n'/\\n}
    s=${s//$'\r'/\\r}
    s=${s//$'\t'/\\t}
    printf '"%s"' "$s"
}

_bundle_sha256() {
    if _bundle_have sha256sum; then
        sha256sum "$1" | cut -d' ' -f1
    elif _bundle_have shasum; then
        shasum -a 256 "$1" | cut -d' ' -f1
    else
        echo "unavailable"
    fi
}

_bundle_bytes() {
    wc -c <"$1" | tr -d '[:space:]'
}

# Append one JSON object to a manifest section. The record is written the
# moment it is known, so a run killed mid-flight still leaves what it learned.
_bundle_record() {
    local section=$1
    shift
    [[ -n "$BUNDLE_META" ]] || return 0
    printf '%s\n' "$*" >>"$BUNDLE_META/$section.jsonl"
}

# bundle_init <harness>: create the run's directory and seed its identity.
bundle_init() {
    BUNDLE_HARNESS=$1
    local root="${MOQ_QA_ARTIFACTS:-$BUNDLE_WORKSPACE/target/qa}"
    # Timestamp plus PID, disambiguated if that pair is somehow already taken:
    # two runs must never share a directory, or the second would overwrite the
    # failure the first was kept for.
    local stamp suffix=1
    stamp="$(date -u +%Y%m%dT%H%M%SZ)-$$"
    BUNDLE_RUN_ID="$stamp"
    while [[ -e "$root/$BUNDLE_HARNESS-$BUNDLE_RUN_ID" ]]; do
        BUNDLE_RUN_ID="$stamp-$suffix"
        suffix=$((suffix + 1))
    done
    BUNDLE_DIR="$root/$BUNDLE_HARNESS-$BUNDLE_RUN_ID"
    BUNDLE_WORK="$BUNDLE_DIR/work"
    BUNDLE_SCRATCH="$BUNDLE_DIR/scratch"
    BUNDLE_TRACE="$BUNDLE_DIR/trace"
    BUNDLE_QLOG="$BUNDLE_DIR/qlog"
    BUNDLE_META="$BUNDLE_DIR/.meta"
    mkdir -p "$BUNDLE_WORK" "$BUNDLE_SCRATCH" "$BUNDLE_TRACE" "$BUNDLE_QLOG" "$BUNDLE_META" "$BUNDLE_DIR/stacks"

    # The drivers write browser traces here without knowing the layout.
    export MOQ_QA_BUNDLE="$BUNDLE_DIR"
    export MOQ_QA_TRACE="$BUNDLE_TRACE"

    _bundle_identity
    bundle_capability packet-payloads "not collected (explicit local opt-in only)"
    bundle_capability core-dumps "not collected (explicit local opt-in only)"
    if [[ -n "${MOQ_QA_QLOG:-}" ]]; then
        bundle_capability qlog "requested: relay built with --features qlog"
    else
        bundle_capability qlog "not captured (set MOQ_QA_QLOG=1; needs a relay rebuild)"
    fi
}

# Run identity: what was built, from which source, with which toolchain. A
# bundle without this is a pile of logs from an unknown tree.
_bundle_identity() {
    local sha branch dirty
    sha=$(git -C "$BUNDLE_WORKSPACE" rev-parse HEAD 2>/dev/null || echo unknown)
    branch=$(git -C "$BUNDLE_WORKSPACE" rev-parse --abbrev-ref HEAD 2>/dev/null || echo unknown)
    if [[ -n "$(git -C "$BUNDLE_WORKSPACE" status --porcelain 2>/dev/null)" ]]; then
        dirty=true
    else
        dirty=false
    fi
    {
        printf '  "harness": %s,\n' "$(_bundle_json "$BUNDLE_HARNESS")"
        printf '  "run_id": %s,\n' "$(_bundle_json "$BUNDLE_RUN_ID")"
        printf '  "started": %s,\n' "$(_bundle_json "$(date -u +%Y-%m-%dT%H:%M:%SZ)")"
        printf '  "host": %s,\n' "$(_bundle_json "$(uname -srm)")"
        printf '  "git": { "commit": %s, "branch": %s, "dirty": %s },\n' \
            "$(_bundle_json "$sha")" "$(_bundle_json "$branch")" "$dirty"
    } >"$BUNDLE_META/identity.json"

    # Versions as plain text as well as in the manifest: this is the file a
    # reader opens first, and `--version` output is rarely one clean line.
    # `|| true` on the pipeline, not decoration: `head` closing the pipe SIGPIPEs
    # a chatty `--version` (ffmpeg prints its whole configure line), which
    # `set -o pipefail` in the sourcing harness would otherwise turn into an
    # aborted run, in the one function that exists to describe the run.
    local tool
    for tool in cargo rustc bun node python3 ffmpeg gst-launch-1.0 tsp; do
        _bundle_have "$tool" || continue
        {
            printf '=== %s ===\n' "$tool"
            "$tool" --version 2>&1 | head -n 3 || true
        } >>"$BUNDLE_DIR/versions.txt"
    done
}

# bundle_rerun <words...>: the exact command that reproduces this run.
bundle_rerun() {
    local word first=1
    : >"$BUNDLE_META/rerun.txt"
    for word in "$@"; do
        ((first)) || printf ' ' >>"$BUNDLE_META/rerun.txt"
        first=0
        printf '%q' "$word" >>"$BUNDLE_META/rerun.txt"
    done
    printf '\n' >>"$BUNDLE_META/rerun.txt"
}

# bundle_capability <name> <state>: what this run could and could not observe.
# A capability nothing recorded is indistinguishable from one that was never
# tried, which is the gap that sends a reader looking for a trace that was
# never going to exist.
bundle_capability() {
    _bundle_record capabilities "{ \"name\": $(_bundle_json "$1"), \"state\": $(_bundle_json "$2") }"
}

# bundle_endpoint <name> <url> [protocol]: the topology under test.
bundle_endpoint() {
    _bundle_record endpoints \
        "{ \"name\": $(_bundle_json "$1"), \"url\": $(_bundle_json "$2"), \"protocol\": $(_bundle_json "${3:-}") }"
}

# bundle_fixture <path> [role]: identify an input by content, so a rerun that
# behaves differently can be told apart from one fed different bytes.
bundle_fixture() {
    [[ -f "$1" ]] || return 0
    _bundle_record fixtures "{ \"path\": $(_bundle_json "$1"), \"role\": $(_bundle_json "${2:-input}"), \
\"bytes\": $(_bundle_bytes "$1"), \"sha256\": $(_bundle_json "$(_bundle_sha256 "$1")") }"
}

# bundle_binary <name> <path>: build identity for a native process, so a stack
# or a crash can be matched against the symbols that produced it.
bundle_binary() {
    [[ -f "$2" ]] || return 0
    _bundle_record binaries "{ \"name\": $(_bundle_json "$1"), \"path\": $(_bundle_json "$2"), \
\"bytes\": $(_bundle_bytes "$2"), \"sha256\": $(_bundle_json "$(_bundle_sha256 "$2")") }"
}

# bundle_process <name> <pid> [command]: a native process this run owns.
# Recorded so the retained-session teardown can reap exactly these and nothing
# else. The command is read from the process unless one is given.
bundle_process() {
    local command="${3:-}"
    [[ -n "$command" ]] || command=$(ps -o command= -p "$2" 2>/dev/null | head -n 1 || true)
    _bundle_record processes \
        "{ \"name\": $(_bundle_json "$1"), \"pid\": $2, \"command\": $(_bundle_json "$command") }"
}

# bundle_result <name> <status> <seconds> [detail]: one matrix cell or case.
bundle_result() {
    _bundle_record results "{ \"name\": $(_bundle_json "$1"), \"status\": $(_bundle_json "$2"), \
\"seconds\": $(_bundle_json "${3:-}"), \"detail\": $(_bundle_json "${4:-}") }"
}

# bundle_note <text>: anything the manifest has no field for.
bundle_note() {
    _bundle_record notes "$(_bundle_json "$*")"
}

# ── stacks ──────────────────────────────────────────────────────────────────

# bundle_stack <name> <pid>: best-effort backtrace of a process and its
# descendants, for the case a timeout is about to destroy the evidence.
#
# Never fatal and never blocking. A debugger that cannot attach is the common
# case (Linux ptrace_scope, macOS SIP, no debugger installed), and a run that
# died because it could not read a stack would be worse than one without the
# stack. The reason is recorded instead, because "no stack file" and "the
# kernel refused" send a reader in very different directions.
_bundle_descendants() {
    local pid=$1 child
    printf '%s\n' "$pid"
    for child in $(pgrep -P "$pid" 2>/dev/null || true); do _bundle_descendants "$child"; done
}

bundle_stack() {
    local name=$1 pid=$2
    [[ -n "$BUNDLE_DIR" ]] || return 0
    [[ "${MOQ_QA_STACKS:-1}" != "0" ]] || return 0
    kill -0 "$pid" 2>/dev/null || return 0

    # The whole tree, not just the named process: a cell is a shell wrapping a
    # `timeout` wrapping the client, and the client is the one that is stuck.
    # Capped, because attaching costs a second each and a tree can be a browser.
    local out="$BUNDLE_DIR/stacks/$name.txt" target left="${MOQ_QA_STACK_MAX:-8}"
    for target in $(_bundle_descendants "$pid"); do
        if ((left-- <= 0)); then
            printf 'stopped after %s processes (MOQ_QA_STACK_MAX)\n' "${MOQ_QA_STACK_MAX:-8}" >>"$out"
            break
        fi
        kill -0 "$target" 2>/dev/null || continue
        {
            printf '=== %s pid %s ===\n' "$name" "$target"
            ps -o pid=,command= -p "$target" 2>/dev/null || true
        } >>"$out"
        _bundle_stack_one "$target" >>"$out" 2>&1
    done
}

# One process, whichever debugger this host has. Bounded so a wedged debugger
# cannot become the hang it was called to explain.
_bundle_stack_one() {
    local pid=$1 budget="${MOQ_QA_STACK_TIMEOUT:-20}"
    # ${arr[@]+...} guard: bash 3.2 errors on an empty array under `set -u`.
    local runner=()
    _bundle_have timeout && runner=(timeout -k 5 "$budget")
    local run=(${runner[@]+"${runner[@]}"})

    if _bundle_have eu-stack; then
        "${run[@]}" eu-stack -p "$pid" && return 0
    elif _bundle_have gdb; then
        "${run[@]}" gdb -p "$pid" -batch -nx -ex "thread apply all bt" && return 0
    elif _bundle_have sample; then
        # macOS: samples without ptrace, so it works where lldb needs approval.
        "${run[@]}" sample "$pid" 1 -f /dev/stdout && return 0
    elif _bundle_have lldb; then
        "${run[@]}" lldb -p "$pid" --batch -o "thread backtrace all" -o detach -o quit && return 0
    else
        echo "no stack capture tool found (looked for eu-stack, gdb, sample, lldb)"
        return 0
    fi

    echo "stack capture failed; the debugger could not attach to pid $pid"
    echo "  linux: sysctl kernel.yama.ptrace_scope=0, or run inside a container with SYS_PTRACE"
    echo "  macos: /usr/bin/sample needs the process to be yours and not hardened-runtime protected"
    return 0
}

# ── retention ───────────────────────────────────────────────────────────────

# Keep both ends of an oversized text file. The head says how the run was set
# up and the tail says how it died; the middle is the part nobody reads.
_bundle_bound_text() {
    local file=$1 cap=$2 bytes
    bytes=$(_bundle_bytes "$file")
    ((bytes > cap)) || return 0
    local keep=$((cap / 2)) tmp="$file.bounded"
    {
        head -c "$keep" "$file"
        printf '\n\n... %s bytes elided by the %s-byte MOQ_QA_LOG_CAP ...\n\n' "$((bytes - cap))" "$cap"
        tail -c "$keep" "$file"
    } >"$tmp"
    mv "$tmp" "$file"
}

# An oversized binary is replaced by its identity. Media captures are the usual
# offender, and the rerun command regenerates them from the same fixtures.
_bundle_bound_binary() {
    local file=$1 cap=$2 bytes
    bytes=$(_bundle_bytes "$file")
    ((bytes > cap)) || return 0
    {
        printf 'omitted: %s bytes exceeds the %s-byte MOQ_QA_FILE_CAP\n' "$bytes" "$cap"
        printf 'sha256: %s\n' "$(_bundle_sha256 "$file")"
        printf 'regenerate with the rerun command in manifest.json\n'
    } >"$file.omitted"
    rm -f "$file"
}

_bundle_is_text() {
    # `file` is not everywhere, and `grep -I` answers exactly the question
    # asked: does this contain a NUL byte. An empty file has no lines to match,
    # so it is decided first rather than being reported as binary.
    [[ -s "$1" ]] || return 0
    LC_ALL=C grep -qI '' "$1" 2>/dev/null
}

# Strip anything credential-shaped before the bundle can be uploaded. The
# harnesses run anonymous by design, so this is a floor rather than the plan:
# the plan is that no real credential is ever in scope.
_bundle_redact() {
    local file=$1
    local tmp="$file.redacted"
    if LC_ALL=C sed -E \
        -e 's#(eyJ[A-Za-z0-9_-]{6,})\.([A-Za-z0-9_-]{6,})\.([A-Za-z0-9_-]{6,})#<redacted-jwt>#g' \
        -e 's#([?&](jwt|token|access_token|auth|key|secret)=)[^&[:space:]"'"'"']+#\1<redacted>#gI' \
        -e 's#((authorization|proxy-authorization|cookie|set-cookie|x-api-key)"?[:=][[:space:]]*"?)[^"[:space:]]+#\1<redacted>#gI' \
        -e 's#([a-zA-Z][a-zA-Z0-9+.-]*://)[^/[:space:]@"]+:[^/[:space:]@"]+@#\1<redacted>@#g' \
        "$file" >"$tmp" 2>/dev/null; then
        mv "$tmp" "$file"
    else
        rm -f "$tmp"
    fi
}

# Walk everything retained: bound it, then redact it. Order matters, because
# bounding is what makes redacting a multi-megabyte log affordable.
_bundle_sweep() {
    local log_cap="${MOQ_QA_LOG_CAP:-2097152}" file_cap="${MOQ_QA_FILE_CAP:-8388608}" file
    while IFS= read -r -d '' file; do
        if _bundle_is_text "$file"; then
            ((log_cap == 0)) || _bundle_bound_text "$file" "$log_cap"
            _bundle_redact "$file"
        else
            ((file_cap == 0)) || _bundle_bound_binary "$file" "$file_cap"
        fi
    done < <(find "$BUNDLE_DIR" -type f ! -path "$BUNDLE_META/*" -print0)
}

# ── manifest ────────────────────────────────────────────────────────────────

# Wrap a section's jsonl fragments into an array, or an empty one.
_bundle_section() {
    local section=$1
    local file="$BUNDLE_META/$section.jsonl"
    printf '  "%s": [' "$section"
    if [[ -s "$file" ]]; then
        printf '\n'
        # `sed '$!s/$/,/'` commas every line but the last, which is what turns
        # append-only fragments into an array without re-reading them.
        sed '$!s/$/,/; s/^/    /' "$file"
        printf '  '
    fi
    printf ']'
}

_bundle_manifest() {
    local status=$1 rerun=""
    [[ -f "$BUNDLE_META/rerun.txt" ]] && rerun=$(<"$BUNDLE_META/rerun.txt")
    {
        printf '{\n'
        cat "$BUNDLE_META/identity.json"
        printf '  "finished": %s,\n' "$(_bundle_json "$(date -u +%Y-%m-%dT%H:%M:%SZ)")"
        printf '  "status": %s,\n' "$status"
        printf '  "rerun": %s,\n' "$(_bundle_json "${rerun%$'\n'}")"
        local section
        for section in capabilities endpoints binaries fixtures processes results notes; do
            _bundle_section "$section"
            printf ',\n'
        done
        printf '  "versions": "versions.txt"\n'
        printf '}\n'
    } >"$BUNDLE_DIR/manifest.json"
}

# A retained session is only useful if the reader is handed the four things
# they would otherwise have to reconstruct: where it is, what is running, how
# to attach, and how to stop it.
_bundle_session() {
    local file="$BUNDLE_DIR/session.md" pid name line
    {
        printf '# Retained session\n\n'
        printf 'Processes from this run are still alive. They hold their ports until torn down.\n\n'
        printf '## Endpoints\n\n'
        [[ -f "$BUNDLE_META/endpoints.jsonl" ]] &&
            sed -n 's/.*"url": "\([^"]*\)".*/- \1/p' "$BUNDLE_META/endpoints.jsonl"
        printf '\n## Processes\n\n'
        if [[ -f "$BUNDLE_META/processes.jsonl" ]]; then
            while IFS= read -r line; do
                pid=$(printf '%s' "$line" | sed -n 's/.*"pid": \([0-9]*\).*/\1/p')
                name=$(printf '%s' "$line" | sed -n 's/.*"name": "\([^"]*\)".*/\1/p')
                [[ -n "$pid" ]] || continue
                kill -0 "$pid" 2>/dev/null || continue
                # shellcheck disable=SC2016  # backticks are Markdown here, not a subshell
                printf -- '- %s: pid %s -- attach with `lldb -p %s` or `gdb -p %s`\n' "$name" "$pid" "$pid" "$pid"
            done <"$BUNDLE_META/processes.jsonl"
        fi
        printf '\n## Teardown\n\n'
        # shellcheck disable=SC2016  # backticks are Markdown here, not a subshell
        printf '```bash\nbash %s/teardown.sh\n```\n' "$BUNDLE_DIR"
    } >"$file"
}

# Reaps only what this run recorded. The command is re-checked before the
# signal, because a PID freed since the run is somebody else's process now.
_bundle_teardown_script() {
    {
        printf '#!/usr/bin/env bash\n'
        printf '# Teardown for the retained %s run %s.\n' "$BUNDLE_HARNESS" "$BUNDLE_RUN_ID"
        cat <<'PRELUDE'
# Kills only the processes this run recorded, and only while they still match
# the command they were started with, so a recycled PID is left alone.
set -uo pipefail

kill_tree() {
    local pid="$1" child
    for child in $(pgrep -P "$pid" 2> /dev/null || true); do kill_tree "$child"; done
    kill -KILL "$pid" 2> /dev/null || true
}

reap() {
    local pid="$1" want="$2" have
    have=$(ps -o command= -p "$pid" 2> /dev/null | head -n 1 || true)
    if [[ -z "$have" ]]; then
        echo "pid $pid: gone"
        return
    fi
    if [[ "$have" != "$want" ]]; then
        echo "pid $pid: reused by another process, skipping"
        return
    fi
    echo "pid $pid: killing $have"
    kill_tree "$pid"
}

PRELUDE
        local line pid command
        if [[ -f "$BUNDLE_META/processes.jsonl" ]]; then
            while IFS= read -r line; do
                pid=$(printf '%s' "$line" | sed -n 's/.*"pid": \([0-9]*\).*/\1/p')
                command=$(printf '%s' "$line" | sed -n 's/.*"command": "\(.*\)" }$/\1/p')
                [[ -n "$pid" ]] || continue
                printf 'reap %s %q\n' "$pid" "$command"
            done <"$BUNDLE_META/processes.jsonl"
        fi
    } >"$BUNDLE_DIR/teardown.sh"
    chmod +x "$BUNDLE_DIR/teardown.sh"
}

# ── finish ──────────────────────────────────────────────────────────────────

# bundle_retained: true when the caller must leave its processes running.
# Called from the harness cleanup so a retained session survives the trap.
bundle_retained() {
    [[ -n "${MOQ_QA_RETAIN:-}" ]]
}

# bundle_finish <status>: keep or drop the bundle, and say which.
#
# Returns the status it was given, so `trap 'bundle_finish $?' EXIT` neither
# masks a failure nor invents one.
bundle_finish() {
    local status=${1:-0}
    [[ -n "$BUNDLE_DIR" && -d "$BUNDLE_DIR" ]] || return "$status"

    if [[ "$status" -eq 0 && -z "${MOQ_QA_KEEP:-}" ]]; then
        rm -rf "$BUNDLE_DIR"
        rmdir "$(dirname "$BUNDLE_DIR")" 2>/dev/null || true
        return 0
    fi

    rm -rf "$BUNDLE_SCRATCH"
    # An empty trace/qlog dir is a claim that nothing was captured, which the
    # capability records already make in words. Drop it so it isn't mistaken
    # for a capture that came back blank.
    rmdir "$BUNDLE_TRACE" "$BUNDLE_QLOG" "$BUNDLE_DIR/stacks" 2>/dev/null || true

    if bundle_retained && [[ "$status" -ne 0 ]]; then
        _bundle_session
    fi
    _bundle_teardown_script
    _bundle_manifest "$status"
    rm -rf "$BUNDLE_META"
    # After the manifest, so the sweep redacts that too: it carries the rerun
    # command and every endpoint URL.
    _bundle_sweep

    {
        printf '\n── debug bundle ─────────────────────────────────────────────\n'
        printf '  path:     %s\n' "$BUNDLE_DIR"
        printf '  manifest: %s/manifest.json\n' "$BUNDLE_DIR"
        if [[ -f "$BUNDLE_DIR/session.md" ]]; then
            printf '  session:  %s/session.md (processes left running)\n' "$BUNDLE_DIR"
            printf '  teardown: bash %s/teardown.sh\n' "$BUNDLE_DIR"
        fi
        printf '  rerun:    %s\n' "$(sed -n 's/"rerun": "\(.*\)",$/\1/p' "$BUNDLE_DIR/manifest.json" | head -n 1)"
        printf '─────────────────────────────────────────────────────────────\n'
    } >&2

    return "$status"
}
