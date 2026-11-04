# Socket pairs use the relay and signed endpoint grants prepared by interop.sh.
# shellcheck shell=bash
# shellcheck disable=SC2034 # overall is the interop runner's shared verdict.

# shellcheck disable=SC2329 # called by harness_spawn
run_socket_client() {
    local runtime="$1"
    shift
    case "$runtime" in
        rust) timeout -k 3 "$TIMEOUT" "$EXAMPLES/interop-socket" "$@" ;;
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
    local dialer="$1" acceptor="$2" protocol="$3" version="$4" started=$SECONDS from pid dial_pid ready
    local name="socket-$protocol-$dialer-$acceptor-$version" prefix="socket-$protocol-$dialer-$acceptor-$version-$$-$RANDOM"
    local events="$LOGS/socket-events" ack="$LOGS/socket-ack"
    local cell=(case="socket $protocol" publisher="$dialer" subscriber="$acceptor" version="$version" log="$LOGS/$name-dial.log")
    if is_broken "$dialer" || is_broken "$acceptor"; then
        record fail "${cell[@]}" detail="client unavailable"
        overall=1
        return
    fi
    mkfifo "$events" "$ack"
    exec 8<>"$events" 9<>"$ack"
    from=$(relay_next_line)
    # Grant each peer only its own publication and the shared socket subtree.
    harness_spawn "$name-accept" "$LOGS/$name-accept.log" run_socket_acceptor "$events" "$ack" "$acceptor" accept \
        "$(token_url --root "$prefix" --publish '.socket/acceptor' --subscribe '.socket/**')" "$prefix" "$protocol" "$version"
    pid="$HARNESS_PID"
    if IFS= read -r -t "$TIMEOUT" ready <&8 && [[ "$ready" == 'socket ready' ]]; then
        harness_spawn "$name-dial" "$LOGS/$name-dial.log" run_socket_client "$dialer" dial \
            "$(token_url --root "$prefix" --publish '.socket/dialer' --subscribe '.socket/**')" "$prefix" "$protocol" "$version"
        dial_pid="$HARNESS_PID"
        harness_wait "$dial_pid" || true
        if grep -qx "socket verified $protocol dial" "$LOGS/$name-dial.log"; then
            printf 'socket drained\n' >&9
            harness_wait "$pid" || true
            if grep -qx "socket verified $protocol accept" "$LOGS/$name-accept.log" && check_versions "$name" "$from" "$version"; then
                echo "  PASS  socket $protocol $dialer -> $acceptor $version"
                record pass "${cell[@]}" seconds=$((SECONDS - started))
                exec 8>&- 9>&-
                rm "$events" "$ack"
                return
            fi
        fi
    fi
    echo "  FAIL  socket $protocol $dialer -> $acceptor $version"
    record fail "${cell[@]}" seconds=$((SECONDS - started)) detail="socket handshake, payload, or FIN failed"
    overall=1
    harness_reap "$pid"
    cat "$LOGS/$name-accept.log"
    [[ ! -f "$LOGS/$name-dial.log" ]] || cat "$LOGS/$name-dial.log"
    exec 8>&- 9>&-
    rm "$events" "$ack"
}

run_sockets() {
    local runtime version protocol
    echo '=== sockets ==='
    for runtime in js-native-node js-native-bun; do
        needs "$runtime" || continue
        for version in moq-lite-07-wip moq-transport-18 moq-transport-20 moq-transport-22; do
            for protocol in tcp track; do
                run_socket_pair rust "$runtime" "$protocol" "$version"
                run_socket_pair "$runtime" rust "$protocol" "$version"
            done
        done
    done
}
