# shellcheck shell=bash
#
# Shared plumbing for the harnesses under test/, so two of them can run at once
# from two worktrees without testing or reaping each other's processes.
#
# Three things belong to a run and to nothing else:
#
#   - a private run directory holding every log, config, and scratch file
#   - the ports it reserved, held for the whole run rather than probed and freed
#   - the process groups it spawned, which are the only ones it ever signals
#
# Usage:
#
#     source "$(dirname "${BASH_SOURCE[0]}")/../lib/harness.sh"
#     harness_begin smoke "just test smoke"
#     harness_port relay
#     harness_spawn relay "$HARNESS_RUN/relay.log" "$RELAY" "$HARNESS_RUN/relay.toml"
#     harness_endpoint relay "http://127.0.0.1:$HARNESS_PORT"
#
# See test/README.md for the contract and the teardown rules.

HARNESS_LIB=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)

# Absolute path to this run's private directory. Every artifact goes here.
HARNESS_RUN=""

# The command that reproduces this run, printed when it fails or is retained.
HARNESS_RERUN=""

# PID of the group `harness_spawn` most recently started.
HARNESS_PID=""

# The port `harness_port` most recently reserved.
HARNESS_PORT=""

# Reservation directories this run holds, released on the way out.
HARNESS_PORTS=()

# Parallel arrays: the process group leaders this run spawned, their labels, and
# whether each has been waited on. A group is signalled only while its state
# reads `live`, which is what makes a recycled PID safe: once `wait` returns the
# state flips to `done` and this run never names that number again.
HARNESS_PIDS=()
HARNESS_LABELS=()
HARNESS_STATES=()

# ── run identity ────────────────────────────────────────────────────────────

