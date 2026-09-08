#!/usr/bin/env bash
#
# Report which verification suites this checkout can actually run, and why a
# capability is unavailable, before anything starts a build. A green local
# command that silently skipped half its tools is the failure this prevents.
#
# Diagnosis only. It never installs a tool, approves an .envrc, widens a
# sandbox, or changes a setting; every remedy names the narrow path, socket, or
# capability that is missing rather than recommending unrestricted access.
#
# Modes:
#   doctor.sh [--json] [--strict] [--base REF] [--suite NAME]...
#   doctor.sh --tools [FILES]   Print the tools a changed-file list needs.
#   doctor.sh --test-tools [FILES]   Print tools the test dispatch needs.
#   doctor.sh --self-test       Check the classifier, budget, and JSON encoder.
#
# Written for bash 3.2 (no associative arrays, no mapfile): the environments
# worth diagnosing are exactly the ones without the good bash.

set -uo pipefail

# A probe reports one of these. `degraded` is the one that only a behavior check
# can produce: the binary is on PATH and answers --version, and still cannot do
# the job the repo needs it for.
#   ok        usable
#   missing   not installed
#   denied    present but refused by permissions or the sandbox
#   timeout   present but did not answer within its budget
#   degraded  present and answering, but the wrong version or behavior
#   skip      not needed by the selected suites, so not probed

usage() {
    cat >&2 <<'EOF'
Usage:
  just doctor [--json] [--strict] [--base REF]
              [--suite check|test|smoke|smoke-full|wasm|all]...
  scripts/doctor.sh --tools [FILES]
  scripts/doctor.sh --test-tools [FILES]
  scripts/doctor.sh --self-test
EOF
}

# ---------------------------------------------------------------------------
# Tool requirements
#
# The single source of truth for "what does this diff need installed", shared
# with the root justfile's `_tools`. Two lists would drift, and the one that
# drifted quietly would be the one turning a skip into a pass.
#
# One deliberate absence: swift exists only on macOS and `swift check` skips
# off-macOS by design, so swift.yml is its real gate.
# ---------------------------------------------------------------------------

# Print the tools a changed-file list needs, one per line. `ALL` requires
# everything, matching `just check-all`.
tools_for_files() {
    local files=$1 tools

    scoped() { [ "$files" = ALL ] || printf '%s\n' "$files" | grep -qE "$1"; }

    # `_check-common` runs on every invocation, so its tools are unconditional.
    tools="actionlint bun jq nix nixfmt shellcheck shfmt taplo"
    scoped '^(quest/|rs/|Cargo\.(toml|lock)$|rust-toolchain\.toml$)' && tools="$tools cargo envsubst"
    scoped '^(py/|pyproject\.toml$|uv\.lock$|rs/moq-ffi/)' && tools="$tools uv"
    # Kotlin's generator builds the Rust FFI before Gradle compiles the wrapper.
    scoped '^(kt/|rs/moq-ffi/)' && tools="$tools cargo rustc gradle java"
    # cargo because `go check` builds moq-ffi for the host, and skips on a
    # missing cargo the same way it skips on a missing go. rsync because the
    # publish scripts stage the mirror tree with it, so the publisher test skips
    # without it, and a skip that keeps `just check` green is what MOQ_STRICT is
    # here to prevent.
    scoped '^(go/|rs/moq-ffi/)' && tools="$tools go uniffi-bindgen-go cargo rsync"
    scoped '^(dart/|rs/moq-ffi/)' && tools="$tools cargo dart uniffi_bindgen_dart"
    # Two obs recipes with two dispatch scopes, so two lines: over-requiring
    # would fail a diff that never runs the recipe. `just obs compile` needs
    # cargo to regenerate moq.h and pkg-config to locate Qt6 and ffmpeg.
    scoped '^(cpp/obs/|rs/libmoq/|flake\.nix$)' && tools="$tools pkg-config cargo"
    # `just obs check` lints with clang-format and gersemi, validates the CMake
    # release configuration, and compares the three OBS pins, one of which moves
    # on a flake.lock bump alone.
    scoped '^(cpp/obs/|flake\.(nix|lock)$)' && tools="$tools clang-format gersemi cmake"

    unset -f scoped
    # Scopes overlap (rs/moq-ffi/ is in five of them), so the same tool lands in
    # the list more than once; sort -u is what makes it a set.
    printf '%s' "$tools" | tr ' ' '\n' | sort -u
}

# Print the tools a cross-language suite refuses to start without.
#
# Exactly what each harness checks: smoke's `require_tools`, the guard at the
# top of test/wasm/run.sh, plus the curl that run.sh polls every relay with.
#
# Plain `smoke` runs the Rust matrix, so a per-client toolchain it merely marks
# broken is deliberately absent: that client is not in the matrix, and its
# absence fails nothing. `smoke-full` names every client on its command line, a
# broken one sets `overall=1`, so there the whole fixed matrix is a
# prerequisite: uv for python, bun for the browser and both native JS clients
# plus node for one of them, go and uniffi-bindgen-go for go, a C compiler for
# c, and a system GStreamer for gst.
tools_for_suite() {
    case $1 in
        smoke) printf 'cargo\ncurl\nffmpeg\ntimeout\n' ;;
        smoke-full)
            printf 'bun\ncargo\ncurl\nffmpeg\ntimeout\n'
            printf 'go\ngst-inspect-1.0\ngst-launch-1.0\nnode\nuniffi-bindgen-go\nuv\n'
            ;;
        wasm) printf 'bun\ncargo\ncurl\nwasm-bindgen\n' ;;
        *) : ;;
    esac
}

# Print the file scope `just check` would really use, given the one it was
# handed. The dispatch itself lives in `justfile` and `test/justfile`, and
# neither matches any language scope, so `check` hands a diff touching either
# one to `check-all` instead. Diagnosing the narrow scope there would clear a
# run that is about to need every tool in the repository.
widen_orchestration() {
    if printf '%s\n' "$1" | grep -qE '^(justfile|test/justfile)$'; then
        printf 'ALL\n'
    else
        printf '%s\n' "$1"
    fi
}

# Print the tools the test dispatcher actually reaches for a changed-file list.
# Unlike check-all, test/all covers only JS, Rust, and Python. An orchestration
# diff still runs `_tools` on the original list before widening to those three.
tools_for_test_files() {
    local files=$1 tools=""
    scoped() { [ "$files" = ALL ] || printf '%s\n' "$files" | grep -qE "$1"; }

    scoped '^(js/|doc/|demo/(boy|web)/|test/smoke/clients/js|test/wasm/|package\.json$|bun\.lock(b)?$|biome\.jsonc$)' &&
        tools="$tools bun"
    scoped '^(Cargo\.(toml|lock)$|rust-toolchain\.toml$|rs/justfile$|\.config/nextest\.toml$|rs/)' &&
        tools="$tools cargo"
    # Python's test setup builds the Rust FFI through maturin.
    scoped '^(py/|pyproject\.toml$|uv\.lock$|rs/moq-ffi/)' && tools="$tools cargo uv"
    if printf '%s\n' "$files" | grep -qE '^(justfile|test/justfile)$'; then
        tools="$tools bun cargo uv"
    fi
    unset -f scoped
    printf '%s' "$tools" | tr ' ' '\n' | grep -v '^$' | sort -u
}

# Print which suite needs which tool, one `tool suite` pair per line, for a
# suite list and a changed-file scope. Per-suite ownership rather than one
# union: the file scope's tools belong to `check` and `test`, which are what
# dispatch them, so a missing actionlint must not block a strict `--suite smoke`
# diagnosis of a harness that never invokes it.
#
# git and just carry the dispatch itself, so they belong to every suite;
# `_tools` leaves them out because just is what runs it.
tool_pairs() {
    local suites=$1 changed=$2
    local test_files=${3:-$changed} suite tool
    for suite in $suites; do
        for tool in git just; do
            printf '%s %s\n' "$tool" "$suite"
        done
        case $suite in
            check)
                for tool in $(tools_for_files "$changed"); do
                    printf '%s %s\n' "$tool" "$suite"
                done
                ;;
            test)
                for tool in $(tools_for_test_files "$test_files"); do
                    printf '%s %s\n' "$tool" "$suite"
                done
                ;;
        esac
        for tool in $(tools_for_suite "$suite"); do
            printf '%s %s\n' "$tool" "$suite"
        done
    done
}

# The suites in PAIRS that named a tool, deduplicated and in the order they were
# given.
suites_for_tool() {
    local out="" t s
    while read -r t s; do
        [ "$t" = "$1" ] || continue
        case " $out " in *" $s "*) ;; *) out="$out $s" ;; esac
    done <<<"${PAIRS:-}"
    printf '%s' "${out# }"
}

# Ask the real Rust selector once, then reuse its answer for every capability
# owned by that dispatch. A diff can select dependents it never names, which no
# path pattern here could reproduce safely.
rust_packages() {
    local changed=$1
    [ -n "$changed" ] || return 0
    command -v just >/dev/null 2>&1 || return 1
    [ "$changed" = ALL ] && {
        printf 'ALL\n'
        return
    }
    just rs _select "$changed"
}

# True when the selector receives more than one changed crate as its seed list.
# That is the only narrow case that puts a literal newline in awk's `-v`
# argument; workspace-wide inputs return before awk, and a single seed is valid
# on both BSD awk and gawk even when it later selects multiple dependents.
needs_awk_select() {
    local changed=$1 count
    [ "$changed" != ALL ] || return 1
    count=$(printf '%s\n' "$changed" | sed -n 's|^rs/\([^/]*\)/.*|\1|p' | sort -u | grep -c . || true)
    ((count > 1))
}

# Print the selected check/test suites that invoke the Rust recipes. Cargo can
# also be reached through Python and bindings checks, but those paths do not run
# clippy, rustfmt, cargo-shear, cargo-sort, or nextest.
rust_suites() {
    [ -n "$1" ] || return
    selected "check test"
}

# True when the selected Rust packages contain tests that bind real sockets.
# Pure protocol/container crates use in-memory transports; keep them runnable in
# sandboxes that deny bind. Update this set when a crate adds or removes an OS
# socket test.
rust_tests_bind() {
    local packages=$1 protocol=$2 pattern
    [ -n "$packages" ] || return 1
    [ "$packages" = ALL ] && return 0
    case $protocol in
        tcp) pattern='(moq-hls|moq-native|moq-relay|moq-rtc|moq-rtmp)' ;;
        udp) pattern='(moq-ffi|moq-native|moq-relay|moq-rtc|moq-srt)' ;;
        *) return 1 ;;
    esac
    printf '%s\n' "$packages" | grep -qwE "$pattern"
}

# Print Cargo's writable cache directory without assuming HOME exists. Minimal
# containers and sandboxes may omit it, which is a capability result to report,
# not a shell error that aborts the diagnosis.
cargo_home() {
    if [ -n "${CARGO_HOME:-}" ]; then
        printf '%s\n' "$CARGO_HOME"
    elif [ -n "${HOME:-}" ]; then
        printf '%s/.cargo\n' "$HOME"
    else
        return 1
    fi
}

