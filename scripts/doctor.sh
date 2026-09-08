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
    scoped '^(kt/|rs/moq-ffi/)' && tools="$tools gradle java"
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
# top of test/wasm/run.sh, plus the curl that run.sh polls every relay with. A
# per-client toolchain that smoke only marks broken is deliberately absent,
# because it fails its own matrix cells rather than the run: that is why bun
# belongs to `smoke-full` (`just test smoke-full`, which publishes from a
# browser) and not to plain `smoke`, whose default matrix is Rust alone.
tools_for_suite() {
    case $1 in
        smoke) printf 'cargo\ncurl\nffmpeg\npgrep\ntimeout\n' ;;
        smoke-full) printf 'bun\ncargo\ncurl\nffmpeg\npgrep\ntimeout\n' ;;
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

# Print which suite needs which tool, one `tool suite` pair per line, for a
# suite list and a changed-file scope. Per-suite ownership rather than one
# union: the file scope's tools belong to `check` and `test`, which are what
# dispatch them, so a missing actionlint must not block a strict `--suite smoke`
# diagnosis of a harness that never invokes it.
#
# git and just carry the dispatch itself, so they belong to every suite;
# `_tools` leaves them out because just is what runs it.
tool_pairs() {
    local suites=$1 changed=$2 suite tool
    for suite in $suites; do
        for tool in git just; do
            printf '%s %s\n' "$tool" "$suite"
        done
        case $suite in
            check | test)
                for tool in $(tools_for_files "$changed"); do
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

# Print the selected suites that actually bind a loopback socket, for a
# changed-file scope. Loopback belongs to whoever binds one: the cross-language
# harnesses always do, while `test` depends on the diff, because `just test`
# hands the file list to the js, rs, and py recipes and all three skip a scope
# they were not given. A docs-only diff binds nothing, and reporting it blocked
# in a sandbox that forbids bind would name a cost it never pays.
#
# The Rust half asks `tools_for_files` rather than the suites that compile,
# because `--suite test wasm` compiles Rust for the wasm harness on a diff that
# hands `just test` nothing.
bind_suites() {
    local changed=$1 out="" suite
    if tools_for_files "$changed" | grep -qx cargo ||
        printf '%s\n' "$changed" | grep -qE '^(js|py)/'; then
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
    case $(ps -o state= -p "$1" 2>/dev/null | tr -d '[:space:]') in
        Z* | '') printf 'gone\n' ;;
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