# Print the caller's own argv, requoted so it can be pasted back: pass it "$@"
# from the top of the script, before anything consumes it. Reconstructing the
# flags from parsed variables instead is how a rerun command quietly drops the
# ones nobody remembered to add back.
harness_argv() {
    [[ $# -gt 0 ]] || return 0
    printf ' %q' "$@"
}

# Print the environment overrides among NAMES that are set, as a requoted prefix
# for the rerun command: `harness_env SMOKE_PORT SMOKE_PROFILE`. Timing, port,
# and profile knobs arrive this way rather than in argv, so a command built from
# argv alone reruns with the defaults and reproduces a different test.
#
# The variables this library reads are added for every caller rather than left to
# each harness to remember: they move the allocator and the artifact root, which
# is exactly what a collision or a filesystem failure depends on.
harness_env() {
    local name value
    for name in MOQ_TEST_RUNS MOQ_TEST_PORTS MOQ_TEST_PORT_BASE MOQ_TEST_KEEP "$@"; do
        value="${!name-}"
        [[ -n "$value" ]] || continue
        printf '%s=%q ' "$name" "$value"
    done
}

# Start a run named NAME, reproducible with RERUN. Creates the run directory and
# installs the teardown trap; every other function needs this called first.
harness_begin() {
    local name="$1" rerun="${2:-}"

    local root="${MOQ_TEST_RUNS:-${TMPDIR:-/tmp}}"
    root="${root%/}"
    [[ -n "${MOQ_TEST_RUNS:-}" ]] || root="$root/moq-test-$(id -u)"
    mkdir -p "$root"
    HARNESS_RUN=$(mktemp -d "$root/$name-XXXXXXXX")
    # A run directory holds relay configs and generated keys, so keep it to the
    # owner even where the temp root itself is world-readable.
    chmod 700 "$HARNESS_RUN"
    HARNESS_RERUN="$rerun"

    # Cancellation needs its own traps: children run in their own process groups
    # (see `harness_spawn`), so a ^C aimed at this shell's group never reaches
    # them, and teardown is the only thing that will.
    trap harness_finish EXIT
    trap 'harness_finish 130; exit 130' INT
    trap 'harness_finish 143; exit 143' TERM

    echo "run: $HARNESS_RUN"
}

# ── endpoints ───────────────────────────────────────────────────────────────

# Where port reservations live. Shared across worktrees on purpose: the point is
# that a run in one worktree cannot hand out a port another already took.
#
# The default is suffixed with the user id, like the run root. On Linux TMPDIR is
# usually unset, so both would land in a world-writable /tmp under a fixed name
# owned by whoever ran first: a second user's `mkdir` would then fail for every
# port and the walk would report the whole range taken with nothing reserved. Two
# worktrees still share, because they run as the same user.
harness_port_root() {
    local root="${MOQ_TEST_PORTS:-${TMPDIR:-/tmp}}"
    root="${root%/}"
    [[ -n "${MOQ_TEST_PORTS:-}" ]] || root="$root/moq-test-ports-$(id -u)"
    echo "$root"
}

# True when PORT is a number a test can bind: `harness_valid_port <port>`.
#
# The reservation is a directory named after the port, so this guards the
# filesystem as much as the socket: an unchecked `../../name` would create a
# directory outside the reservation root that teardown then removes.
harness_valid_port() {
    local port="$1"
    [[ "$port" =~ ^[1-9][0-9]*$ ]] && ((${#port} <= 5)) && ((port >= 1024 && port <= 65535))
}

# Reserve a port for this run, held until it exits, and set HARNESS_PORT.
#
# `harness_port <label> [wanted]`. With `wanted` that exact port is taken or the
# call fails, which is what an explicit SMOKE_PORT/WASM_PORT asks for; without it
# the search walks up from MOQ_TEST_PORT_BASE.
#
# The answer lands in a variable rather than on stdout because `$(harness_port)`
# would run it in a subshell, where the reservation it just took is recorded into
# a copy of the table and never released by the run that owns it.
#
# Holding the reservation for the run's lifetime is the difference from probing:
# a probe that finds a port free has already released it by the time the relay
# binds, so two runs that probe together pick the same number. An advisory lock
# serializes replacement, and a populated claim is renamed into place atomically.
# shellcheck disable=SC2034  # HARNESS_PORT is the result, read by the caller
harness_port() {
    local label="$1" wanted="${2:-}"
    local root port last status
    root=$(harness_port_root)
    mkdir -p "$root"

    if [[ -n "$wanted" ]]; then
        harness_valid_port "$wanted" || {
            echo "error: port for $label must be 1024..65535 (got '$wanted')" >&2
            return 1
        }
        if harness_port_take "$root" "$wanted"; then
            HARNESS_PORT="$wanted"
            return 0
        else
            status=$?
        fi
        if ((status == 1)); then
            echo "error: port $wanted ($label) is held by another run; see $root/$wanted" >&2
            return 1
        fi
        return "$status"
    fi

    port="${MOQ_TEST_PORT_BASE:-4500}"
    # A base outside the port range would hand out numbers no socket can bind, and
    # the run would report it much later as a relay that never became ready.
    harness_valid_port "$port" || {
        echo "error: MOQ_TEST_PORT_BASE must be 1024..65535 (got '$port')" >&2
        return 1
    }
    last=$((port + 500))
    ((last <= 65535)) || last=65535
    while ((port <= last)); do
        if harness_port_take "$root" "$port"; then
            HARNESS_PORT="$port"
            return 0
        else
            status=$?
        fi
        ((status == 1)) || return "$status"
        port=$((port + 1))
    done
    echo "error: no free port for $label in ${MOQ_TEST_PORT_BASE:-4500}..$last (see $root)" >&2
    return 1
}

# Claim one port. Private; `harness_port` is the entry point.
harness_port_take() {
    local root="$1" port="$2" lock="$1/.lock-$2"
    if command -v flock >/dev/null 2>&1; then
        flock "$lock" "$HARNESS_LIB/reserve.sh" "$root" "$port" "$$" "$HARNESS_RUN" || return $?
    elif command -v lockf >/dev/null 2>&1; then
        lockf -k "$lock" "$HARNESS_LIB/reserve.sh" "$root" "$port" "$$" "$HARNESS_RUN" || return $?
    else
        echo "error: port reservations require flock or lockf" >&2
        return 2
    fi
    HARNESS_PORTS+=("$root/$port")
}

# Record an endpoint this run stood up: `harness_endpoint <label> <url>`.
# Printed as it happens and appended to the run's endpoints.txt, so a retained
# session says what to open without rereading the script.
harness_endpoint() {
    local label="$1" url="$2"
    printf '%s\t%s\n' "$label" "$url" >>"$HARNESS_RUN/endpoints.txt"
    echo "endpoint: $label $url"
}

# Poll URL until it answers, up to SECONDS: `harness_ready <url> [seconds] [pid]`.
# Tight interval on purpose; a relay binds in about 130ms, so a half-second tick
# spends most of the wait asleep.
#
# With PID, the process that is supposed to be answering. A URL that answers
# while that process is gone is somebody else's server on our port, and the run
# would otherwise go on to test whatever binary that is; a process that has
# already exited will never bind, so waiting out the budget only delays the
# report of a bind that failed.
harness_ready() {
    local url="$1" seconds="${2:-30}" pid="${3:-}" deadline remaining
    deadline=$((SECONDS + seconds))
    while ((SECONDS < deadline)); do
        remaining=$((deadline - SECONDS))
        if harness_probe "$url" "$remaining"; then
            if [[ -n "$pid" ]] && harness_exited "$pid"; then
                echo "error: $url answered, but the process this run started is gone" >&2
                return 1
            fi
            return 0
        fi
        if [[ -n "$pid" ]] && harness_exited "$pid"; then
            return 1
        fi
        sleep 0.05
    done
    return 1
}

# Probe one URL without letting a connected but unresponsive peer block forever.
harness_probe() {
    local url="$1" seconds="${2:-1}"
    curl -sf --max-time "$seconds" "$url" >/dev/null 2>&1
}

# True once a spawned child has exited, waited on or not: `harness_exited <pid>`.
# `kill -0` cannot answer this, because an unreaped child is still a process and
# answers yes for a zombie. The process state can.
harness_exited() {
    local state
    state=$(ps -o state= -p "$1" 2>/dev/null || true)
    if [[ -n "$state" ]]; then
        [[ "$state" == Z* ]]
    else
        # A sandbox can deny process inspection even while signalling is allowed.
        # In that case, absence from `ps` does not prove the process is gone.
        ! kill -0 "$1" 2>/dev/null
    fi
}

# ── processes ───────────────────────────────────────────────────────────────

# Spawn a command as its own process group, writing to LOG (or `-` to inherit
# this shell's stdout/stderr): `harness_spawn <label> <log> <cmd...>`. Sets
# HARNESS_PID to the group leader. CMD may be a shell function.
#
# Job control creates the group, and it is enabled only across the `&` so the
# shell never prints its own "Killed" notice for a group we reaped on purpose.
# The group is the unit of ownership: signalling it catches grandchildren that a
# `pgrep -P` walk misses once they reparent.
#
# stdin is /dev/null because a background job in its own group that reads the
# terminal takes SIGTTIN and stops. ffmpeg reads stdin for keyboard commands and
# would do exactly that.
harness_spawn() {
    local label="$1" log="$2"
    shift 2
    set -m
    if [[ "$log" == "-" ]]; then
        "$@" </dev/null &
    else
        "$@" </dev/null >"$log" 2>&1 &
    fi
    HARNESS_PID=$!
    set +m
    HARNESS_PIDS+=("$HARNESS_PID")
    HARNESS_LABELS+=("$label")
    HARNESS_STATES+=("live")
    if declare -F bundle_process >/dev/null 2>&1; then
        bundle_process "$label" "$HARNESS_PID"
    fi
}

# Index of PID in the spawn table; fails when this run never spawned it.
#
# Newest first, because the OS can hand the same number to a later spawn in the
# same run. The first match going forward would be the retired entry, so reaping
# would treat the live process as already collected and leave it running.
harness_index() {
    local pid="$1" i
    for ((i = ${#HARNESS_PIDS[@]} - 1; i >= 0; i--)); do
        if [[ "${HARNESS_PIDS[$i]}" == "$pid" ]]; then
            echo "$i"
            return 0
        fi
    done
    return 1
}

# Wait for a spawned group leader and return its status: `harness_wait <pid>`.
#
# The leader finishing does not mean the group did: a launcher that dies while
# the browser it started keeps running leaves a survivor with no parent left to
# walk down from. So sweep the rest of the group before retiring the entry, which
# is also the last moment it is safe to name: a process group id stays reserved
# while any member lives, and becomes reusable the instant the last one exits.
harness_wait() {
    local pid="$1" status=0 i
    wait "$pid" || status=$?
    kill -KILL -- -"$pid" 2>/dev/null || true
    if i=$(harness_index "$pid"); then
        HARNESS_STATES[i]="done"
    fi
    return "$status"
}

# Kill a spawned group and reap it: `harness_reap <pid>`.
#
# SIGKILL rather than SIGTERM because moq-cli handles only SIGINT, so a polite
# signal leaves it running; these are ephemeral test processes either way. A PID
# this run did not spawn, or already waited on, is ignored: reaping may be called
# twice, and the second call must not signal whatever now holds the number.
harness_reap() {
    local pid="$1" i
    i=$(harness_index "$pid") || return 0
    [[ "${HARNESS_STATES[$i]}" == live ]] || return 0
    # The group first, so grandchildren go with it; the bare PID is the fallback
    # for a job that somehow never became a group leader.
    kill -KILL -- -"$pid" 2>/dev/null || kill -KILL "$pid" 2>/dev/null || true
    wait "$pid" 2>/dev/null || true
    HARNESS_STATES[i]="done"
}

# Reap every group this run still owns, newest first so clients go before the
# relay they were talking to.
harness_reap_all() {
    local i
    for ((i = ${#HARNESS_PIDS[@]} - 1; i >= 0; i--)); do
        harness_reap "${HARNESS_PIDS[$i]}"
    done
}

# Release every port reservation this run owns.
harness_release_ports() {
    local reservation
    for reservation in ${HARNESS_PORTS[@]+"${HARNESS_PORTS[@]}"}; do
        rm -rf "$reservation"
    done
    HARNESS_PORTS=()
}

# Mark retained reservations so the allocator keeps refusing them after this
# shell exits. Record the exact directories so teardown removes the claims.
harness_retain_ports() {
    local reservation i pid
    for i in ${HARNESS_PIDS[@]+"${!HARNESS_PIDS[@]}"}; do
        [[ "${HARNESS_STATES[$i]}" == live ]] || continue
        pid=${HARNESS_PIDS[$i]}
        if ! harness_exited "$pid" || kill -0 -- -"$pid" 2>/dev/null; then
            for reservation in ${HARNESS_PORTS[@]+"${HARNESS_PORTS[@]}"}; do
                : >"$reservation/retained"
                bundle_reservation "$reservation"
            done
            return 0
        fi
    done
    return 1
}

# ── teardown ────────────────────────────────────────────────────────────────

# Release everything this run owns: `harness_finish [status]`, where the status
# defaults to the one it inherits and is passed explicitly by the signal traps,
# which have no failing command to inherit it from.
#
# Reached from the EXIT/INT/TERM traps, so it has to hold for a cancellation
# during startup as well as a clean finish: nothing here assumes a process was
# spawned or a port was ever reserved.
harness_finish() {
    local status=${1:-$?}
    local reservation i
    trap - EXIT INT TERM

    # Name what is about to be force-reaped when the run is failing, so a hung
    # child is attributable rather than just a process that stopped existing.
    if ((status != 0)); then
        for i in ${HARNESS_PIDS[@]+"${!HARNESS_PIDS[@]}"}; do
            if [[ "${HARNESS_STATES[$i]}" == live ]]; then
                echo "reaping: ${HARNESS_LABELS[$i]} (pid ${HARNESS_PIDS[$i]})" >&2
            fi
        done
    fi

    harness_reap_all

    harness_release_ports

    [[ -n "$HARNESS_RUN" ]] || return "$status"

    if [[ "${MOQ_TEST_KEEP:-0}" == 0 ]]; then
        rm -rf "$HARNESS_RUN"
    else
        # The ports are already released and the children are gone, so a retained
        # directory is evidence only. Say so rather than implying a live session.
        echo "kept: $HARNESS_RUN (children reaped, ports released)" >&2
        echo "remove it with: rm -rf $HARNESS_RUN" >&2
    fi
    if [[ -n "$HARNESS_RERUN" && ("$status" -ne 0 || "${MOQ_TEST_KEEP:-0}" != 0) ]]; then
        echo "rerun: $HARNESS_RERUN" >&2
    fi
    return "$status"
}