# Resolve the same per-user roots as test/lib/harness.sh. Explicit overrides are
# shared across worktrees by definition; defaults are namespaced by uid.
harness_root() {
    local kind=$1 root tmpdir=${HARNESS_TMPDIR:-${TMPDIR:-/tmp}}
    case $kind in
        runs) root=${MOQ_TEST_RUNS:-$tmpdir} ;;
        ports) root=${MOQ_TEST_PORTS:-$tmpdir} ;;
        *) return 1 ;;
    esac
    while [ "$root" != / ] && [ "${root%/}" != "$root" ]; do
        root=${root%/}
    done
    case $root in
        /*) ;;
        *) root="$REPO/test/$root" ;;
    esac
    if [ "$kind" = runs ] && [ -z "${MOQ_TEST_RUNS:-}" ]; then
        root="$root/moq-test-$(id -u)"
    elif [ "$kind" = ports ] && [ -z "${MOQ_TEST_PORTS:-}" ]; then
        root="$root/moq-test-ports-$(id -u)"
    fi
    printf '%s\n' "$root"
}

# Print an absolute root for doctor's private scratch files. A relative TMPDIR
# belongs to test/ for the harness recipes and must not be reinterpreted from
# doctor's repository-root working directory.
doctor_tmpdir() {
    case ${TMPDIR:-/tmp} in
        /*) printf '%s\n' "${TMPDIR:-/tmp}" ;;
        *) printf '/tmp\n' ;;
    esac
}

# True when a configured harness port is in the range the shared allocator accepts.
valid_harness_port() {
    local port=$1
    [[ "$port" =~ ^[1-9][0-9]*$ ]] && ((${#port} <= 5)) && ((port >= 1024 && port <= 65535))
}

# True when an allocator base leaves COUNT candidates after excluding a pinned
# port already reserved inside its 501-port search window.
valid_harness_port_span() {
    local port=$1 count=$2 excluded=${3:-} last available
    valid_harness_port "$port" || return 1
    last=$((port + 500))
    ((last <= 65535)) || last=65535
    available=$((last - port + 1))
    if [ -n "$excluded" ] && valid_harness_port "$excluded" &&
        ((excluded >= port && excluded <= last)); then
        available=$((available - 1))
    fi
    ((available >= count))
}

# Record whether one harness port setting can supply the required slots.
probe_harness_port() {
    local id=$1 name=$2 value=$3 slots=$4 suites=$5 excluded=${6:-}
    if valid_harness_port_span "$value" "$slots" "$excluded"; then
        record "behavior.$id" behavior ok true "$suites" \
            "$name=$value leaves at least $slots valid allocator slots" "" 5 0
    else
        record "behavior.$id" behavior degraded true "$suites" \
            "$name=$value does not leave $slots valid allocator slots" \
            "choose a valid base whose allocator range has $slots slots outside the pinned port" 5 0
    fi
}

wants_wasm() {
    local packages=$1
    [ -n "$packages" ] || return 1
    [ "$packages" = ALL ] && return 0
    just rs _wants-wasm "$packages" >/dev/null 2>&1
}

# Print the selected suites that actually invoke Cargo. Check reaches Cargo
# through more languages than test; test dispatches only Rust and Python.
cargo_suites() {
    local changed=$1 packages=$2 out="" suite
    if tools_for_files "$changed" | grep -qx cargo; then
        out=$(selected check)
    fi
    if [ "$changed" = ALL ] || [ -n "$packages" ] ||
        printf '%s\n' "$changed" | grep -qE '^(py/|pyproject\.toml$|uv\.lock$|rs/moq-ffi/)'; then
        out="$out $(selected test)"
    fi
    for suite in smoke smoke-full wasm; do
        out="$out $(selected "$suite")"
    done
    out=$(printf '%s' "$out" | tr ' ' '\n' | grep -v '^$' | sort -u | tr '\n' ' ')
    printf '%s' "${out% }"
}

# Print check when the changed-file predicate for the real `_flake` recipe
# matches. Nix on PATH is required more broadly, but store access is not.
nix_suites() {
    local changed=$1
    [ -n "$(selected check)" ] || return
    if [ "$changed" = ALL ] ||
        printf '%s\n' "$changed" | grep -qE '(^rs/|^Cargo\.(toml|lock)$|^flake\.lock$|\.nix$)'; then
        printf 'check\n'
    fi
}

# Print the selected suites that actually bind a loopback socket, for a
# changed-file scope and its Rust package selection. The cross-language
# harnesses always bind. Scoped Rust and Python tests bind, while JS unit tests
# use mock transport pairs and bind nothing.
bind_suites() {
    local changed=$1 packages=$2 protocol=$3 out="" suite
    if rust_tests_bind "$packages" "$protocol" ||
        { [ "$protocol" = udp ] && printf '%s\n' "$changed" | grep -qE '^(py/|pyproject\.toml$|uv\.lock$)'; }; then
        out=$(selected test)
    fi
    for suite in $SUITES; do
        case $suite in smoke | smoke-full | wasm) out="$out $suite" ;; esac
    done
    out=$(printf '%s' "$out" | tr ' ' '\n' | grep -v '^$' | sort -u | tr '\n' ' ')
    printf '%s' "${out% }"
}

# ---------------------------------------------------------------------------
# Result registry
# ---------------------------------------------------------------------------

R_ID=()
R_SECTION=()
R_STATUS=()
R_REQUIRED=()
R_SUITES=()
R_DETAIL=()
R_REMEDY=()
R_BUDGET=()
R_ELAPSED=()

# Record one probe result: id section status required suites detail remedy budget elapsed
record() {
    R_ID[${#R_ID[@]}]=$1
    R_SECTION[${#R_SECTION[@]}]=$2
    R_STATUS[${#R_STATUS[@]}]=$3
    R_REQUIRED[${#R_REQUIRED[@]}]=$4
    R_SUITES[${#R_SUITES[@]}]=$5
    R_DETAIL[${#R_DETAIL[@]}]=$6
    R_REMEDY[${#R_REMEDY[@]}]=$7
    R_BUDGET[${#R_BUDGET[@]}]=$8
    R_ELAPSED[${#R_ELAPSED[@]}]=$9
}

# ---------------------------------------------------------------------------
# Bounded execution
#
# `timeout` is coreutils, which is one of the things that may be absent, so the
# budget is enforced here instead. The counter doubles as the elapsed clock:
# `date +%s%N` is a GNU extension that macOS does not have.
# ---------------------------------------------------------------------------

BOUNDED_OUT=""
BOUNDED_ELAPSED=0

# Run a command with a budget in seconds. Returns its status, or 124 on timeout.
#
# Monitor mode puts the child in its own process group, so the timeout can
# signal the whole tree. Killing the immediate PID alone leaves the descendants
# that actually cost something -- a cargo build, a Chromium, a `sh -c` wrapper's
# real work -- running past the budget and writing into caches after the report
# is out. TERM then KILL, because a process that ignores TERM would otherwise
# hold the `wait` past the same budget.
bounded() {
    local budget=$1
    shift
    local pid ticks=0 limit=$((budget * 10)) status

    BOUNDED_OUT=""
    set -m
    "$@" >"$BOUNDED_TMP" 2>&1 &
    pid=$!
    set +m

    while ((ticks < limit)); do
        kill -0 "$pid" 2>/dev/null || break
        sleep 0.1
        ticks=$((ticks + 1))
    done

    if kill -0 "$pid" 2>/dev/null; then
        kill -TERM -"$pid" 2>/dev/null || kill -TERM "$pid" 2>/dev/null
        # A short grace period for a clean exit, then the signal that cannot be
        # ignored. Bounded itself: the budget is a budget.
        ticks=0
        while ((ticks < 20)) && kill -0 "$pid" 2>/dev/null; do
            sleep 0.1
            ticks=$((ticks + 1))
        done
        kill -KILL -"$pid" 2>/dev/null || kill -KILL "$pid" 2>/dev/null
        wait "$pid" 2>/dev/null
        BOUNDED_ELAPSED=$budget
        BOUNDED_OUT=$(cat "$BOUNDED_TMP" 2>/dev/null)
        return 124
    fi

    wait "$pid"
    status=$?
    BOUNDED_ELAPSED=$(printf '%d.%d' $((ticks / 10)) $((ticks % 10)))
    BOUNDED_OUT=$(cat "$BOUNDED_TMP" 2>/dev/null)
    return $status
}

# Print `alive` or `gone` for a pid. A zombie counts as gone: it consumes
# nothing and writes nothing, it just holds a pid until someone reaps it, and a
# killed descendant reparented to a container's PID 1 that does not reap stays
# that way forever. `kill -0` succeeds on a zombie, so it cannot answer this
# question on its own.
process_state() {
    kill -0 "$1" 2>/dev/null || {
        printf 'gone\n'
        return
    }
    local state
    state=$(ps -o state= -p "$1" 2>/dev/null | tr -d '[:space:]') || {
        # Some sandboxes allow signaling a process but deny process listing.
        # In that case kill -0 is the strongest answer available.
        kill -0 "$1" 2>/dev/null && printf 'alive\n' || printf 'gone\n'
        return
    }
    case $state in
        Z*) printf 'gone\n' ;;
        *) printf 'alive\n' ;;
    esac
}

# Map a failed command's exit status and output onto a status. A refusal reads
# as `denied` so the remedy can name a permission instead of an install.
classify() {
    local status=$1 output=$2
    if ((status == 124)); then
        printf 'timeout\n'
    elif printf '%s' "$output" | grep -qiE 'permission denied|operation not permitted|not permitted|access denied|EACCES|EPERM|read-only file system|sandbox|refusing to|is not allowed'; then
        printf 'denied\n'
    else
        printf 'degraded\n'
    fi
}

# Print the first line of output, trimmed, for a one-line detail field.
first_line() {
    printf '%s' "$1" | tr -d '\r' | sed -n '1s/^[[:space:]]*//p' | cut -c1-160
}

# Print the line of a failure worth quoting. Runtimes that echo the offending
# source before the diagnosis (bun, cargo) would otherwise report the fixture
# back at the reader instead of what went wrong.
error_line() {
    local body line
    # Drop the source echo first. bun prints the offending line as `123 | ...`
    # with a caret under it, and those lines contain the word "error" far more
    # often than the diagnosis does.
    body=$(printf '%s' "$1" | tr -d '\r' | grep -vE '^[[:space:]]*([0-9]+ \||\^|[│║╔╚╗╝═─]|at )')
    line=$(printf '%s' "$body" | grep -m1 -iE 'error|denied|refused|not permitted|failed|no such|cannot|does not exist|doesn.t exist')
    [ -n "$line" ] || line=$(printf '%s' "$body" | grep -m1 '[^[:space:]]')
    first_line "${line:-$1}"
}

# ---------------------------------------------------------------------------
# JSON
#
# Hand-rolled because jq and bun are two of the tools this may have to report
# missing, and a diagnostic that needs the thing it diagnoses is no diagnostic.
# ---------------------------------------------------------------------------

