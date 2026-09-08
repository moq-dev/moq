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
#   MOQ_QA_STACK_TIMEOUT  seconds allowed per debugger attach (default 20)
#
# Deliberately not collected: packet payloads and core dumps. Both carry far
# more than the failure needs and neither is safe to upload by default, so they
# stay an explicit local opt-in (see test/README.md).

# shellcheck disable=SC2034  # these are consumed by the sourcing harness

BUNDLE_LIB_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
BUNDLE_WORKSPACE=$(cd "$BUNDLE_LIB_DIR/../.." && pwd)

# Set by bundle_init.
BUNDLE_DIR=""       # the run's directory
BUNDLE_WORK=""      # retained scratch: logs, configs, captures
BUNDLE_SCRATCH=""   # never retained: compiled fixtures, plugin registries
BUNDLE_TRACE=""     # browser traces, HARs, screenshots
BUNDLE_QLOG=""      # bounded qlog snapshot inside the uploadable bundle
BUNDLE_QLOG_LIVE="" # live relay qlogs outside the uploadable bundle
BUNDLE_META=""      # jsonl fragments assembled into manifest.json
BUNDLE_LIVE=""      # live retained logs, outside the uploadable bundle
BUNDLE_HARNESS=""
BUNDLE_RUN_ID=""

_bundle_have() { command -v "$1" >/dev/null 2>&1; }

