# Socket pairs use the relay and signed endpoint grants prepared by interop.sh.
# shellcheck shell=bash
# shellcheck disable=SC2034 # overall is the interop runner's shared verdict.

# shellcheck disable=SC2329 # called by harness_spawn
run_socket_client() {
    local runtime="$1"
    shift
    case "$runtime" in
        rust) timeout -k 3 "$TIMEOUT" "$EXAMPLES/interop-socket-js" "$@" ;;
        js-native-node) (cd "$CLIENTS/js-native" && timeout -k 3 "$TIMEOUT" node --import tsx socket.ts "$@") ;;
        js-native-bun) (cd "$CLIENTS/js-native" && timeout -k 3 "$TIMEOUT" bun socket.ts "$@") ;;
    esac
}

# shellcheck disable=SC2329 # called by harness_spawn
run_socket_acceptor() {
    local events="$1" ack="$2"
    shift 2
    run_socket_client "$@" <"$ack" >"$events" 8>&- 9>&-
}

run_socket_pair() {
    local dialer="$1" acceptor="$2" version="$3" started=$SECONDS from pid dial_pid ready
    local name="socket-$dialer-$acceptor-$version" prefix="socket-$dialer-$acceptor-$version-$$-$RANDOM"
    local events="$LOGS/socket-events" ack="$LOGS/socket-ack"
    local cell=(case="socket" publisher="$dialer" subscriber="$acceptor" version="$version" log="$LOGS/$name-dial.log")
    if is_broken "$dialer" || is_broken "$acceptor"; then
        record fail "${cell[@]}" detail="client unavailable"
        overall=1
        return
    fi
    mkfifo "$events" "$ack"
    exec 8<>"$events" 9<>"$ack"
    from=$(relay_next_line)
    # Grant each peer only its own endpoint and the other role's endpoints.
    harness_spawn "$name-accept" "$LOGS/$name-accept.log" run_socket_acceptor "$events" "$ack" "$acceptor" accept \
        "$(token_url --root "$prefix" --publish '.server/acceptor' --subscribe '.client/**')" "$prefix" "$version"
    pid="$HARNESS_PID"
    if IFS= read -r -t "$TIMEOUT" ready <&8 && [[ "$ready" == 'socket ready' ]]; then
        harness_spawn "$name-dial" "$LOGS/$name-dial.log" run_socket_client "$dialer" dial \
            "$(token_url --root "$prefix" --publish '.client/dialer' --subscribe '.server/**')" "$prefix" "$version"
        dial_pid="$HARNESS_PID"
        harness_wait "$dial_pid" || true
        if grep -qx "socket verified dial" "$LOGS/$name-dial.log"; then
            printf 'socket drained\n' >&9
            harness_wait "$pid" || true
            if grep -qx "socket verified accept" "$LOGS/$name-accept.log" && check_versions "$name" "$from" "$version"; then
                echo "  PASS  socket $dialer -> $acceptor $version"
                record pass "${cell[@]}" seconds=$((SECONDS - started))
                exec 8>&- 9>&-
                rm "$events" "$ack"
                return
            fi
        fi
    fi
    echo "  FAIL  socket $dialer -> $acceptor $version"
    record fail "${cell[@]}" seconds=$((SECONDS - started)) detail="socket handshake, payload, or FIN failed"
    overall=1
    harness_reap "$pid"
    cat "$LOGS/$name-accept.log"
    [[ ! -f "$LOGS/$name-dial.log" ]] || cat "$LOGS/$name-dial.log"
    exec 8>&- 9>&-
    rm "$events" "$ack"
}

socket_pairs() {
    local action="$1" runtime
    local version=moq-lite-07-wip
    for runtime in js-native-node js-native-bun; do
        needs "$runtime" || continue
        "$action" rust "$runtime" "$version"
        "$action" "$runtime" rust "$version"
    done
}

# shellcheck disable=SC2329 # called by socket_pairs
plan_socket_pair() {
    record "not run" case="socket" publisher="$1" subscriber="$2" version="$3"
}

plan_js_sockets() {
    socket_pairs plan_socket_pair
}

run_js_sockets() {
    echo '=== sockets ==='
    socket_pairs run_socket_pair
}