json_string() {
    local s=$1
    s=${s//\\/\\\\}
    s=${s//\"/\\\"}
    s=${s//$'\n'/\\n}
    s=${s//$'\r'/\\r}
    s=${s//$'\t'/\\t}
    # Remaining C0 controls have no short escape and would make the document
    # invalid, so drop them rather than emit them raw.
    printf '"%s"' "$(printf '%s' "$s" | tr -d '\000-\010\013\014\016-\037')"
}

# Print a whitespace-separated list as a JSON array of strings.
json_array() {
    local item first=1
    printf '['
    for item in $1; do
        ((first)) || printf ','
        first=0
        json_string "$item"
    done
    printf ']'
}

# ---------------------------------------------------------------------------
# Probes
# ---------------------------------------------------------------------------

# How long a tool gets to print its version. A JVM or a Dart VM has to start
# first, and reporting a healthy toolchain as hung is worse than waiting.
tool_budget() {
    case $1 in
        dart | go | gradle | java | uv) printf '30\n' ;;
        *) printf '5\n' ;;
    esac
}

# Report one required tool: its path, its version, or why it is unusable.
probe_tool() {
    local tool=$1 suites=$2 path budget
    budget=$(tool_budget "$tool")
    path=$(command -v "$tool" 2>/dev/null)
    if [ -z "$path" ]; then
        record "tool.$tool" tools missing true "$suites" "not on PATH" \
            "install $tool, or enter the dev shell: nix develop" "$budget" 0
        return
    fi
    if [ ! -x "$path" ]; then
        record "tool.$tool" tools denied true "$suites" "$path is not executable" \
            "grant execute permission on $path" "$budget" 0
        return
    fi

    local status version
    bounded "$budget" "$path" --version
    status=$?
    version=$(first_line "$BOUNDED_OUT")
    if ((status != 0)); then
        # Not every tool takes --version, and one that does not is still
        # installed, so only a refusal or a hang is worth reporting here.
        local kind
        kind=$(classify "$status" "$BOUNDED_OUT")
        if [ "$kind" = degraded ]; then
            record "tool.$tool" tools ok true "$suites" "$path (version unknown)" "" "$budget" "$BOUNDED_ELAPSED"
        else
            record "tool.$tool" tools "$kind" true "$suites" "$path: $(error_line "$BOUNDED_OUT")" \
                "allow executing $path" "$budget" "$BOUNDED_ELAPSED"
        fi
        return
    fi
    record "tool.$tool" tools ok true "$suites" "$path (${version:-unknown})" "" "$budget" "$BOUNDED_ELAPSED"
}

# Bun parses `.github/workflows/*.yml` for alert.sh's coverage check. Bun before
# 1.3 folds the YAML 1.1 key `on` to the boolean `true`, so the trigger it needs
# to read is not there under the name it looks for, and every workflow reads as
# unwatched. The executable name and a version string do not separate the two.
probe_bun_yaml() {
    local suites="check"
    if ! command -v bun >/dev/null 2>&1; then
        record behavior.bun-yaml behavior skip false "$suites" "bun is not installed" "" 10 0
        return
    fi
    local status keys
    bounded 10 bun -e 'const d = Bun.YAML.parse("on: push\nname: x\n"); console.log(Object.keys(d).join(","))'
    status=$?
    keys=$(first_line "$BOUNDED_OUT")
    if ((status != 0)); then
        record behavior.bun-yaml behavior "$(classify "$status" "$BOUNDED_OUT")" true "$suites" \
            "bun could not parse a YAML fixture: $(error_line "$BOUNDED_OUT")" \
            "use the dev shell's bun: nix develop" 10 "$BOUNDED_ELAPSED"
        return
    fi
    case ",$keys," in
        *,on,*)
            record behavior.bun-yaml behavior ok true "$suites" "bun keeps the workflow key 'on'" "" 10 "$BOUNDED_ELAPSED"
            ;;
        *)
            record behavior.bun-yaml behavior degraded true "$suites" \
                "bun folds the workflow key 'on' to a boolean (parsed keys: $keys), so 'alert.sh check-coverage' cannot read a workflow trigger" \
                "use the dev shell's bun (1.3+): nix develop" 10 "$BOUNDED_ELAPSED"
            ;;
    esac
}

# `just rs check-changed` selects packages with an awk program whose seed list
# arrives through `-v`, and a seed list is one crate per line. BSD awk rejects a
# literal newline in a -v assignment ("newline in string"), so the selector dies
# on any diff touching two crates while a one-crate diff sails through.
probe_awk_select() {
    local suites=$1
    if ! command -v awk >/dev/null 2>&1; then
        record behavior.awk-select behavior missing true "$suites" "awk is not on PATH" \
            "install awk, or enter the dev shell: nix develop" 10 0
        return
    fi
    local status out
    cat >"$SCRATCH/select.awk" <<'EOF'
BEGIN { split(seeds, s, "\n"); for (i in s) want[s[i]] = 1 }
END { for (n in want) if (want[n]) print n }
EOF
    bounded 10 awk -v seeds=$'moq-net\nmoq-lite' -f "$SCRATCH/select.awk" /dev/null
    status=$?
    out=$(printf '%s' "$BOUNDED_OUT" | sort | tr '\n' ' ')
    out=${out% }
    if ((status != 0)); then
        record behavior.awk-select behavior degraded true "$suites" \
            "awk cannot run the Rust package selector with a multi-crate diff: $(error_line "$BOUNDED_OUT")" \
            "use the dev shell's awk (gawk): nix develop" 10 "$BOUNDED_ELAPSED"
        return
    fi
    if [ "$out" != "moq-lite moq-net" ]; then
        record behavior.awk-select behavior degraded true "$suites" \
            "awk selected '$out' instead of 'moq-lite moq-net'" \
            "use the dev shell's awk (gawk): nix develop" 10 "$BOUNDED_ELAPSED"
        return
    fi
    record behavior.awk-select behavior ok true "$suites" "awk selects a multi-crate diff" "" 10 "$BOUNDED_ELAPSED"
}

# Resolve a command from the directory that will invoke it.
command_path() {
    (cd "$1" && command -v "$2" 2>/dev/null)
}

# Report the configured C compiler without putting a dynamic path in the
# whitespace-delimited tool map.
probe_c_compiler() {
    local suites=$1 cc=${CC:-cc} path display status kind version
    path=$(command_path "$REPO/test" "$cc")
    if [ -z "$path" ]; then
        record behavior.c-compiler behavior missing true "$suites" \
            "configured C compiler is missing: $cc" \
            "install the configured C compiler, or enter the dev shell: nix develop" 10 0
        return
    fi
    case $path in /*) display=$path ;; *) display="$REPO/test/$path" ;; esac
    bounded 10 bash -c 'cd "$1" && exec "$2" --version' _ "$REPO/test" "$cc"
    status=$?
    version=$(first_line "$BOUNDED_OUT")
    kind=$(classify "$status" "$BOUNDED_OUT")
    if ((status != 0)) && [ "$kind" != degraded ]; then
        record behavior.c-compiler behavior "$kind" true "$suites" \
            "$display: $(error_line "$BOUNDED_OUT")" "allow executing $display" 10 "$BOUNDED_ELAPSED"
        return
    fi
    record behavior.c-compiler behavior ok true "$suites" \
        "$display${version:+ ($version)}" "" 10 "$BOUNDED_ELAPSED"
}

# Ask Cargo for the HOST and TARGET it gives build scripts. This incorporates
# build.target from every Cargo configuration layer without reimplementing its
# merge rules. The marker is emitted before the probe crate itself is checked,
# so an uninstalled cross target still tells us which pkg-config variables the
# real build will select.
cargo_targets() {
    local cargo=$1 cargo_home=${2:-} cargo_dir=${3:-$REPO} crate="$SCRATCH/probe-crate-cargo-target" marker previous=$PWD
    local command=(env "CARGO_TARGET_DIR=$crate/target" "MOQ_DOCTOR_TARGET_DIR=$crate/target")
    [ -n "$cargo_home" ] && command+=("CARGO_HOME=$cargo_home")
    command+=("$cargo" check --offline --color never --manifest-path "$crate/Cargo.toml")

    mkdir -p "$crate/src"
    cat >"$crate/Cargo.toml" <<'EOF'
[package]
name = "moq-doctor-target"
version = "0.0.0"
edition = "2021"
build = "build.rs"

[workspace]
EOF
    cat >"$crate/build.rs" <<'EOF'
fn main() {
    let host = std::env::var("HOST").unwrap();
    let target = std::env::var("TARGET").unwrap();
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let root = std::path::PathBuf::from(std::env::var("MOQ_DOCTOR_TARGET_DIR").unwrap());
    let relative = out.strip_prefix(root).unwrap();
    let explicit = relative.components().next().unwrap().as_os_str() == target.as_str();
    println!("cargo:warning=moq-doctor-target:{host}:{target}:{explicit}");
}
EOF
    printf 'fn main() {}\n' >"$crate/src/main.rs"

    cd "$cargo_dir" || return 1
    bounded 30 "${command[@]}"
    cd "$previous" || return 1
    marker=$(printf '%s\n' "$BOUNDED_OUT" | sed -n 's/.*moq-doctor-target:\([^:]*\):\([^:]*\):\([^:]*\)$/\1 \2 \3/p' | tail -1)
    CARGO_PROBE_HOST=${marker%% *}
    marker=${marker#* }
    CARGO_PROBE_TARGET=${marker%% *}
    CARGO_PROBE_EXPLICIT_TARGET=${marker##* }
    [ -n "$CARGO_PROBE_HOST" ] && [ -n "$CARGO_PROBE_TARGET" ] &&
        { [ "$CARGO_PROBE_EXPLICIT_TARGET" = true ] || [ "$CARGO_PROBE_EXPLICIT_TARGET" = false ]; }
}

# The harnesses read artifacts from target/{debug,release}; any explicit Cargo
# target writes them below target/<triple>/ instead, even for HOST.
probe_harness_cargo_target() {
    local id=$1 suites=$2 cargo=$3 status relay_override=0
    [ "$id" != smoke ] || SMOKE_CARGO_TARGET_READY=0
    if [ "${CARGO_TARGET_DIR+x}" = x ] && [ -z "$CARGO_TARGET_DIR" ]; then
        record "behavior.$id-artifact-layout" behavior degraded true "$suites" \
            "CARGO_TARGET_DIR is explicitly empty, which Cargo refuses" \
            "unset CARGO_TARGET_DIR or set it to a non-empty path" 30 0
        return
    fi
    if [ "$id" = wasm ] && [ -n "${RELAY_BIN:-}" ]; then
        case $RELAY_BIN in
            /*)
                if [ ! -x "$RELAY_BIN" ] || [ -d "$RELAY_BIN" ]; then
                    record "behavior.$id-artifact-layout" behavior missing true "$suites" \
                        "RELAY_BIN is not an executable file: $RELAY_BIN" \
                        "set RELAY_BIN to an executable absolute path" 30 0
                    return
                fi
                relay_override=1
                ;;
            *)
                record "behavior.$id-artifact-layout" behavior degraded true "$suites" \
                    "RELAY_BIN changes meaning after wasm changes directories: $RELAY_BIN" \
                    "set RELAY_BIN to an executable absolute path" 30 0
                return
                ;;
        esac
    fi
    if [ "$id" = wasm ] && [ "$relay_override" = 0 ] && [ -n "${CARGO_TARGET_DIR:-}" ] && [ "${CARGO_TARGET_DIR#/}" = "$CARGO_TARGET_DIR" ]; then
        record "behavior.$id-artifact-layout" behavior degraded true "$suites" \
            "CARGO_TARGET_DIR is relative, but wasm resolves the relay after changing directories: $CARGO_TARGET_DIR" \
            "use an absolute CARGO_TARGET_DIR for wasm" 30 0
        return
    fi
    if ! command -v "$cargo" >/dev/null 2>&1; then
        record "behavior.$id-artifact-layout" behavior missing true "$suites" \
            "$cargo is missing, so its artifact layout cannot be resolved" \
            "install the Rust toolchain, or enter the dev shell: nix develop" 30 0
        return
    fi
    cargo_targets "$cargo"
    status=$?
    if ((status != 0)); then
        record "behavior.$id-artifact-layout" behavior "$(classify "$status" "$BOUNDED_OUT")" true "$suites" \
            "cargo could not resolve its artifact layout: $(error_line "$BOUNDED_OUT")" \
            "use the dev shell's Rust toolchain and a valid Cargo configuration: nix develop" 30 "$BOUNDED_ELAPSED"
        return
    fi
    if [ "$CARGO_PROBE_EXPLICIT_TARGET" = true ] && [ "$relay_override" = 0 ]; then
        record "behavior.$id-artifact-layout" behavior degraded true "$suites" \
            "Cargo writes artifacts below target/$CARGO_PROBE_TARGET, but the harnesses read target directly" \
            "unset Cargo build.target for smoke and wasm, including CARGO_BUILD_TARGET and .cargo configuration" 30 "$BOUNDED_ELAPSED"
        return
    fi
    if [ "$id" = smoke ]; then
        SMOKE_CARGO_TARGET_READY=1
        SMOKE_CARGO_HOST=$CARGO_PROBE_HOST
        SMOKE_CARGO_TARGET=$CARGO_PROBE_TARGET
    fi
    record "behavior.$id-artifact-layout" behavior ok true "$suites" \
        "Cargo uses the host artifact layout for $CARGO_PROBE_TARGET" "" 30 "$BOUNDED_ELAPSED"
}

# Print the same target-qualified environment value pkg-config-rs selects.
targeted_env() {
    local base=$1 target=$2 host=$3 kind name value
    if [ "$host" = "$target" ]; then
        kind=HOST
    else
        kind=TARGET
    fi
    for name in "${base}_${target}" "${base}_${target//-/_}" "${kind}_${base}" "$base"; do
        if value=$(printenv "$name" 2>/dev/null); then
            printf '%s\n' "$value"
            return
        fi
    done
    return 1
}

# Print Cargo's configured executable, whose fallback is the literal
# `pkg-config`; pkg-config-rs does not substitute a distinct `pkgconf` binary.
pkg_config_executable() {
    targeted_env PKG_CONFIG "$1" "$2" || printf 'pkg-config\n'
}

# True when resolving a command depends on the caller's working directory.
relative_command() {
    case $1 in
        /*) return 1 ;;
        */*) return 0 ;;
    esac
    local entry path=${PATH-} more
    while :; do
        case $path in
            *:*)
                entry=${path%%:*}
                path=${path#*:}
                more=1
                ;;
            *)
                entry=$path
                more=0
                ;;
        esac
        [ -n "$entry" ] || return 0
        case $entry in
            /*)
                if [ -x "$entry/$1" ] && [ ! -d "$entry/$1" ]; then
                    return 1
                fi
                ;;
            *) return 0 ;;
        esac
        [ "$more" = 1 ] || break
    done
    return 1
}

# True when a pkg-config search path changes meaning with the working directory.
relative_search_path() {
    local value=$1 entry old_ifs=$IFS
    [ -n "$value" ] || return 1
    case $value in :* | *: | *::*) return 0 ;; esac
    IFS=:
    for entry in $value; do
        case $entry in /*) ;; *)
            IFS=$old_ifs
            return 0
            ;;
        esac
    done
    IFS=$old_ifs
    return 1
}

# Verify the native metadata the requested GStreamer smoke client links against.
probe_gstreamer_devel() {
    local suites=$1 status host target pkg_config source display base value
    local pkg_env=(env)
    if [ "${SMOKE_CARGO_TARGET_READY:-0}" != 1 ]; then
        record behavior.gstreamer-devel behavior skip false "$suites" \
            "Cargo's smoke artifact layout is unavailable" "" 10 0
        return
    fi
    host=$SMOKE_CARGO_HOST
    target=$SMOKE_CARGO_TARGET
    if pkg_config=$(targeted_env PKG_CONFIG "$target" "$host"); then
        source="Cargo's target-qualified pkg-config override"
    else
        pkg_config=$(pkg_config_executable "$target" "$host")
        source="Cargo's default pkg-config executable"
    fi
    display=${pkg_config:-'<empty pkg-config override>'}
    if relative_command "$pkg_config"; then
        record behavior.gstreamer-devel behavior degraded true "$suites" \
            "$source depends on Cargo's build-script working directory: $display" \
            "use an absolute pkg-config override and absolute PATH entries" 10 0
        return
    fi
    if ! command -v "$pkg_config" >/dev/null 2>&1; then
        record behavior.gstreamer-devel behavior missing true "$suites" \
            "$source is missing: $display" \
            "install the configured pkg-config executable, or enter the dev shell: nix develop" 10 0
        return
    fi
    for base in PKG_CONFIG_PATH PKG_CONFIG_LIBDIR PKG_CONFIG_SYSROOT_DIR; do
        if value=$(targeted_env "$base" "$target" "$host"); then
            if { [ "$base" = PKG_CONFIG_SYSROOT_DIR ] && [ -n "$value" ] && [ "${value#/}" = "$value" ]; } ||
                { [ "$base" != PKG_CONFIG_SYSROOT_DIR ] && relative_search_path "$value"; }; then
                record behavior.gstreamer-devel behavior degraded true "$suites" \
                    "$base depends on Cargo's build-script working directory: $value" \
                    "use absolute paths in pkg-config metadata configuration" 10 0
                return
            fi
            pkg_env+=("$base=$value")
        fi
    done
    bounded 10 "${pkg_env[@]}" "$pkg_config" --atleast-version=1.14 gstreamer-1.0
    status=$?
    if ((status == 0)); then
        record behavior.gstreamer-devel behavior ok true "$suites" \
            "$display resolves GStreamer 1.14+ development metadata" "" 10 "$BOUNDED_ELAPSED"
    else
        record behavior.gstreamer-devel behavior "$(classify "$status" "$BOUNDED_OUT")" true "$suites" \
            "$display cannot resolve GStreamer 1.14+ development metadata" \
            "install GStreamer 1.14+ development metadata that provides gstreamer-1.0.pc" 10 "$BOUNDED_ELAPSED"
    fi
}

# Print the nearest existing directory at or above a requested path. Cargo
# creates nested cache and target paths recursively, so their immediate parent
# need not exist yet.
existing_ancestor() {
    local path=$1 parent
    while [ ! -e "$path" ]; do
        [ -L "$path" ] && return 1
        parent=$(dirname "$path")
        [ "$parent" != "$path" ] || return 1
        path=$parent
    done
    [ -d "$path" ] || return 1
    printf '%s\n' "$path"
}

# A directory that exists is not a directory this session may write to.
probe_writable() {
    local id=$1 requested=$2 suites=$3 probe note="" dir
    if ! dir=$(existing_ancestor "$requested"); then
        record "storage.$id" storage missing true "$suites" \
            "$requested has no existing directory ancestor" "create a directory ancestor for $requested" 5 0
        return
    fi
    if [ "$dir" != "$requested" ]; then
        note=", the nearest existing ancestor of $requested"
    fi
    probe="$dir/.moq-doctor.$$"
    if : >"$probe" 2>/dev/null; then
        rm -f "$probe"
        record "storage.$id" storage ok true "$suites" "$dir is writable$note" "" 5 0
    else
        record "storage.$id" storage denied true "$suites" "$dir is not writable$note" \
            "grant write access to $dir" 5 0
    fi
}

# Compiling the workspace needs room. Reported against the target directory,
# which is where all of it lands.
probe_disk() {
    local dir=$1 suites=$2 free_kb free_gib
    if ! dir=$(existing_ancestor "$dir"); then
        record storage.disk storage degraded false "$suites" "cannot locate a filesystem for the target path" "" 5 0
        return
    fi
    free_kb=$(df -Pk "$dir" 2>/dev/null | awk 'NR == 2 { print $4 }')
    if [ -z "$free_kb" ]; then
        record storage.disk storage degraded false "$suites" "cannot read free space on $dir" "" 5 0
        return
    fi
    free_gib=$((free_kb / 1024 / 1024))
    if ((free_gib < DISK_FLOOR_GIB)); then
        record storage.disk storage degraded true "$suites" \
            "${free_gib} GiB free on $dir, below the ${DISK_FLOOR_GIB} GiB a workspace build needs" \
            "free space on $dir" 5 0
    else
        record storage.disk storage ok true "$suites" "${free_gib} GiB free on $dir" "" 5 0
    fi
}

# Nix on PATH proves nothing about the daemon socket or the store, which is
# what a sandboxed session actually loses.
probe_nix() {
    local suites="check"
    if ! command -v nix >/dev/null 2>&1; then
        # Not required here: `tool.nix` already reports the absence, and
        # reporting it twice would double-count one problem. An installed nix
        # that cannot reach its store is the case only this probe catches, and
        # that one does block `check`.
        record probe.nix probe missing false "$suites" "nix is not installed" \
            "install nix, or accept that 'just check' skips 'nix flake check'" 30 0
        return
    fi
    local status remedy
    remedy="allow reading ${NIX_STORE:-/nix/store} and connecting to /nix/var/nix/daemon-socket/socket"

    # The store first, because evaluating a literal never contacts it: with an
    # unreachable daemon `nix eval --expr` still prints its answer, and `nix
    # flake check` then dies on the first line with "cannot connect to socket".
    bounded 30 nix store info
    status=$?
    if ((status != 0)); then
        record probe.nix probe "$(classify "$status" "$BOUNDED_OUT")" true "$suites" \
            "nix cannot reach its store: $(error_line "$BOUNDED_OUT")" "$remedy" 30 "$BOUNDED_ELAPSED"
        return
    fi

    bounded 30 nix eval --impure --raw --expr '"ok"'
    status=$?
    if ((status == 0)) && [ "$BOUNDED_OUT" = ok ]; then
        record probe.nix probe ok true "$suites" "nix evaluates and reaches its store" "" 30 "$BOUNDED_ELAPSED"
        return
    fi
    record probe.nix probe "$(classify "$status" "$BOUNDED_OUT")" true "$suites" \
        "nix eval failed: $(error_line "$BOUNDED_OUT")" "$remedy" 30 "$BOUNDED_ELAPSED"
}

# The wrapper, the linker, and a writable target directory, exercised end to end
# on a crate with no dependencies. Nothing else finds a wrapper that cannot
# write its cache before a workspace compile has already been paid for.
#
# TARGET is a rustc target triple, or empty for the host. The host compiling is
# no evidence at all for the wasm suite: rust-toolchain.toml does not install
# wasm32-unknown-unknown, so a rustup box passes this and then fails on the
# first line of `just wasm`.
probe_cargo_compile() {
    local id=$1 suites=$2 target=$3 cargo
    cargo=${4:-${RUST_CARGO:-cargo}}
    if [ "${CARGO_TARGET_DIR+x}" = x ] && [ -z "$CARGO_TARGET_DIR" ]; then
        record "probe.$id" probe degraded true "$suites" \
            "CARGO_TARGET_DIR is explicitly empty, which Cargo refuses" \
            "unset CARGO_TARGET_DIR or set it to a non-empty path" 120 0
        return
    fi
    if ! command -v "$cargo" >/dev/null 2>&1; then
        record "probe.$id" probe missing true "$suites" "$cargo is not on PATH" \
            "install the Rust toolchain, or enter the dev shell: nix develop" 120 0
        return
    fi

    local crate="$SCRATCH/probe-crate-$id"
    mkdir -p "$crate/src"
    cat >"$crate/Cargo.toml" <<'EOF'
[package]
name = "moq-doctor-probe"
version = "0.0.0"
edition = "2021"

[workspace]
EOF
    printf 'fn main() {}\n' >"$crate/src/main.rs"

    local status label="for the host" remedy cargo_cache
    if cargo_cache=$(cargo_home); then
        remedy="allow writing $cargo_cache and ${TMPDIR:-/tmp}, and executing the linker"
    else
        remedy="set HOME or CARGO_HOME to a writable directory, allow writing ${TMPDIR:-/tmp}, and execute the linker"
    fi
    if [ -n "$target" ]; then
        label="for $target"
        remedy="install the target: rustup target add $target, or enter the dev shell: nix develop"
        bounded 120 env CARGO_TARGET_DIR="$crate/target" "$cargo" build --offline --quiet \
            --target "$target" --manifest-path "$crate/Cargo.toml"
    else
        bounded 120 env CARGO_TARGET_DIR="$crate/target" "$cargo" build --offline --quiet \
            --manifest-path "$crate/Cargo.toml"
    fi
    status=$?
    if ((status == 0)); then
        record "probe.$id" probe ok true "$suites" "$cargo compiles a trivial crate $label" "" 120 "$BOUNDED_ELAPSED"
        return
    fi
    record "probe.$id" probe "$(classify "$status" "$BOUNDED_OUT")" true "$suites" \
        "$cargo could not compile a trivial crate $label: $(error_line "$BOUNDED_OUT")" \
        "$remedy" 120 "$BOUNDED_ELAPSED"
}

# `cargo` on PATH is not the subcommands a suite invokes. `just rs check` runs
# clippy, fmt, shear, and sort; `just rs test` runs nextest. A rustup install
# without the clippy component, or without the two cargo extensions the dev
# shell pins, compiles a crate fine and then dies partway through the suite this
# command just called ready.
probe_cargo_subcommand() {
    local sub=$1 suites=$2 remedy=$3 cargo
    # Probed the way rs/justfile calls it: clippy and nextest go through the
    # wrapper, fmt, shear, and sort are plain `cargo`. A wrapper that proxies
    # only the compiling subcommands would otherwise be reported broken for the
    # ones that never reach it.
    case $sub in
        clippy | nextest) cargo=${RUST_CARGO:-cargo} ;;
        *) cargo=cargo ;;
    esac
    if ! command -v "$cargo" >/dev/null 2>&1; then
        record "probe.cargo-$sub" probe missing true "$suites" "$cargo is not on PATH" \
            "install the Rust toolchain, or enter the dev shell: nix develop" 30 0
        return
    fi
    bounded 30 "$cargo" "$sub" --version
    local status=$?
    if ((status == 0)); then
        record "probe.cargo-$sub" probe ok true "$suites" "$(first_line "$BOUNDED_OUT")" "" 30 "$BOUNDED_ELAPSED"
        return
    fi
    record "probe.cargo-$sub" probe "$(classify "$status" "$BOUNDED_OUT")" true "$suites" \
        "cargo $sub is unavailable: $(error_line "$BOUNDED_OUT")" "$remedy" 30 "$BOUNDED_ELAPSED"
}

# Every relay, gateway, and harness test stands up a loopback endpoint, and a
# sandbox that forbids bind fails all of them identically and late.
probe_loopback() {
    local kind=$1 suites=$2
    if ! command -v bun >/dev/null 2>&1; then
        # `missing`, not an optional skip. Plain `smoke` still runs without bun,
        # so clearing the bind here would let a strict run pass on a sandbox
        # that forbids one, having never asked.
        record "probe.loopback-$kind" probe missing true "$suites" \
            "bun is not installed, so the bind could not be probed" \
            "install bun so the bind can be probed, or enter the dev shell: nix develop" 15 0
        return
    fi
    local status script
    case $kind in
        tcp) script='const s = Bun.listen({ hostname: "127.0.0.1", port: 0, socket: { data() {} } }); s.stop(true); console.log("ok");' ;;
        udp) script='const s = await Bun.udpSocket({ hostname: "127.0.0.1", port: 0 }); s.close(); console.log("ok");' ;;
        *) return ;;
    esac
    bounded 15 bun -e "$script"
    status=$?
    if ((status == 0)) && [ "$(first_line "$BOUNDED_OUT")" = ok ]; then
        record "probe.loopback-$kind" probe ok true "$suites" "bind on 127.0.0.1:0 ($kind) succeeds" "" 15 "$BOUNDED_ELAPSED"
        return
    fi
    record "probe.loopback-$kind" probe "$(classify "$status" "$BOUNDED_OUT")" true "$suites" \
        "cannot bind 127.0.0.1:0 ($kind): $(error_line "$BOUNDED_OUT")" \
        "allow binding and connecting to 127.0.0.1 over $kind" 15 "$BOUNDED_ELAPSED"
}

# Concurrent harnesses serialize port reservations with whichever advisory
# locking utility the host provides. Linux normally has flock and macOS lockf;
# requiring one by name would reject the other valid platform.
probe_advisory_lock() {
    local suites=$1 requested=$2 tool status root lock
    if ! root=$(existing_ancestor "$requested"); then
        record probe.advisory-lock probe missing true "$suites" \
            "$requested has no usable directory ancestor" "configure MOQ_TEST_PORTS under a writable directory" 5 0
        return
    fi
    lock="$root/.moq-doctor-harness-lock.$$"
    if command -v flock >/dev/null 2>&1; then
        tool=flock
        bounded 5 flock "$lock" true
    elif command -v lockf >/dev/null 2>&1; then
        tool=lockf
        bounded 5 lockf -k "$lock" true
    else
        record probe.advisory-lock probe missing true "$suites" \
            "neither flock nor lockf is on PATH" "install flock or lockf" 5 0
        return
    fi
    status=$?
    rm -f "$lock" 2>/dev/null || true
    if ((status == 0)); then
        record probe.advisory-lock probe ok true "$suites" \
            "$tool acquires an advisory lock in $root, the existing ancestor of $requested" "" 5 "$BOUNDED_ELAPSED"
    else
        record probe.advisory-lock probe "$(classify "$status" "$BOUNDED_OUT")" true "$suites" \
            "$tool could not acquire an advisory lock: $(error_line "$BOUNDED_OUT")" \
            "allow $tool to create and lock files in $root" 5 "$BOUNDED_ELAPSED"
    fi
}

# `just test smoke` and `just test wasm` drive headless Chromium through the
# Playwright each pins in its own package. Probed by launching it, because an
# installed package with no downloaded browser is the common shape of this
# failure and it reads as "works here" right up to the launch.
probe_playwright() {
    local suite=$1 dir=$2
    if ! command -v bun >/dev/null 2>&1; then
        record "probe.playwright-$suite" probe missing true "$suite" "bun is not installed" \
            "enter the dev shell: nix develop" 60 0
        return
    fi

    # No pre-check for node_modules: bun hoists workspace dependencies to the
    # repository root, so the package a harness resolves is not necessarily next
    # to it. Launching is the only question that matters anyway.
    #
    # `channel: "chromium"` because that is what both harnesses launch: the
    # default headless shell has no WebTransport, so probing it would report ok
    # on an install the suites cannot use.
    local status
    bounded 60 env PLAYWRIGHT_DIR="$dir" bun -e '
		process.chdir(Bun.env.PLAYWRIGHT_DIR);
		const { chromium } = await import("playwright");
		const browser = await chromium.launch({ channel: "chromium", headless: true });
		await browser.close();
		console.log("ok");
	'
    status=$?
    if ((status == 0)) && [ "$(first_line "$BOUNDED_OUT")" = ok ]; then
        record "probe.playwright-$suite" probe ok true "$suite" "headless Chromium launches for $suite" "" 60 "$BOUNDED_ELAPSED"
        return
    fi
    # The two ways it is absent read very differently and need different
    # remedies: no package at all, or a package whose browser was never
    # downloaded.
    local kind remedy
    if printf '%s' "$BOUNDED_OUT" | grep -qiE "cannot find (module|package)|failed to resolve"; then
        kind=missing
        remedy="run: bun install"
    elif printf '%s' "$BOUNDED_OUT" | grep -qiE "executable doesn't exist|playwright install"; then
        kind=missing
        remedy="run: bunx playwright install chromium"
    else
        kind=$(classify "$status" "$BOUNDED_OUT")
        remedy="allow executing the Playwright browser under ${PLAYWRIGHT_BROWSERS_PATH:-the Playwright browser cache}"
    fi
    record "probe.playwright-$suite" probe "$kind" true "$suite" \
        "headless Chromium did not launch for $suite (${dir#"$REPO"/}): $(error_line "$BOUNDED_OUT")" \
        "$remedy" 60 "$BOUNDED_ELAPSED"
}

# Reading a PR's checks is a different capability from reaching the network: a
# session can have general egress and no credential, or a credential and no
# egress, and the two have different remedies. Never required, because nothing
# in `just check` or `just test` reads GitHub.
probe_github() {
    local suites="" status

    if command -v bun >/dev/null 2>&1; then
        bounded 15 bun -e 'const r = await fetch("https://api.github.com/", { method: "HEAD" }); console.log(r.status);'
        status=$?
        if ((status == 0)); then
            record github.network github ok false "$suites" "api.github.com reachable (HTTP $(first_line "$BOUNDED_OUT"))" "" 15 "$BOUNDED_ELAPSED"
        else
            record github.network github "$(classify "$status" "$BOUNDED_OUT")" false "$suites" \
                "api.github.com unreachable: $(error_line "$BOUNDED_OUT")" \
                "allow outbound HTTPS to api.github.com" 15 "$BOUNDED_ELAPSED"
        fi
    else
        record github.network github skip false "$suites" "bun is not installed, so reachability was not probed" "" 15 0
    fi

    if ! command -v gh >/dev/null 2>&1; then
        record github.read github missing false "$suites" "gh is not installed, so PR checks, logs, and artifacts cannot be read" \
            "install gh and authenticate it" 20 0
        return
    fi

    # This checkout's own repository, so a fork is not told it cannot read
    # somebody else's.
    local slug
    slug=$(git remote get-url origin 2>/dev/null |
        sed -e 's|^git@[^:]*:||' -e 's|^[a-z+]*://[^/]*/||' -e 's|\.git$||')
    [ -n "$slug" ] || slug=moq-dev/moq

    # One authenticated read of its workflow runs: the same endpoint that backs
    # PR checks, run logs, and artifacts. Only the HTTP status is reported, so
    # no credential reaches the output.
    bounded 20 gh api -i -X GET "repos/$slug/actions/runs" -f per_page=1
    status=$?
    local code
    code=$(printf '%s' "$BOUNDED_OUT" | sed -n '1s|^HTTP/[0-9.]* \([0-9]*\).*|\1|p')
    if ((status == 0)) && [ "$code" = 200 ]; then
        record github.read github ok false "$suites" "$slug workflow runs, logs, and artifacts are readable" "" 20 "$BOUNDED_ELAPSED"
        return
    fi
    local kind=denied
    ((status == 124)) && kind=timeout
    record github.read github "$kind" false "$suites" \
        "cannot read repos/$slug/actions/runs${code:+ (HTTP $code)}" \
        "authenticate gh with read access to $slug (gh auth login), and allow outbound HTTPS to api.github.com" \
        20 "$BOUNDED_ELAPSED"
}