# A directory that exists is not a directory this session may write to.
probe_writable() {
    local id=$1 dir=$2 suites=$3 probe note=""
    if [ ! -d "$dir" ]; then
        # A directory that does not exist yet is fine as long as its parent takes
        # a mkdir, which is the same question one level up.
        local parent
        parent=$(dirname "$dir")
        if [ ! -d "$parent" ]; then
            record "storage.$id" storage missing true "$suites" "$dir does not exist and neither does $parent" \
                "create $parent" 5 0
            return
        fi
        note=", which will hold $dir"
        dir=$parent
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
    [ -d "$dir" ] || dir=$(dirname "$dir")
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
probe_nix_eval() {
    local suites="check"
    if ! command -v nix >/dev/null 2>&1; then
        # Not required here: `tool.nix` already reports the absence, and
        # reporting it twice would double-count one problem. An installed nix
        # that cannot reach its store is the case only this probe catches, and
        # that one does block `check`.
        record probe.nix-eval probe missing false "$suites" "nix is not installed" \
            "install nix, or accept that 'just check' skips 'nix flake check'" 30 0
        return
    fi
    local status
    bounded 30 nix eval --impure --raw --expr '"ok"'
    status=$?
    if ((status == 0)) && [ "$BOUNDED_OUT" = ok ]; then
        record probe.nix-eval probe ok true "$suites" "nix evaluates" "" 30 "$BOUNDED_ELAPSED"
        return
    fi
    record probe.nix-eval probe "$(classify "$status" "$BOUNDED_OUT")" true "$suites" \
        "nix eval failed: $(error_line "$BOUNDED_OUT")" \
        "allow reading ${NIX_STORE:-/nix/store} and connecting to /nix/var/nix/daemon-socket/socket" \
        30 "$BOUNDED_ELAPSED"
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
    local id=$1 suites=$2 target=$3 cargo=${RUST_CARGO:-cargo}
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

    local status label="for the host" remedy
    remedy="allow writing ${CARGO_HOME:-$HOME/.cargo} and ${TMPDIR:-/tmp}, and executing the linker"
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
    local sub=$1 suites=$2 remedy=$3 cargo=${RUST_CARGO:-cargo}
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

# Run the whole thing against a PATH holding only the handful of POSIX tools it
# is written against, which is the environment it exists for and the one CI
# never has. Uses the `check` helper its caller defines.
#
# The PATH is built out of symlinks to the real binaries rather than a hardcoded
# /usr/bin, because a NixOS host has almost nothing there and the test would be
# asserting the wrong thing.
self_test_incomplete() {
    local bin="$SCRATCH/bin" tool path
    mkdir -p "$bin"
    for tool in awk bash cat cut df dirname env git grep mktemp rm sed sleep sort tr; do
        path=$(command -v "$tool" 2>/dev/null)
        if [ -z "$path" ]; then
            printf 'doctor: self-test: skipping the incomplete-PATH run, no %s\n' "$tool" >&2
            return 0
        fi
        ln -sf "$path" "$bin/$tool"
    done

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

    SUITES="smoke"
    PAIRS=$(tool_pairs "$SUITES" 'rs/moq-net/src/lib.rs')
    check 'a harness run drops the linters' "$(suites_for_tool shellcheck)" ''

    # A docs-only `test` compiles nothing and hands no scope to a recipe that
    # binds, so a sandbox forbidding bind blocks none of it.
    SUITES="check test"
    check 'a docs diff binds nothing' "$(bind_suites 'doc/a.md')" ''
    check 'a js diff binds under test' "$(bind_suites 'js/hang/src/index.ts')" test
    check 'a rust diff binds under test' "$(bind_suites 'rs/moq-net/src/lib.rs')" test
    # `check` lints and compiles; nothing in it listens, so it is never charged.
    check 'an unscoped run binds under test alone' "$(bind_suites ALL)" test

    SUITES="check smoke wasm"
    check 'the harnesses always bind' "$(bind_suites 'doc/a.md')" 'smoke wasm'

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
    check 'tools ffi pulls go' "$(tools_for_files 'rs/moq-ffi/src/lib.rs' | grep -c '^go$')" 1
    check 'tools ALL pulls gradle' "$(tools_for_files ALL | grep -c '^gradle$')" 1
    check 'tools js only' "$(tools_for_files 'js/hang/src/index.ts' | grep -c '^cargo$')" 0

    # Exactly what each harness refuses to start without. A suite reported
    # healthy that then dies in its own prerequisite check is the failure this
    # command exists to remove.
    check 'smoke needs ffmpeg' "$(tools_for_suite smoke | grep -c '^ffmpeg$')" 1
    check 'smoke needs timeout' "$(tools_for_suite smoke | grep -c '^timeout$')" 1
    check 'wasm needs wasm-bindgen' "$(tools_for_suite wasm | grep -c '^wasm-bindgen$')" 1
    check 'wasm needs curl' "$(tools_for_suite wasm | grep -c '^curl$')" 1
    # smoke's default matrix is Rust alone, and smoke.sh only marks the browser
    # clients broken without bun, so plain smoke is not blocked by its absence.
    check 'plain smoke does not need bun' "$(tools_for_suite smoke | grep -c '^bun$')" 0
    check 'smoke-full needs bun' "$(tools_for_suite smoke-full | grep -c '^bun$')" 1

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
    self_test
    exit $?
fi

REPO=$(git rev-parse --show-toplevel 2>/dev/null)
if [ -z "$REPO" ]; then
    printf 'doctor: not inside a git checkout\n' >&2
    exit 2
fi
cd "$REPO" || exit 2

# Every fixture, probe crate, and captured output lands here. Without it the
# paths below would resolve against the filesystem root, and the report would be
# a list of unrelated root-level write failures instead of the one real problem.
SCRATCH=$(mktemp -d 2>/dev/null)
if [ -z "$SCRATCH" ] || [ ! -d "$SCRATCH" ]; then
    printf 'doctor: cannot create a scratch directory in %s\n' "${TMPDIR:-/tmp}" >&2
    printf '        grant write access to %s, or set TMPDIR to a writable path\n' "${TMPDIR:-/tmp}" >&2
    exit 2
fi
BOUNDED_TMP="$SCRATCH/out"
trap 'rm -rf "$SCRATCH"' EXIT

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
else
    # Without just there is no scope to narrow to, and a narrow report would
    # understate what is missing, so require everything.
    CHANGED=ALL
    BASE_REF="unresolved (just is unavailable)"
fi

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
TARGET_DIR=${CARGO_TARGET_DIR:-$REPO/target}

probe_session

PAIRS=$(tool_pairs "$SUITES" "${CHANGED:-}")

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

# Which selected suites actually compile Rust. `check` and `test` do only when
# the file scope reaches it, which is the same question `_tools` answers; the
# cross-language harnesses always do. Attributing these to a fixed `check test`
# would report a docs-only diff blocked on a toolchain it never invokes, and
# would leave a broken toolchain blocking nothing under `--suite wasm`.
CARGO_SUITES=$(suites_for_tool cargo)

# `just rs check-changed` is where the selector runs, and its seed list only
# grows past one line when the diff reaches Rust, so a docs-only branch on BSD
# awk is not blocked by it.
if [ -n "$(selected "check test")" ] && tools_for_files "${CHANGED:-}" | grep -qx cargo; then
    probe_awk_select "$(selected "check test")"
else
    record behavior.awk-select behavior skip false "" "this scope selects no Rust packages" "" 10 0
fi

probe_writable scratch "${TMPDIR:-/tmp}" "$SUITES"

if [ -n "$CARGO_SUITES" ]; then
    probe_writable target "$TARGET_DIR" "$CARGO_SUITES"
    probe_writable cargo-home "${CARGO_HOME:-$HOME/.cargo}" "$CARGO_SUITES"
    probe_disk "$TARGET_DIR" "$CARGO_SUITES"
    probe_cargo_compile cargo-compile "$CARGO_SUITES" ""
else
    record storage.target storage skip false "" "this scope compiles nothing" "" 5 0
    record probe.cargo-compile probe skip false "" "this scope compiles nothing" "" 120 0
fi

# The subcommands each Rust suite invokes, charged to the suite that invokes
# them. The harnesses build and run, so they need none of these.
CHECK_CARGO=""
TEST_CARGO=""
case " $CARGO_SUITES " in *" check "*) CHECK_CARGO="check" ;; esac
case " $CARGO_SUITES " in *" test "*) TEST_CARGO="test" ;; esac
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
# and `check-all` runs it unconditionally, so a widened check needs the target
# as much as the wasm harness does. A narrow Rust scope reaches it only through
# `just rs _wants-wasm`, which this cannot answer without running the package
# selector, so it is not charged there.
WASM_TARGET_SUITES=$(selected wasm)
if [ "$CHANGED" = ALL ]; then
    WASM_TARGET_SUITES="$(selected check) $WASM_TARGET_SUITES"
    WASM_TARGET_SUITES=$(printf '%s' "$WASM_TARGET_SUITES" | tr ' ' '\n' | grep -v '^$' | tr '\n' ' ')
    WASM_TARGET_SUITES=${WASM_TARGET_SUITES% }
fi
if [ -n "$WASM_TARGET_SUITES" ]; then
    probe_cargo_compile cargo-wasm32 "$WASM_TARGET_SUITES" wasm32-unknown-unknown
else
    record probe.cargo-wasm32 probe skip false "" "this scope compiles nothing for wasm32" "" 120 0
fi

if [ -n "$(selected check)" ]; then
    probe_nix_eval
else
    record probe.nix-eval probe skip false "" "check is not selected" "" 30 0
fi

BIND_SUITES=$(bind_suites "$CHANGED")

if [ -n "$BIND_SUITES" ]; then
    probe_loopback tcp "$BIND_SUITES"
    probe_loopback udp "$BIND_SUITES"
else
    record probe.loopback-tcp probe skip false "" "this scope binds no sockets" "" 15 0
    record probe.loopback-udp probe skip false "" "this scope binds no sockets" "" 15 0
fi

# Plain `smoke` runs the Rust matrix and needs no browser; `smoke-full`
# publishes from one, and the wasm harness is nothing but one.
case " $SUITES " in *" smoke-full "*) probe_playwright smoke-full "$REPO/test/smoke/clients/js" ;; esac
case " $SUITES " in *" wasm "*) probe_playwright wasm "$REPO/test/wasm" ;; esac

probe_github

# Tally. A required check that is not ok blocks the selected suites it names,
# and only those: a probe declares every suite it serves, so a nix-eval failure
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
