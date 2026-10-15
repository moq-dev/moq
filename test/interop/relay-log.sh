#!/usr/bin/env bash
# Relay connection accounting shared with the harness regression.
# shellcheck disable=SC2034 # overall is the sourcing harness's exit status.

relay_log_since() {
    tail -n +"$1" "$HARNESS_RUN/relay.log" | sed 's/\x1b\[[0-9;]*m//g'
}

# Print each relay connection opened from log line FROM on, with how it closed: the
# relay's close error, "ended" for a close without one, or "open" while it hasn't closed yet.
# The patterns follow the relay's `conn{id=...}` span and its `connection closed` log
# (`rs/moq-relay/src/connection.rs`). The relay numbers connections in accept order, so an id at
# or below the highest one logged before FROM was opened earlier: a straggler from an earlier
# round that logs or idles out here isn't blamed on this one.
relay_connections() {
    relay_log_since 1 | awk -v from="$1" '
        BEGIN { before = -1 }
        { id = -1; closed = match($0, /connection closed id=[0-9]+/) }
        closed { id = substr($0, RSTART + 21, RLENGTH - 21) + 0; rest = substr($0, RSTART + RLENGTH) }
        !closed && match($0, /conn\{id=[0-9]+ /) { id = substr($0, RSTART + 8, RLENGTH - 9) + 0 }
        id < 0 { next }
        NR < from { if (id > before) before = id; next }
        id <= before { next }
        !(id in state) { state[id] = "open"; order[++n] = id }
        closed { state[id] = sub(/^ err=/, "", rest) ? rest : "ended" }
        END { for (i = 1; i <= n; i++) print order[i], state[order[i]] }
    '
}

# Fail the round for every relay connection it opened that idled out rather than closed.
#
# The relay times a silent peer out after its 10 s QUIC idle timeout, which can land long
# after the cell that stalled finished. Every client closes its session on the way out, so
# each close shows up at once, and an idle-out is a real stall that a reconnect would
# otherwise hide behind a slow cell. A connection still open after the idle timeout is about
# to idle out, so it fails the same way.
check_idle_outs() {
    local pub="$1" from="$2" deadline=$((SECONDS + 15)) id reason role failed=0 connections
    # Plain grep, not -q: under pipefail an early exit fails the pipeline on SIGPIPE.
    while relay_connections "$from" | grep ' open$' >/dev/null && ((SECONDS < deadline)); do
        sleep 0.2
    done
    connections=$(relay_connections "$from")
    if [[ -z "$connections" ]]; then
        echo "  FAIL  $pub round: relay log contains no connections"
        overall=1
        return
    fi
    while read -r id reason; do
        [[ "$reason" == "open" || "$reason" == *"timed out"* ]] || continue
        # The relay subscribes or fetches upstream only on the publisher's connection.
        role=subscriber
        relay_log_since "$from" | grep -E "conn\{id=$id .*::subscriber: (subscribe|fetch) started" >/dev/null && role=publisher
        echo "  FAIL  $pub round: relay connection $id ($role) idled out instead of closing ($reason)"
        relay_log_since "$from" | grep -E "conn\{id=$id |connection closed id=$id " | sed 's/^/        /'
        failed=1
    done <<<"$connections"
    ((failed == 0)) || overall=1
}