# The SessionStart hook exports its own result, so a session that started
# without the dev shell says so here instead of failing a build later.
probe_session() {
    local result=${MOQ_SESSION_SETUP:-} log=${MOQ_SESSION_SETUP_LOG:-}
    if [ -z "$result" ]; then
        record session.setup session skip false "" \
            "no session setup hook reported a result (MOQ_SESSION_SETUP is unset)" "" 5 0
        return
    fi
    local detail="$result"
    # The hook writes `none` when it stopped before it had a log to write to.
    [ -n "$log" ] && [ "$log" != none ] && detail="$result (log: $log)"
    case $result in
        nix-dev-env | direnv)
            record session.setup session ok false "" "$detail" "" 5 0
            ;;
        *)
            record session.setup session degraded false "" "$detail" \
                "read ${log:-the session setup log} for why the dev shell did not load" 5 0
            ;;
    esac
}

# ---------------------------------------------------------------------------
# Output
# ---------------------------------------------------------------------------

# Print the suites in a check's list that this run actually selected. A probe
# declares every suite it serves; only the selected ones are being blocked.
selected() {
    local suite out=""
    for suite in $1; do
        case " $SUITES " in *" $suite "*) out="$out $suite" ;; esac
    done
    printf '%s' "${out# }"
}

emit_human() {
    printf 'moq doctor\n'
    printf '  base      %s (%s changed %s)\n' "$BASE_REF" "$CHANGED_COUNT" "$(
        [ "$CHANGED_COUNT" = 1 ] && printf 'file' || printf 'files'
    )"
    printf '  scope     %s\n' "${SCOPE_LABEL:-none}"
    printf '  suites    %s\n' "$SUITES"
    printf '  shell     %s\n' "$SHELL_ID"
    printf '  cargo     %s (target %s)\n' "${RUST_CARGO:-cargo}${RUSTC_WRAPPER:+, wrapper ${RUSTC_WRAPPER}}" "$TARGET_DIR"

    local section='' i blocks
    for ((i = 0; i < ${#R_ID[@]}; i++)); do
        if [ "${R_SECTION[$i]}" != "$section" ]; then
            section=${R_SECTION[$i]}
            printf '\n%s\n' "$section"
        fi
        printf '  %-8s %-24s %s\n' "${R_STATUS[$i]}" "${R_ID[$i]#*.}" "${R_DETAIL[$i]}"
        [ "${R_STATUS[$i]}" = ok ] && continue
        [ "${R_STATUS[$i]}" = skip ] && continue
        # What it costs, then what to do about it. A capability with no suite
        # behind it blocks nothing and says so, rather than looking like an
        # unexplained failure.
        blocks=$(selected "${R_SUITES[$i]}")
        if [ "${R_REQUIRED[$i]}" = true ] && [ -n "$blocks" ]; then
            printf '           %-24s blocks: %s\n' '' "$(printf '%s' "$blocks" | tr ' ' ',')"
        else
            printf '           %-24s blocks nothing; reported for context\n' ''
        fi
        [ -n "${R_REMEDY[$i]}" ] &&
            printf '           %-24s -> %s\n' '' "${R_REMEDY[$i]}"
    done

    printf '\nsummary\n'
    printf '  ok %s, missing %s, denied %s, timeout %s, degraded %s, skipped %s\n' \
        "$N_OK" "$N_MISSING" "$N_DENIED" "$N_TIMEOUT" "$N_DEGRADED" "$N_SKIP"
    if [ -n "$BLOCKED_SUITES" ]; then
        printf '  blocked suites: %s\n' "$BLOCKED_SUITES"
    else
        printf '  blocked suites: none\n'
    fi
    if ((FAILED > 0)); then
        if ((STRICT)); then
            printf '  strict: FAIL, %s required capabilities are unavailable\n' "$FAILED"
        else
            printf '  %s required capabilities are unavailable; MOQ_STRICT=1 makes this an error\n' "$FAILED"
        fi
    fi
}

emit_json() {
    local i
    printf '{'
    printf '"base":%s,' "$(json_string "$BASE_REF")"
    printf '"changed":%s,' "$CHANGED_COUNT"
    printf '"scope":%s,' "$(json_array "$SCOPE_LABEL")"
    printf '"suites":%s,' "$(json_array "$SUITES")"
    printf '"shell":%s,' "$(json_string "$SHELL_ID")"
    printf '"cargo":%s,' "$(json_string "${RUST_CARGO:-cargo}")"
    printf '"rustc_wrapper":%s,' "$(json_string "${RUSTC_WRAPPER:-}")"
    printf '"target_dir":%s,' "$(json_string "$TARGET_DIR")"
    printf '"strict":%s,' "$( ((STRICT)) && printf 'true' || printf 'false')"
    printf '"checks":['
    for ((i = 0; i < ${#R_ID[@]}; i++)); do
        ((i)) && printf ','
        printf '{"id":%s,"section":%s,"status":%s,"required":%s,"suites":%s,"detail":%s,"remedy":%s,"budget_s":%s,"elapsed_s":%s}' \
            "$(json_string "${R_ID[$i]}")" \
            "$(json_string "${R_SECTION[$i]}")" \
            "$(json_string "${R_STATUS[$i]}")" \
            "${R_REQUIRED[$i]}" \
            "$(json_array "${R_SUITES[$i]}")" \
            "$(json_string "${R_DETAIL[$i]}")" \
            "$(json_string "${R_REMEDY[$i]}")" \
            "${R_BUDGET[$i]}" \
            "${R_ELAPSED[$i]}"
    done
    printf '],'
    printf '"summary":{"ok":%s,"missing":%s,"denied":%s,"timeout":%s,"degraded":%s,"skip":%s,"failed":%s,"blocked_suites":%s}' \
        "$N_OK" "$N_MISSING" "$N_DENIED" "$N_TIMEOUT" "$N_DEGRADED" "$N_SKIP" "$FAILED" "$(json_array "$BLOCKED_SUITES")"
    printf '}\n'
}

# ---------------------------------------------------------------------------
# Self-test
#
# The classifier, the budget, and the JSON encoder are the parts whose bugs are
# invisible in a healthy environment, which is every environment that runs CI.
# ---------------------------------------------------------------------------

# Build a directory of symlinks to the POSIX tools this script is written
# against, for a PATH that holds those and nothing else. Symlinks to the real
# binaries rather than a hardcoded /usr/bin, because a NixOS host has almost
# nothing there and the test would be asserting the wrong thing. Fails when the
# host is missing one, so the caller skips instead of asserting nonsense.
posix_path() {
    local bin=$1 tool path
    mkdir -p "$bin"
    for tool in awk bash cat cut df dirname env git grep mktemp printenv rm sed sleep sort tr; do
        path=$(command -v "$tool" 2>/dev/null)
        if [ -z "$path" ]; then
            printf 'doctor: self-test: skipping a bare-PATH run, no %s\n' "$tool" >&2
            return 1
        fi
        ln -sf "$path" "$bin/$tool"
    done
}

# Run the whole thing against that PATH, which is the environment this exists
# for and the one CI never has. Uses the `check` helper its caller defines.
self_test_incomplete() {
    local bin="$SCRATCH/bin"
    posix_path "$bin" || return 0

    local out status
    out=$(PATH="$bin" MOQ_STRICT='' "$SELF" --json --suite check 2>/dev/null)
    status=$?
    check 'incomplete run still reports' "$status" 0
    check 'incomplete run reports a missing tool' \
        "$(printf '%s' "$out" | grep -c '"id":"tool.just","section":"tools","status":"missing"')" 1
    check 'incomplete run blocks the suite' \
        "$(printf '%s' "$out" | grep -c '"blocked_suites":\["check"\]')" 1
    # A capability nothing required is still reported, so a diagnostic run gives
    # back every independent result rather than stopping at the first refusal.
    check 'incomplete run still reports github' \
        "$(printf '%s' "$out" | grep -c '"id":"github.read"')" 1

    PATH="$bin" MOQ_STRICT=1 "$SELF" --suite check >/dev/null 2>&1
    check 'strict refuses an incomplete scope' "$?" 1

    # An unprobed capability is not a cleared one. Plain `smoke` runs without
    # bun, but bun is what binds the probe socket, so reporting the bind as an
    # optional skip would let a strict harness run pass on a sandbox that
    # forbids one, having never asked.
    out=$(PATH="$bin" MOQ_STRICT='' "$SELF" --json --suite smoke 2>/dev/null)
    check 'an unprobed bind is not cleared' \
        "$(printf '%s' "$out" | grep -c '"id":"probe.loopback-tcp","section":"probe","status":"missing"')" 1
}

# A diff touching the root justfile widens the real dispatch to `check-all`, so
# the diagnosis has to widen with it, and an explicit base that does not resolve
# is refused rather than silently answered about some other scope. Uses the
# `check` helper its caller defines.
self_test_orchestration() {
    check 'the root justfile widens' "$(widen_orchestration justfile)" ALL
    check 'test/justfile widens' \
        "$(widen_orchestration "$(printf 'doc/a.md\ntest/justfile')")" ALL
    check 'anything else stays narrow' "$(widen_orchestration doc/a.md)" doc/a.md
    check 'ALL stays ALL' "$(widen_orchestration ALL)" ALL
    # A per-language justfile is a language scope like any other file under it.
    check 'a language justfile stays narrow' "$(widen_orchestration rs/justfile)" rs/justfile

    "$SELF" --base no/such/ref/for/doctor >/dev/null 2>&1
    check 'an unresolvable base is refused' "$?" 2

    # The same refusal without just, which is what resolves a base at all.
    # Answering about everything instead would name a scope nobody asked for,
    # and this is the environment the command exists to diagnose.
    local bin="$SCRATCH/nojust"
    if posix_path "$bin"; then
        PATH="$bin" "$SELF" --base origin/main >/dev/null 2>&1
        check 'a base with no just is refused' "$?" 2
    fi
}

# A capability is charged to the suites that actually invoke it, never to the
# union of everything selected. Charging the union is what makes a strict
# harness run fail on a linter it never calls, which is the false negative this
# command exists to remove. Uses the `check` helper its caller defines.
self_test_ownership() {
    local saved_pairs=${PAIRS:-} saved_suites=$SUITES

    SUITES="check smoke"
    PAIRS=$(tool_pairs "$SUITES" 'rs/moq-net/src/lib.rs')
    check 'the dispatch belongs to every suite' "$(suites_for_tool git)" 'check smoke'
    check 'a file-scope linter belongs to check' "$(suites_for_tool shellcheck)" check
    check 'a harness tool belongs to its harness' "$(suites_for_tool ffmpeg)" smoke
    check 'a shared tool names both' "$(suites_for_tool cargo)" 'check smoke'
    check 'an unclaimed tool names nothing' "$(suites_for_tool gradle)" ''
    check 'an orchestration test widens to Cargo' \
        "$(tools_for_test_files justfile | grep -c '^cargo$')" 1
    check 'an orchestration test widens to Python' \
        "$(tools_for_test_files justfile | grep -c '^uv$')" 1
    check 'an orchestration test excludes check-only Gradle' \
        "$(tools_for_test_files justfile | grep -c '^gradle$')" 0
    check 'an oversized test needs only three language toolchains' \
        "$(tools_for_test_files ALL | tr '\n' ' ')" 'bun cargo uv '
    check 'a Go-only test needs no toolchain' "$(tools_for_test_files go/wrapper/moq/lib.go)" ''

    SUITES="smoke"
    PAIRS=$(tool_pairs "$SUITES" 'rs/moq-net/src/lib.rs')
    check 'a harness run drops the linters' "$(suites_for_tool shellcheck)" ''

    # A docs-only `test` compiles nothing and hands no scope to a recipe that
    # binds, so a sandbox forbidding bind blocks none of it.
    SUITES="check test"
    check 'a docs diff binds no TCP' "$(bind_suites 'doc/a.md' '' tcp)" ''
    check 'a JS diff binds no TCP' "$(bind_suites 'js/hang/src/index.ts' '' tcp)" ''
    check 'a root Python diff binds UDP under test' "$(bind_suites pyproject.toml '' udp)" test
    check 'a Python diff binds no TCP under test' "$(bind_suites 'py/moq-rs/tests/test_server.py' '' tcp)" ''
    check 'an HLS Rust diff binds TCP under test' \
        "$(bind_suites 'rs/moq-hls/src/lib.rs' '--package path+file:///repo/rs/moq-hls#0.0.0' tcp)" test
    check 'an HLS Rust diff binds no UDP under test' \
        "$(bind_suites 'rs/moq-hls/src/lib.rs' '--package path+file:///repo/rs/moq-hls#0.0.0' udp)" ''
    check 'an SRT Rust diff binds UDP under test' \
        "$(bind_suites 'rs/moq-srt/src/lib.rs' '--package path+file:///repo/rs/moq-srt#0.0.0' udp)" test
    check 'an SRT Rust diff binds no TCP under test' \
        "$(bind_suites 'rs/moq-srt/src/lib.rs' '--package path+file:///repo/rs/moq-srt#0.0.0' tcp)" ''
    check 'a pure Rust diff binds nothing under test' \
        "$(bind_suites 'rs/quest/src/lib.rs' '--package path+file:///repo/rs/quest#0.0.0' tcp)" ''
    check 'a Go diff binds nothing under test' "$(bind_suites 'go/wrapper/moq/lib.go' '' tcp)" ''
    check 'a Go diff uses Cargo under check only' \
        "$(cargo_suites 'go/wrapper/moq/lib.go' '')" check
    check 'a Rust diff uses Cargo under both' \
        "$(cargo_suites 'rs/moq-net/src/lib.rs' packages)" 'check test'
    check 'a Python diff runs no Rust suite commands' "$(rust_suites '')" ''
    check 'a Rust diff runs both Rust suite commands' "$(rust_suites packages)" 'check test'
    check 'Cargo home prefers its override' "$(HOME=/home CARGO_HOME=/cargo cargo_home)" /cargo
    check 'Cargo home falls back to HOME' "$(HOME=/home CARGO_HOME= cargo_home)" /home/.cargo
    (
        unset HOME CARGO_HOME
        cargo_home >/dev/null
    )
    check 'Cargo home rejects an unknown location' "$?" 1
    check 'an absolute harness port root is preserved' \
        "$(MOQ_TEST_PORTS=/custom/ports harness_root ports)" /custom/ports
    check 'a relative harness port root uses the test directory' \
        "$(MOQ_TEST_PORTS=relative harness_root ports)" "$REPO/test/relative"
    check 'the filesystem root is preserved' "$(MOQ_TEST_PORTS=/ harness_root ports)" /
    check 'the shared harness preserves the filesystem root' \
        "$(bash -c 'source "$1"; harness_normalize_root /' _ "$REPO/test/lib/harness.sh")" /
    check 'doctor preserves an absolute temporary root' "$(TMPDIR=/custom doctor_tmpdir)" /custom
    check 'doctor isolates a relative harness temporary root' "$(TMPDIR=relative doctor_tmpdir)" /tmp
    valid_harness_port 1024
    check 'the first harness port is valid' "$?" 0
    valid_harness_port 65536
    check 'a harness port above the range is rejected' "$?" 1
    valid_harness_port words
    check 'a nonnumeric harness port is rejected' "$?" 1
    valid_harness_port_span 65533 3
    check 'three wasm allocator slots fit below the limit' "$?" 0
    valid_harness_port_span 65534 3
    check 'three wasm allocator slots reject a short range' "$?" 1
    valid_harness_port_span 65534 2
    check 'a pinned wasm run needs only two allocator slots' "$?" 0
    valid_harness_port_span 65534 2 65534
    check 'a pinned port inside the wasm allocator range consumes a slot' "$?" 1
    valid_harness_port_span 65534 2 60000
    check 'a pinned port outside the wasm allocator range consumes no slot' "$?" 0
    check 'an empty diff selects no Rust packages' "$(rust_packages '')" ''
    check 'a nested path uses its existing ancestor' \
        "$(existing_ancestor "$SCRATCH/new/parent/cache")" "$SCRATCH"
    ln -s missing "$SCRATCH/dangling"
    existing_ancestor "$SCRATCH/dangling" >/dev/null
    check 'a dangling symlink has no writable ancestor' "$?" 1
    check 'one Rust seed skips the multiline awk probe' \
        "$(needs_awk_select 'rs/moq-net/src/lib.rs' && printf yes || printf no)" no
    check 'two Rust seeds require the multiline awk probe' \
        "$(needs_awk_select "$(printf 'rs/moq-net/src/lib.rs\nrs/hang/src/lib.rs')" && printf yes || printf no)" yes
    # `check` lints and compiles; nothing in it listens, so it is never charged.
    check 'an unscoped run binds TCP under test alone' "$(bind_suites ALL ALL tcp)" test
    check 'a docs diff skips Nix store access' "$(nix_suites 'doc/a.md')" ''
    check 'a Rust diff probes Nix store access' "$(nix_suites 'rs/moq-net/src/lib.rs')" check

    SUITES="check smoke wasm"
    check 'the harnesses always bind' "$(bind_suites 'doc/a.md' '' udp)" 'smoke wasm'

    PAIRS=$saved_pairs
    SUITES=$saved_suites
}

# A zombie holds a pid without holding a resource, and `kill -0` succeeds on
# one, so the leak check has to look past it. Uses the `check` helper its
# caller defines.
self_test_process_state() {
    check 'the current process is alive' "$(process_state $$)" alive
    # Reaped immediately by this shell, so by the time it is asked the pid is
    # either gone or a zombie, and both answers must read the same.
    local reaped
    sh -c 'exit 0' &
    reaped=$!
    wait "$reaped" 2>/dev/null
    check 'a reaped child is gone' "$(process_state "$reaped")" gone
}

self_test() {
    local fail=0
    check() {
        if [ "$2" != "$3" ]; then
            printf 'doctor: self-test: %s: expected %s, got %s\n' "$1" "$3" "$2" >&2
            fail=1
        fi
    }

    check 'escape quotes' "$(json_string 'a"b')" '"a\"b"'
    check 'escape backslash' "$(json_string 'a\b')" '"a\\b"'
    check 'escape newline' "$(json_string "$(printf 'a\nb')")" '"a\nb"'
    check 'drop control chars' "$(json_string "$(printf 'a\001b')")" '"ab"'
    check 'array' "$(json_array 'a b')" '["a","b"]'
    check 'empty array' "$(json_array '')" '[]'

    SCRATCH=$(mktemp -d)
    BOUNDED_TMP="$SCRATCH/out"
    trap 'rm -rf "$SCRATCH"' EXIT

    bounded 5 sh -c 'echo hello'
    check 'bounded status' "$?" 0
    check 'bounded output' "$BOUNDED_OUT" hello

    bounded 5 sh -c 'exit 3'
    check 'bounded propagates status' "$?" 3

    # A grandchild that outlives the budget is the failure worth testing: the
    # immediate PID is a wrapper, and the cargo build or Chromium underneath it
    # is what keeps burning the machine after doctor has already reported.
    # shellcheck disable=SC2016  # $! and $GRANDCHILD belong to the inner shell.
    bounded 1 env GRANDCHILD="$SCRATCH/grandchild" sh -c 'sleep 30 & echo $! > "$GRANDCHILD"; wait'
    check 'bounded timeout' "$?" 124
    check 'bounded timeout elapsed' "$BOUNDED_ELAPSED" 1

    local orphan ticks=0
    orphan=$(cat "$SCRATCH/grandchild" 2>/dev/null)
    # Signal delivery and reaping are asynchronous, so give it a moment before
    # calling it a leak.
    while ((ticks < 20)) && [ "$(process_state "$orphan")" = alive ]; do
        sleep 0.1
        ticks=$((ticks + 1))
    done
    check 'bounded kills the process tree' "$(process_state "$orphan")" gone

    # Only the suites this run selected are blocked. A probe declares every
    # suite it serves, and counting an unselected one would fail a strict run
    # whose own capabilities are all present.
    local saved_suites=$SUITES
    SUITES="smoke"
    check 'selected drops unselected suites' "$(selected 'check test')" ''
    check 'selected keeps selected suites' "$(selected 'smoke wasm')" 'smoke'
    SUITES=$saved_suites

    # Malformed input is refused, not defaulted: a typo that quietly runs the
    # default scope reports a plausible answer to a question nobody asked.
    "$SELF" --suite nonsense >/dev/null 2>&1
    check 'unknown suite is refused' "$?" 2
    "$SELF" --suite >/dev/null 2>&1
    check 'bare --suite is refused' "$?" 2
    "$SELF" --base >/dev/null 2>&1
    check 'bare --base is refused' "$?" 2

    check 'classify timeout' "$(classify 124 '')" timeout
    check 'classify denied' "$(classify 1 'bind: Operation not permitted')" denied
    check 'classify denied eacces' "$(classify 1 'open failed: EACCES')" denied
    check 'classify degraded' "$(classify 1 'no such subcommand')" degraded

    # The tool mapping is what `_tools` enforces, so a diff that reaches a
    # language has to pull that language's toolchain in.
    check 'tools always' "$(tools_for_files '' | tr '\n' ' ')" 'actionlint bun jq nix nixfmt shellcheck shfmt taplo '
    check 'tools rust' "$(tools_for_files 'rs/moq-net/src/lib.rs' | grep -c '^cargo$')" 1
    check 'tools Kotlin needs Cargo' "$(tools_for_files 'kt/moq/src/main.kt' | grep -c '^cargo$')" 1
    check 'tools Kotlin needs rustc' "$(tools_for_files 'kt/moq/src/main.kt' | grep -c '^rustc$')" 1
    check 'tools ffi pulls go' "$(tools_for_files 'rs/moq-ffi/src/lib.rs' | grep -c '^go$')" 1
    check 'tools ALL pulls gradle' "$(tools_for_files ALL | grep -c '^gradle$')" 1
    check 'tools js only' "$(tools_for_files 'js/hang/src/index.ts' | grep -c '^cargo$')" 0

    # Exactly what each harness refuses to start without. A suite reported
    # healthy that then dies in its own prerequisite check is the failure this
    # command exists to remove.
    check 'smoke needs ffmpeg' "$(tools_for_suite smoke | grep -c '^ffmpeg$')" 1
    check 'smoke needs timeout' "$(tools_for_suite smoke | grep -c '^timeout$')" 1
    check 'smoke no longer needs pgrep' "$(tools_for_suite smoke | grep -c '^pgrep$')" 0
    check 'wasm needs wasm-bindgen' "$(tools_for_suite wasm | grep -c '^wasm-bindgen$')" 1
    check 'wasm needs curl' "$(tools_for_suite wasm | grep -c '^curl$')" 1
    # smoke's default matrix is Rust alone, and smoke.sh only marks the browser
    # clients broken without bun, so plain smoke is not blocked by its absence.
    check 'plain smoke does not need bun' "$(tools_for_suite smoke | grep -c '^bun$')" 0
    # smoke-full names every client on its command line and a broken one sets
    # overall=1, so its whole fixed matrix is a prerequisite there.
    check 'smoke-full needs bun' "$(tools_for_suite smoke-full | grep -c '^bun$')" 1
    check 'smoke-full needs go' "$(tools_for_suite smoke-full | grep -c '^go$')" 1
    check 'smoke-full needs uv' "$(tools_for_suite smoke-full | grep -c '^uv$')" 1
    check 'smoke-full needs a gstreamer' "$(tools_for_suite smoke-full | grep -c '^gst-launch-1.0$')" 1
    check 'dynamic C compiler stays out of tool words' "$(CC='/tmp/c compiler' tools_for_suite smoke-full | grep -c 'c compiler')" 0
    mkdir -p "$SCRATCH/compiler dir"
    ln -sf "$(command -v env)" "$SCRATCH/compiler dir/c compiler"
    check 'relative C compiler resolves from smoke cwd' \
        "$(command_path "$SCRATCH/compiler dir" './c compiler')" './c compiler'
    check 'dynamic pkg-config stays out of tool words' "$(tools_for_suite smoke-full | grep -c 'pkg-config')" 0
    check 'pkg-config target override wins' \
        "$(PKG_CONFIG_doctor_test_host=/target HOST_PKG_CONFIG=/host PKG_CONFIG=/generic targeted_env PKG_CONFIG doctor-test-host doctor-test-host)" /target
    check 'pkg-config host override wins generic' \
        "$(HOST_PKG_CONFIG='/host pkg-config' PKG_CONFIG=/generic targeted_env PKG_CONFIG doctor-test-host doctor-test-host)" '/host pkg-config'
    check 'pkg-config target kind wins generic' \
        "$(TARGET_PKG_CONFIG=/cross PKG_CONFIG=/generic targeted_env PKG_CONFIG doctor-test-target doctor-test-host)" /cross
    check 'pkg-config default does not substitute pkgconf' \
        "$(
            unset PKG_CONFIG HOST_PKG_CONFIG PKG_CONFIG_doctor_test_host
            pkg_config_executable doctor-test-host doctor-test-host
        )" pkg-config
    relative_command ./tools/pkg-config
    check 'relative pkg-config override is context dependent' "$?" 0
    mkdir -p "$SCRATCH/pkg-config-bin"
    ln -sf "$(command -v env)" "$SCRATCH/pkg-config-bin/pkg-config"
    PATH="$SCRATCH/pkg-config-bin:./later" relative_command pkg-config
    check 'PATH stops at the first pkg-config match' "$?" 1
    PATH="./first:$SCRATCH/pkg-config-bin" relative_command pkg-config
    check 'relative PATH before pkg-config is context dependent' "$?" 0
    PATH="$SCRATCH/missing:" relative_command pkg-config
    check 'trailing empty PATH is context dependent' "$?" 0
    mkdir -p "$SCRATCH/pkg-config-dir/pkg-config"
    PATH="$SCRATCH/pkg-config-dir:./later" relative_command pkg-config
    check 'PATH ignores executable directories' "$?" 0
    relative_search_path '/absolute/one:/absolute/two'
    check 'absolute pkg-config search paths are stable' "$?" 1
    relative_search_path '/absolute:relative'
    check 'relative pkg-config search path is context dependent' "$?" 0
    if command -v cargo >/dev/null 2>&1; then
        mkdir -p "$SCRATCH/cargo-config/.cargo"
        cat >"$SCRATCH/cargo-config/.cargo/config.toml" <<'EOF'
[build]
target = "wasm32-unknown-unknown"
EOF
        local configured_target
        configured_target=$(
            unset CARGO_BUILD_TARGET
            cargo_targets cargo "" "$SCRATCH/cargo-config"
            printf '%s %s' "$CARGO_PROBE_TARGET" "$CARGO_PROBE_EXPLICIT_TARGET"
        )
        check 'Cargo config selects an explicit artifact target' "$configured_target" 'wasm32-unknown-unknown true'

        cat >"$SCRATCH/cargo-wrapper" <<'EOF'
#!/usr/bin/env bash
CARGO_BUILD_TARGET=wasm32-unknown-unknown exec cargo "$@"
EOF
        chmod +x "$SCRATCH/cargo-wrapper"
        configured_target=$(
            unset CARGO_BUILD_TARGET
            cargo_targets "$SCRATCH/cargo-wrapper"
            printf '%s %s' "$CARGO_PROBE_TARGET" "$CARGO_PROBE_EXPLICIT_TARGET"
        )
        check 'configured Cargo wrapper selects the artifact target' "$configured_target" 'wasm32-unknown-unknown true'

        RUST_CARGO=/no/such/wrapper probe_cargo_compile smoke-cargo smoke "" cargo
        check 'smoke compile uses literal Cargo' "${R_STATUS[${#R_STATUS[@]} - 1]}" ok
        CARGO_TARGET_DIR= probe_cargo_compile empty-target wasm ""
        check 'empty Cargo target directory is refused' "${R_STATUS[${#R_STATUS[@]} - 1]}" degraded
        CARGO_TARGET_DIR=relative RELAY_BIN="$(command -v env)" \
            probe_harness_cargo_target wasm wasm cargo
        check 'relay override permits relative Cargo target directory' "${R_STATUS[${#R_STATUS[@]} - 1]}" ok
    fi
    check 'plain smoke needs no gstreamer' "$(tools_for_suite smoke | grep -c '^gst-launch-1.0$')" 0

    self_test_process_state
    self_test_ownership
    self_test_incomplete
    self_test_orchestration

    unset -f check
    if ((fail)); then
        return 1
    fi
    printf 'doctor: self-test ok\n'
}

# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

DISK_FLOOR_GIB=5
SELF=$(cd "$(dirname "$0")" && pwd)/$(basename "$0")
JSON=0
STRICT=${MOQ_STRICT:+1}
STRICT=${STRICT:-0}
BASE=""
SUITES=""
MODE=run

while (($#)); do
    case $1 in
        --json) JSON=1 ;;
        --strict) STRICT=1 ;;
        --base)
            shift
            [ $# -gt 0 ] || {
                printf 'doctor: --base needs a git ref\n' >&2
                exit 2
            }
            BASE=$1
            ;;
        --suite)
            shift
            # Refused rather than defaulted: a typo that runs the default scope
            # and reports it as the requested one is a plausible, wrong answer,
            # and a wrong answer is what this whole command exists to prevent.
            [ $# -gt 0 ] || {
                printf 'doctor: --suite needs a name\n' >&2
                usage
                exit 2
            }
            case $1 in
                all | check | smoke | smoke-full | test | wasm) ;;
                *)
                    printf 'doctor: unknown suite: %s\n' "$1" >&2
                    usage
                    exit 2
                    ;;
            esac
            SUITES="$SUITES $1"
            ;;
        --tools)
            shift
            tools_for_files "${1:-}"
            exit 0
            ;;
        --test-tools)
            shift
            tools_for_test_files "${1:-}"
            exit 0
            ;;
        --self-test) MODE=self-test ;;
        -h | --help)
            usage
            exit 0
            ;;
        *)
            printf 'doctor: unknown argument: %s\n' "$1" >&2
            usage
            exit 2
            ;;
    esac
    shift