# JSON string escaping for every byte Bash can store. Bash variables cannot
# contain NUL; the remaining JSON control bytes are emitted as Unicode escapes.
_bundle_json() {
    local s=$1 code oct ch escaped
    s=${s//\\/\\\\}
    s=${s//\"/\\\"}
    for ((code = 1; code < 32; code++)); do
        printf -v oct '%03o' "$code"
        printf -v ch '%b' "\\$oct"
        printf -v escaped '\\u%04x' "$code"
        s=${s//$ch/$escaped}
    done
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
    local root="${MOQ_QA_ARTIFACTS:-${MOQ_TEST_RUNS:-$BUNDLE_WORKSPACE/target/qa}}"
    if [[ -n "${MOQ_TEST_KEEP:-}" && -z "${MOQ_QA_KEEP:-}" ]]; then
        MOQ_QA_KEEP="$MOQ_TEST_KEEP"
    fi
    local knob value expected
    for knob in MOQ_QA_LOG_CAP MOQ_QA_FILE_CAP MOQ_QA_STACK_MAX MOQ_QA_STACK_TIMEOUT; do
        value=${!knob:-}
        if [[ -n "$value" && ! "$value" =~ ^[0-9]+$ ]]; then
            echo "error: $knob must be a non-negative integer (got '$value')" >&2
            return 2
        fi
    done
    for knob in MOQ_QA_KEEP MOQ_QA_RETAIN MOQ_QA_QLOG MOQ_QA_STACKS; do
        value=${!knob:-}
        case "$knob:$value" in
            MOQ_QA_STACKS: | MOQ_QA_STACKS:0 | MOQ_QA_STACKS:1 | *: | *:1) ;;
            *)
                expected="1 or unset"
                [[ "$knob" != MOQ_QA_STACKS ]] || expected="0, 1, or unset"
                echo "error: $knob must be $expected (got '$value')" >&2
                return 2
                ;;
        esac
    done
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
    BUNDLE_LIVE="${root}-live/$BUNDLE_HARNESS-$BUNDLE_RUN_ID"
    BUNDLE_QLOG_LIVE="$BUNDLE_LIVE/qlog"
    mkdir -p "$BUNDLE_WORK" "$BUNDLE_SCRATCH" "$BUNDLE_TRACE" "$BUNDLE_QLOG" \
        "$BUNDLE_QLOG_LIVE" "$BUNDLE_META/process-starts" "$BUNDLE_DIR/stacks"
    chmod 700 "$BUNDLE_DIR" "$BUNDLE_LIVE"

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
    local command="${3:-}" started
    [[ -n "$command" ]] || command=$(ps -o command= -p "$2" 2>/dev/null | head -n 1 || true)
    started=$(ps -o lstart= -p "$2" 2>/dev/null | sed 's/^[[:space:]]*//; s/[[:space:]]*$//' || true)
    [[ -z "$started" ]] || printf '%s' "$started" >"$BUNDLE_META/process-starts/$2"
    _bundle_record processes \
        "{ \"name\": $(_bundle_json "$1"), \"pid\": $2, \"command\": $(_bundle_json "$command") }"
}

# Record an exact port reservation for retained-session teardown. This stays
# out of the manifest because it is local allocator bookkeeping, not evidence.
bundle_reservation() {
    [[ -n "$BUNDLE_META" ]] || return 0
    printf '%s\n' "$1" >>"$BUNDLE_META/reservations.txt"
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

# bundle_stack <name> <pid> [wrapper]: best-effort backtrace of a process and
# its descendants, for the case a timeout is about to destroy the evidence.
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
    local name=$1 pid=$2 wrapper=${3:-}
    [[ -n "$BUNDLE_DIR" ]] || return 0
    [[ "${MOQ_QA_STACKS:-1}" != "0" ]] || return 0
    kill -0 "$pid" 2>/dev/null || return 0

    # The whole tree, not just the named process: a cell is a shell wrapping a
    # `timeout` wrapping the client, and the client is the one that is stuck.
    # Capped, because attaching costs a second each and a tree can be a browser.
    local out="$BUNDLE_DIR/stacks/$name.txt" target walked=0 left="${MOQ_QA_STACK_MAX:-8}"
    for target in $(_bundle_descendants "$pid"); do
        if ((left-- <= 0)); then
            printf 'stopped after %s processes (MOQ_QA_STACK_MAX)\n' "${MOQ_QA_STACK_MAX:-8}" >>"$out"
            break
        fi
        kill -0 "$target" 2>/dev/null || continue
        walked=$((walked + 1))
        {
            printf '=== %s pid %s ===\n' "$name" "$target"
            ps -o pid=,command= -p "$target" 2>/dev/null || true
        } >>"$out"
        _bundle_stack_one "$target" >>"$out" 2>&1
    done

    # A tree of one is almost always the harness shell alone, which is not the
    # process anybody came here to read. Say so: a file holding a shell's stack
    # under the client's name is worse than one that admits the client had
    # already exited, because it looks like the answer.
    if [[ "$wrapper" == wrapper ]] && ((walked <= 1)); then
        printf '\nno child processes were running under pid %s at capture time;\n' "$pid" >>"$out"
        printf 'this is the harness shell, not the client it was waiting on.\n' >>"$out"
    fi
}

# Run a debugger under `timeout` where the host has one, and directly where it
# does not. A function rather than a prefix array, because an empty array is
# exactly what bash 3.2 refuses to expand under `set -u`, and aborting here
# would take the cleanup that follows down with it.
_bundle_stack_timeout() {
    if _bundle_have timeout; then
        timeout -k 5 "${MOQ_QA_STACK_TIMEOUT:-20}" "$@"
    else
        "$@"
    fi
}

# One process, whichever debugger this host has. Bounded so a wedged debugger
# cannot become the hang it was called to explain.
_bundle_stack_one() {
    local pid=$1

    if _bundle_have eu-stack; then
        _bundle_stack_timeout eu-stack -p "$pid" && return 0
    elif _bundle_have gdb; then
        _bundle_stack_timeout gdb -p "$pid" -batch -nx -ex "thread apply all bt" && return 0
    elif _bundle_have sample; then
        # macOS: samples without ptrace, so it works where lldb needs approval.
        _bundle_stack_timeout sample "$pid" 1 -f /dev/stdout && return 0
    elif _bundle_have lldb; then
        _bundle_stack_timeout lldb -p "$pid" --batch -o "thread backtrace all" -o detach -o quit && return 0
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
    local file=$1 cap=$2 bytes marker marker_bytes remaining head_keep tail_keep elided
    bytes=$(_bundle_bytes "$file")
    ((bytes > cap)) || return 0
    local tmp="$file.bounded"
    marker=$(printf '\n\n... %s bytes elided by the %s-byte MOQ_QA_LOG_CAP ...\n\n' "$bytes" "$cap")
    marker_bytes=${#marker}
    if ((cap <= marker_bytes)); then
        head -c "$cap" "$file" >"$tmp"
    else
        remaining=$((cap - marker_bytes))
        head_keep=$(((remaining + 1) / 2))
        tail_keep=$((remaining / 2))
        elided=$((bytes - head_keep - tail_keep))
        marker=$(printf '\n\n... %s bytes elided by the %s-byte MOQ_QA_LOG_CAP ...\n\n' "$elided" "$cap")
        {
            head -c "$head_keep" "$file"
            printf '%s' "$marker"
            tail -c "$tail_keep" "$file"
        } >"$tmp"
    fi
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

# Spell a literal as a case-insensitive ERE. BSD sed has no `I` flag, and a
# pattern that fails to compile there is a redaction that silently does not
# happen, so the case folding is written into the pattern instead.
_bundle_anycase() {
    local word=$1 out="" ch upper i
    for ((i = 0; i < ${#word}; i++)); do
        ch=${word:i:1}
        if [[ "$ch" == [a-z] ]]; then
            upper=$(printf '%s' "$ch" | LC_ALL=C tr '[:lower:]' '[:upper:]')
            out="${out}[${ch}${upper}]"
        else
            out="$out$ch"
        fi
    done
    printf '%s' "$out"
}

# Alternations of folded literals, built once: the sweep redacts every file in
# the bundle and the pattern never changes within a run.
_BUNDLE_RE_PARAMS=""
_BUNDLE_RE_HEADERS=""
_bundle_patterns() {
    [[ -z "$_BUNDLE_RE_PARAMS" ]] || return 0
    local word
    for word in jwt token access_token auth key secret; do
        _BUNDLE_RE_PARAMS="${_BUNDLE_RE_PARAMS:+$_BUNDLE_RE_PARAMS|}$(_bundle_anycase "$word")"
    done
    # Longest first, so `proxy-authorization` is not matched as `authorization`
    # with a stray prefix left behind.
    for word in proxy-authorization authorization set-cookie cookie x-api-key; do
        _BUNDLE_RE_HEADERS="${_BUNDLE_RE_HEADERS:+$_BUNDLE_RE_HEADERS|}$(_bundle_anycase "$word")"
    done
}

# Strip anything credential-shaped before the bundle can be uploaded. The
# harnesses run anonymous by design, so this is a floor rather than the plan:
# the plan is that no real credential is ever in scope.
#
# Fails closed: a file this cannot rewrite is replaced by its identity rather
# than shipped unread, because "the redactor errored" and "there was nothing to
# redact" are indistinguishable once the bundle leaves the machine.
_bundle_redact() {
    local file=$1
    local tmp="$file.redacted"
    if _bundle_redact_stream <"$file" >"$tmp" 2>/dev/null; then
        mv "$tmp" "$file"
        return 0
    fi
    rm -f "$tmp"
    {
        printf 'withheld: the redactor could not rewrite this file, so it is not shipped\n'
        printf 'sha256: %s\n' "$(_bundle_sha256 "$file")"
        printf 'reproduce it with the rerun command in manifest.json\n'
    } >"$file.withheld"
    rm -f "$file"
    return 1
}

# Redact one text stream. The awk pass handles HAR headers, whose name and
# value are separate JSON fields and commonly live on separate lines.
_bundle_redact_stream() {
    _bundle_patterns
    # A header value runs to the closing quote or the end of the line, not to
    # the first space: `Bearer <token>` is two words and the second is the one
    # worth having.
    LC_ALL=C sed -E \
        -e 's#(eyJ[A-Za-z0-9_-]{6,})\.([A-Za-z0-9_-]{6,})\.([A-Za-z0-9_-]{6,})#<redacted-jwt>#g' \
        -e "s#([?&]($_BUNDLE_RE_PARAMS)=)[^\&[:space:]\"']+#\1<redacted>#g" \
        -e "s#(($_BUNDLE_RE_HEADERS)\"?[:=][[:space:]]*\"?)[^\"]*#\1<redacted>#g" \
        -e 's#([a-zA-Z][a-zA-Z0-9+.-]*://)[^/[:space:]@"]+:[^/[:space:]@"]+@#\1<redacted>@#g' |
        LC_ALL=C awk '
            function closing_quote(text,    i, ch, slashes) {
                slashes = 0
                for (i = 1; i <= length(text); i++) {
                    ch = substr(text, i, 1)
                    if (ch == "\\") {
                        slashes++
                    } else {
                        if (ch == "\"" && slashes % 2 == 0) return i
                        slashes = 0
                    }
                }
                return 0
            }
            function redact_value(    start, tail, quote) {
                if (!match($0, /"value"[[:space:]]*:[[:space:]]*"/)) return
                start = RSTART + RLENGTH
                tail = substr($0, start)
                quote = closing_quote(tail)
                if (quote) $0 = substr($0, 1, start - 1) "<redacted>" substr(tail, quote)
            }
            {
                lower = tolower($0)
                if (sensitive) {
                    redact_value()
                    if (lower ~ /"value"[[:space:]]*:/) sensitive = 0
                }
                if (lower ~ /"name"[[:space:]]*:[[:space:]]*"(proxy-authorization|authorization|set-cookie|cookie|x-api-key)"/) {
                    sensitive = 1
                    redact_value()
                    if (lower ~ /"value"[[:space:]]*:/) sensitive = 0
                }
                print
            }
        '
}

# Walk everything retained: bound it, then redact it. Order matters, because
# bounding is what makes redacting a multi-megabyte log affordable.
_bundle_sweep() {
    local log_cap="${MOQ_QA_LOG_CAP:-2097152}" file_cap="${MOQ_QA_FILE_CAP:-8388608}" file
    local withheld=0
    while IFS= read -r -d '' file; do
        # This script is generated from already-redacted command values. A
        # second textual rewrite could remove printf %q escaping and make it
        # unparseable, leaving the retained processes alive.
        [[ "$file" == "$BUNDLE_DIR/teardown.sh" ]] && continue
        case "$file" in
            *.qlog | *.sqlog)
                # qlog is structured JSON-SEQ. Cutting bytes through a record
                # makes the entire trace unreadable, so keep it whole or omit
                # it under the binary-file budget.
                ((file_cap == 0)) || _bundle_bound_binary "$file" "$file_cap"
                if [[ -f "$file" ]]; then
                    _bundle_redact "$file" || withheld=$((withheld + 1))
                fi
                ;;
            *)
                if _bundle_is_text "$file"; then
                    # Byte-splicing structured metadata makes invalid JSON. Its
                    # fields are individually small, so keep the manifest whole.
                    if [[ "$file" != "$BUNDLE_DIR/manifest.json" ]]; then
                        ((log_cap == 0)) || _bundle_bound_text "$file" "$log_cap"
                    fi
                    _bundle_redact "$file" || withheld=$((withheld + 1))
                else
                    ((file_cap == 0)) || _bundle_bound_binary "$file" "$file_cap"
                fi
                ;;
        esac
    done < <(find "$BUNDLE_DIR" -type f ! -path "$BUNDLE_META/*" -print0)
    # Named in the bundle as well as on stderr: the bundle is what travels, and
    # a reader has to be able to tell a withheld file from one that was clean.
    if ((withheld > 0)); then
        printf 'redaction failed on %s file(s); they were withheld rather than shipped\n' "$withheld" \
            >"$BUNDLE_DIR/REDACTION-FAILED.txt"
        printf 'warning: %s file(s) were withheld because the redactor failed on them\n' "$withheld" >&2
    fi
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
        printf 'Live captures continue in `%s`; the bundle is the redacted failure-time snapshot.\n\n' "$BUNDLE_LIVE"
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

# Reaps only what this run recorded. The process start time is re-checked before
# the signal, because a PID freed since the run is somebody else's process now.
_bundle_teardown_script() {
    {
        printf '#!/usr/bin/env bash\n'
        printf '# Teardown for the retained %s run %s.\n' "$BUNDLE_HARNESS" "$BUNDLE_RUN_ID"
        cat <<'PRELUDE'
# Kills only the processes this run recorded, and only while their process birth
# identity still matches, so a recycled PID is left alone even if it runs the
# same command.
set -uo pipefail

kill_tree() {
    local pid="$1" child
    for child in $(pgrep -P "$pid" 2> /dev/null || true); do kill_tree "$child"; done
    kill -KILL -- -"$pid" 2> /dev/null || kill -KILL "$pid" 2> /dev/null || true
}

reap() {
    local pid="$1" want="$2" have started
    have=$(ps -o command= -p "$pid" 2> /dev/null | head -n 1 || true)
    if [[ -z "$have" ]]; then
        echo "pid $pid: gone"
        return
    fi
    started=$(ps -o lstart= -p "$pid" 2> /dev/null | sed 's/^[[:space:]]*//; s/[[:space:]]*$//' || true)
    if [[ -z "$started" || "$started" != "$want" ]]; then
        echo "pid $pid: reused by another process, skipping"
        return
    fi
    echo "pid $pid: killing $have"
    kill_tree "$pid"
}

PRELUDE
        local start_file pid
        for start_file in "$BUNDLE_META/process-starts"/*; do
            [[ -f "$start_file" ]] || continue
            pid=${start_file##*/}
            printf 'reap %s %q\n' "$pid" "$(<"$start_file")"
        done
        local reservation
        if [[ -f "$BUNDLE_META/reservations.txt" ]]; then
            while IFS= read -r reservation; do
                printf 'rm -rf -- %q\n' "$reservation"
            done <"$BUNDLE_META/reservations.txt"
        fi
        if [[ -d "$BUNDLE_LIVE" ]]; then
            printf 'rm -rf -- %q\n' "$BUNDLE_LIVE"
            printf 'rmdir %q 2>/dev/null || true\n' "$(dirname "$BUNDLE_LIVE")"
        fi
    } >"$BUNDLE_DIR/teardown.sh"
    chmod +x "$BUNDLE_DIR/teardown.sh"
}

# ── finish ──────────────────────────────────────────────────────────────────

# bundle_retained: true when the caller must leave its processes running.
# Called from the harness cleanup so a retained session survives the trap.
bundle_retained() {
    [[ "${MOQ_QA_RETAIN:-}" == "1" ]]
}

# bundle_finish <status>: keep or drop the bundle, and say which.
#
# Returns the status it was given, so `trap 'bundle_finish $?' EXIT` neither
# masks a failure nor invents one.
bundle_finish() {
    local status=${1:-0}
    [[ -n "$BUNDLE_DIR" && -d "$BUNDLE_DIR" ]] || return "$status"

    if [[ "$status" -eq 0 && -z "${MOQ_QA_KEEP:-}" ]]; then
        rm -rf "$BUNDLE_DIR" "$BUNDLE_LIVE"
        rmdir "$(dirname "$BUNDLE_DIR")" 2>/dev/null || true
        rmdir "$(dirname "$BUNDLE_LIVE")" 2>/dev/null || true
        return 0
    fi

    rm -rf "$BUNDLE_SCRATCH"
    # Relays always write outside the upload tree. Copy a failure-time snapshot
    # in for bounding and redaction; retained relays can create later files
    # without bypassing that one-time sweep.
    if find "$BUNDLE_QLOG_LIVE" -type f -print -quit | grep -q .; then
        cp -R "$BUNDLE_QLOG_LIVE/." "$BUNDLE_QLOG/"
    fi
    # An empty trace/qlog dir is a claim that nothing was captured, which the
    # capability records already make in words. Drop it so it isn't mistaken
    # for a capture that came back blank.
    rmdir "$BUNDLE_TRACE" "$BUNDLE_QLOG" "$BUNDLE_DIR/stacks" 2>/dev/null || true

    if bundle_retained && [[ "$status" -ne 0 ]]; then
        # Keep the live inodes outside the upload root, then sweep a snapshot.
        # Processes continue writing to BUNDLE_LIVE while the uploadable bundle
        # remains bounded and redacted.
        mkdir -p "$BUNDLE_LIVE"
        mv "$BUNDLE_WORK" "$BUNDLE_LIVE/work"
        mkdir -p "$BUNDLE_WORK"
        cp -R "$BUNDLE_LIVE/work/." "$BUNDLE_WORK/"
        _bundle_session
    else
        rm -rf "$BUNDLE_LIVE"
        rmdir "$(dirname "$BUNDLE_LIVE")" 2>/dev/null || true
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