done

if [ "$MODE" = self-test ]; then
    REPO=$(cd "$(dirname "$SELF")/.." && pwd)
    HARNESS_TMPDIR=${TMPDIR:-/tmp}
    TMPDIR=$(doctor_tmpdir)
    export TMPDIR
    self_test
    exit $?
fi

REPO=$(git rev-parse --show-toplevel 2>/dev/null)
if [ -z "$REPO" ]; then
    printf 'doctor: not inside a git checkout\n' >&2
    exit 2
fi
cd "$REPO" || exit 2

# Every fixture, probe crate, and captured output lands here. A relative TMPDIR
# is a harness path rooted under test/, so doctor uses a private absolute root
# rather than interpreting that same setting from a different working directory.
HARNESS_TMPDIR=${TMPDIR:-/tmp}
SCRATCH_ROOT=$(doctor_tmpdir)
SCRATCH=$(TMPDIR="$SCRATCH_ROOT" mktemp -d 2>/dev/null)
if [ -z "$SCRATCH" ] || [ ! -d "$SCRATCH" ]; then
    printf 'doctor: cannot create a scratch directory in %s\n' "$SCRATCH_ROOT" >&2
    printf '        grant write access to %s, or set TMPDIR to a writable absolute path\n' "$SCRATCH_ROOT" >&2
    exit 2
fi
BOUNDED_TMP="$SCRATCH/out"
trap 'rm -rf "$SCRATCH"' EXIT
# Keep just and every private probe from reinterpreting a relative harness
# TMPDIR at doctor's repository-root working directory.
export TMPDIR="$SCRATCH_ROOT"

# Scope. `_changed` is the same resolver `check` and `test` use, so the scope
# reported here is the scope those will pick.
BASE_REF="$BASE"
CHANGED=""
CHANGED_COUNT=0
if command -v just >/dev/null 2>&1; then
    if CHANGED=$(just _changed "$BASE" 2>"$SCRATCH/base"); then
        BASE_REF=$(sed -n 's/^base: //p' "$SCRATCH/base" | tail -1)
        : "${BASE_REF:=unknown}"
    elif [ -n "$BASE" ]; then
        # An explicit ref that does not resolve is a question this cannot
        # answer. Widening to everything would report a scope the caller never
        # asked about, and call it their base.
        printf 'doctor: cannot resolve --base %s\n' "$BASE" >&2
        sed 's/^/        /' "$SCRATCH/base" >&2
        exit 2
    else
        CHANGED=ALL
        BASE_REF="unresolved"
    fi
elif [ -n "$BASE" ]; then
    # Same refusal as an unresolvable ref, and for the same reason: `_changed`
    # is what resolves a base, so without just the requested comparison cannot
    # be made at all. Answering about everything instead would report a scope
    # the caller never asked for.
    printf 'doctor: cannot resolve --base %s: just is not installed\n' "$BASE" >&2
    printf '        install just, or drop --base to diagnose the whole repository\n' >&2
    exit 2
else
    # Without just there is no scope to narrow to, and a narrow report would
    # understate what is missing, so require everything.
    CHANGED=ALL
    BASE_REF="unresolved (just is unavailable)"
fi

TEST_FILES=$CHANGED
CHANGED=$(widen_orchestration "$CHANGED")

if [ "$CHANGED" = ALL ]; then
    CHANGED_COUNT=0
else
    CHANGED_COUNT=$(printf '%s' "$CHANGED" | grep -c . || true)
fi

# Suites. Auto is what `just check` and `just test` dispatch; the cross-language
# harnesses are opt-in so a docs diff does not go looking for a browser.
SUITES=$(printf '%s' "$SUITES" | tr ' ' '\n' | grep -v '^$' | sort -u | tr '\n' ' ')
SUITES=${SUITES% }
case " $SUITES " in
    *" all "*) SUITES="check smoke smoke-full test wasm" ;;
    "  ") SUITES="check test" ;;
esac

# Which language scopes the diff reached, for the header line only.
SCOPE_LABEL=""
if [ "$CHANGED" = ALL ]; then
    SCOPE_LABEL="everything"
else
    for area in cpp dart demo doc drafts go js kt py quest rs scripts swift test; do
        printf '%s\n' "$CHANGED" | grep -q "^$area/" && SCOPE_LABEL="$SCOPE_LABEL $area"
    done
    SCOPE_LABEL=${SCOPE_LABEL# }
fi

# Environment identity.
if [ -n "${MOQ_DEV_SHELL:-}" ]; then
    SHELL_ID="moq dev shell (IN_NIX_SHELL=${IN_NIX_SHELL:-unset})"
elif [ -n "${IN_NIX_SHELL:-}" ]; then
    SHELL_ID="a nix shell that is not this repo's (IN_NIX_SHELL=$IN_NIX_SHELL)"
else
    SHELL_ID="host toolchain, not the dev shell"
fi
TARGET_DIR=${CARGO_TARGET_DIR-$REPO/target}

probe_session

PAIRS=$(tool_pairs "$SUITES" "${CHANGED:-}" "${TEST_FILES:-}")

TOOL_LIST=$(printf '%s\n' "$PAIRS" | cut -d' ' -f1 | grep -v '^$' | sort -u)
for tool in $TOOL_LIST; do
    probe_tool "$tool" "$(suites_for_tool "$tool")"
done

# Both belong to `check` alone: alert.sh's coverage check and `nix flake check`
# run there and nowhere else, so a standalone harness diagnosis should not pay
# for them.
if [ -n "$(selected check)" ]; then
    probe_bun_yaml
else
    record behavior.bun-yaml behavior skip false "" "check is not selected" "" 10 0
fi

SMOKE_CARGO_SUITES=$(selected 'smoke smoke-full')
if [ -n "$SMOKE_CARGO_SUITES" ]; then
    probe_harness_cargo_target smoke "$SMOKE_CARGO_SUITES" cargo
else
    record behavior.smoke-artifact-layout behavior skip false "" "smoke is not selected" "" 30 0
fi

SMOKE_FULL_SUITES=$(selected smoke-full)
if [ -n "$SMOKE_FULL_SUITES" ]; then
    probe_c_compiler "$SMOKE_FULL_SUITES"
    probe_gstreamer_devel "$SMOKE_FULL_SUITES"
else
    record behavior.c-compiler behavior skip false "" "smoke-full is not selected" "" 10 0
    record behavior.gstreamer-devel behavior skip false "" "smoke-full is not selected" "" 10 0
fi

WASM_CARGO_SUITES=$(selected wasm)
if [ -n "$WASM_CARGO_SUITES" ]; then
    probe_harness_cargo_target wasm "$WASM_CARGO_SUITES" "${RUST_CARGO:-cargo}"
else
    record behavior.wasm-artifact-layout behavior skip false "" "wasm is not selected" "" 30 0
fi

# Ask the Rust selector once. Its result drives the Cargo, wasm-target, and
# test-bind probes; using check's broader tool list for those is what made a
# Go-only test pay for work it never dispatches. A failed selector is itself a
# blocked Rust dispatch, not an empty selection.
RUST_PACKAGES=$(rust_packages "$CHANGED" 2>"$SCRATCH/rust-select")
RUST_SELECT_STATUS=$?
if ((RUST_SELECT_STATUS != 0)); then
    record behavior.rust-select behavior degraded true "$(selected "check test")" \
        "the Rust package selector failed: $(error_line "$(cat "$SCRATCH/rust-select")")" \
        "enter the dev shell and run: just rs _select <changed files>" 30 0
    RUST_PACKAGES=""
fi
CARGO_SUITES=$(cargo_suites "$CHANGED" "$RUST_PACKAGES")
CARGO_SMOKE_SUITES=""
CARGO_RUST_SUITES=""
for suite in $CARGO_SUITES; do
    case $suite in
        smoke | smoke-full) CARGO_SMOKE_SUITES="$CARGO_SMOKE_SUITES $suite" ;;
        *) CARGO_RUST_SUITES="$CARGO_RUST_SUITES $suite" ;;
    esac
done
CARGO_SMOKE_SUITES=${CARGO_SMOKE_SUITES# }
CARGO_RUST_SUITES=${CARGO_RUST_SUITES# }

# A successful narrow selection exercised the same awk behavior as this
# fixture. A failed selection is already recorded above with its real output,
# while an empty or workspace-wide selection never evaluates the multiline
# seed expression.
if ((RUST_SELECT_STATUS == 0)) && [ -n "$(selected "check test")" ] && needs_awk_select "$CHANGED"; then
    probe_awk_select "$(selected "check test")"
else
    record behavior.awk-select behavior skip false "" "this scope selects no Rust packages" "" 10 0
fi

probe_writable scratch "${TMPDIR:-/tmp}" "$SUITES"

if [ -n "$CARGO_SUITES" ]; then
    if [ -z "$TARGET_DIR" ]; then
        record storage.target storage degraded true "$CARGO_SUITES" \
            "CARGO_TARGET_DIR is explicitly empty, which Cargo refuses" \
            "unset CARGO_TARGET_DIR or set it to a non-empty path" 5 0
        record storage.disk storage skip false "$CARGO_SUITES" "the Cargo target directory is malformed" "" 5 0
    else
        probe_writable target "$TARGET_DIR" "$CARGO_SUITES"
        probe_disk "$TARGET_DIR" "$CARGO_SUITES"
    fi
    if CARGO_HOME_DIR=$(cargo_home); then
        probe_writable cargo-home "$CARGO_HOME_DIR" "$CARGO_SUITES"
    else
        record storage.cargo-home storage missing true "$CARGO_SUITES" \
            "neither CARGO_HOME nor HOME is set, so Cargo's cache location is unknown" \
            "set HOME or CARGO_HOME to a writable directory" 5 0
    fi
else
    record storage.target storage skip false "" "this scope compiles nothing" "" 5 0
fi
if [ -n "$CARGO_RUST_SUITES" ]; then
    probe_cargo_compile cargo-compile "$CARGO_RUST_SUITES" "" "${RUST_CARGO:-cargo}"
else
    record probe.cargo-compile probe skip false "" "no selected Rust recipe compiles for this scope" "" 120 0
fi
if [ -n "$CARGO_SMOKE_SUITES" ]; then
    probe_cargo_compile cargo-smoke "$CARGO_SMOKE_SUITES" "" cargo
else
    record probe.cargo-smoke probe skip false "" "no selected Smoke suite compiles for this scope" "" 120 0
fi

# The subcommands each Rust suite invokes, charged to the suite that invokes
# them. The harnesses build and run, so they need none of these.
RUST_SUITES=$(rust_suites "$RUST_PACKAGES")
CHECK_CARGO=""
TEST_CARGO=""
case " $RUST_SUITES " in *" check "*) CHECK_CARGO="check" ;; esac
case " $RUST_SUITES " in *" test "*) TEST_CARGO="test" ;; esac
if [ -n "$CHECK_CARGO" ]; then
    probe_cargo_subcommand clippy "$CHECK_CARGO" "add the component: rustup component add clippy, or enter the dev shell: nix develop"
    probe_cargo_subcommand fmt "$CHECK_CARGO" "add the component: rustup component add rustfmt, or enter the dev shell: nix develop"
    probe_cargo_subcommand shear "$CHECK_CARGO" "install it: cargo install cargo-shear, or enter the dev shell: nix develop"
    probe_cargo_subcommand sort "$CHECK_CARGO" "install it: cargo install cargo-sort, or enter the dev shell: nix develop"
fi
if [ -n "$TEST_CARGO" ]; then
    probe_cargo_subcommand nextest "$TEST_CARGO" "install it: cargo install cargo-nextest, or enter the dev shell: nix develop"
fi

# `just rs wasm` cross-compiles moq-wasm, moq-mux, and moq-ffi for the target,
# so a check that reaches it needs the target as much as the wasm harness does.
# `check-all` runs it unconditionally; a narrow scope reaches it through
# `check-changed`'s gate, which is asked here rather than reimplemented, since
# the copy of a selection rule is the one that drifts.
WASM_TARGET_SUITES=$(selected wasm)
if wants_wasm "$RUST_PACKAGES"; then
    WASM_TARGET_SUITES="$(selected check) $WASM_TARGET_SUITES"
    WASM_TARGET_SUITES=$(printf '%s' "$WASM_TARGET_SUITES" | tr ' ' '\n' | grep -v '^$' | tr '\n' ' ')
    WASM_TARGET_SUITES=${WASM_TARGET_SUITES% }
fi
if [ -n "$WASM_TARGET_SUITES" ]; then
    probe_cargo_compile cargo-wasm32 "$WASM_TARGET_SUITES" wasm32-unknown-unknown
else
    record probe.cargo-wasm32 probe skip false "" "this scope compiles nothing for wasm32" "" 120 0
fi

NIX_SUITES=$(nix_suites "$CHANGED")
if [ -n "$NIX_SUITES" ]; then
    probe_nix
else
    record probe.nix probe skip false "" "this check scope does not evaluate the flake" "" 30 0
fi

TCP_BIND_SUITES=$(bind_suites "$CHANGED" "$RUST_PACKAGES" tcp)
UDP_BIND_SUITES=$(bind_suites "$CHANGED" "$RUST_PACKAGES" udp)

if [ -n "$TCP_BIND_SUITES" ]; then
    probe_loopback tcp "$TCP_BIND_SUITES"
else
    record probe.loopback-tcp probe skip false "" "this scope binds no sockets" "" 15 0
fi
if [ -n "$UDP_BIND_SUITES" ]; then
    probe_loopback udp "$UDP_BIND_SUITES"
else
    record probe.loopback-udp probe skip false "" "this scope binds no sockets" "" 15 0
fi

LOCK_SUITES=$(selected "smoke smoke-full wasm")
if [ -n "$LOCK_SUITES" ]; then
    SMOKE_BASE_SUITES=""
    [ -z "${SMOKE_PORT:-}" ] && SMOKE_BASE_SUITES=$(selected "smoke smoke-full")
    if [ -n "$SMOKE_BASE_SUITES" ]; then
        probe_harness_port harness-port-base-smoke MOQ_TEST_PORT_BASE \
            "${MOQ_TEST_PORT_BASE:-4500}" 1 "$SMOKE_BASE_SUITES"
    else
        record behavior.harness-port-base-smoke behavior skip false "" \
            "smoke is not selected or uses its pinned port" "" 5 0
    fi
    WASM_BASE_SUITES=$(selected wasm)
    if [ -n "$WASM_BASE_SUITES" ]; then
        WASM_BASE_SLOTS=3
        [ -n "${WASM_PORT:-}" ] && WASM_BASE_SLOTS=2
        probe_harness_port harness-port-base-wasm MOQ_TEST_PORT_BASE \
            "${MOQ_TEST_PORT_BASE:-4500}" "$WASM_BASE_SLOTS" "$WASM_BASE_SUITES" "${WASM_PORT:-}"
    else
        record behavior.harness-port-base-wasm behavior skip false "" "wasm is not selected" "" 5 0
    fi
    SMOKE_PIN_SUITES=$(selected "smoke smoke-full")
    if [ -n "$SMOKE_PIN_SUITES" ] && [ -n "${SMOKE_PORT:-}" ]; then
        probe_harness_port smoke-port SMOKE_PORT "$SMOKE_PORT" 1 "$SMOKE_PIN_SUITES"
    else
        record behavior.smoke-port behavior skip false "" "SMOKE_PORT is not selected or set" "" 5 0
    fi
    WASM_PIN_SUITES=$(selected wasm)
    if [ -n "$WASM_PIN_SUITES" ] && [ -n "${WASM_PORT:-}" ]; then
        probe_harness_port wasm-port WASM_PORT "$WASM_PORT" 1 "$WASM_PIN_SUITES"
    else
        record behavior.wasm-port behavior skip false "" "WASM_PORT is not selected or set" "" 5 0
    fi
    HARNESS_RUN_ROOT=$(harness_root runs)
    HARNESS_PORT_ROOT=$(harness_root ports)
    probe_writable harness-runs "$HARNESS_RUN_ROOT" "$LOCK_SUITES"
    probe_writable harness-ports "$HARNESS_PORT_ROOT" "$LOCK_SUITES"
    probe_advisory_lock "$LOCK_SUITES" "$HARNESS_PORT_ROOT"
else
    record behavior.harness-port-base-smoke behavior skip false "" "no harness suite is selected" "" 5 0
    record behavior.harness-port-base-wasm behavior skip false "" "no harness suite is selected" "" 5 0
    record behavior.smoke-port behavior skip false "" "no harness suite is selected" "" 5 0
    record behavior.wasm-port behavior skip false "" "no harness suite is selected" "" 5 0
    record storage.harness-runs storage skip false "" "no harness suite is selected" "" 5 0
    record storage.harness-ports storage skip false "" "no harness suite is selected" "" 5 0
    record probe.advisory-lock probe skip false "" "no harness suite is selected" "" 5 0
fi

# Plain `smoke` runs the Rust matrix and needs no browser; `smoke-full`
# publishes from one, and the wasm harness is nothing but one.
case " $SUITES " in *" smoke-full "*) probe_playwright smoke-full "$REPO/test/smoke/clients/js" ;; esac
case " $SUITES " in *" wasm "*) probe_playwright wasm "$REPO/test/wasm" ;; esac

probe_github

# Tally. A required check that is not ok blocks the selected suites it names,
# and only those: a probe declares every suite it serves, so a nix failure
# declared for `check` must not fail a `--strict --suite smoke` run whose own
# capabilities are all there. A check naming no selected suite is reported and
# never blocks, which is how a diagnostic run still returns every independent
# result.
N_OK=0
N_MISSING=0
N_DENIED=0
N_TIMEOUT=0
N_DEGRADED=0
N_SKIP=0
FAILED=0
BLOCKED=""
for ((i = 0; i < ${#R_ID[@]}; i++)); do
    case ${R_STATUS[$i]} in
        ok) N_OK=$((N_OK + 1)) ;;
        missing) N_MISSING=$((N_MISSING + 1)) ;;
        denied) N_DENIED=$((N_DENIED + 1)) ;;
        timeout) N_TIMEOUT=$((N_TIMEOUT + 1)) ;;
        degraded) N_DEGRADED=$((N_DEGRADED + 1)) ;;
        skip) N_SKIP=$((N_SKIP + 1)) ;;
    esac
    [ "${R_STATUS[$i]}" = ok ] && continue
    [ "${R_STATUS[$i]}" = skip ] && continue
    [ "${R_REQUIRED[$i]}" = true ] || continue
    blocks=$(selected "${R_SUITES[$i]}")
    [ -n "$blocks" ] || continue
    FAILED=$((FAILED + 1))
    BLOCKED="$BLOCKED $blocks"
done
BLOCKED_SUITES=$(printf '%s' "$BLOCKED" | tr ' ' '\n' | grep -v '^$' | sort -u | tr '\n' ' ')
BLOCKED_SUITES=${BLOCKED_SUITES% }

if ((JSON)); then
    emit_json
else
    emit_human
fi

# Strict refuses an incomplete scope. Without it every independent result is
# still reported, which is the whole point of a diagnostic run.
if ((STRICT)) && ((FAILED > 0)); then
    exit 1
fi
exit 0
