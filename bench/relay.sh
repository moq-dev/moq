#!/usr/bin/env bash

# Sourced by run.sh, which owns these process and output paths.
# shellcheck disable=SC2153,SC2154

# Set to a profile.py mode (lock, cpu, heap) to profile each workload's relay over its steady window.
PROFILE=

relay_config() {
    local path=$1
    local port=$2
    local runtime=${3:-}
    local workers=${4:-1}
    local -a tls runtime_config

    if [[ -n $runtime ]]; then
        tls=(
            "tls.cert = [\"$RUN/localhost.crt\"]"
            "tls.key = [\"$RUN/localhost.key\"]"
        )
    else
        tls=('tls.generate = ["localhost"]')
    fi

    case $runtime in
        '' | tokio-shared)
            runtime_config=()
            ;;
        tokio-workers)
            runtime_config=(
                ''
                '[runtime]'
                "workers = $workers"
                'pin = false'
            )
            ;;
        io-uring-workers)
            runtime_config=(
                ''
                '[runtime]'
                "workers = $workers"
                'pin = false'
                'io_uring = true'
            )
            ;;
        *)
            printf 'unknown relay runtime: %s\n' "$runtime" >&2
            return 1
            ;;
    esac

    printf '%s\n' \
        '[log]' \
        'level = "warn"' \
        '' \
        '[listen]' \
        "bind = \"127.0.0.1:$port\"" \
        "${tls[@]}" \
        '' \
        '[web.http]' \
        "listen = \"127.0.0.1:$port\"" \
        '' \
        '[auth]' \
        'public = "**"' \
        "${runtime_config[@]}" >"$path"
    if [[ $PROFILE == heap ]]; then
        # Its /sessions list shows when the relay has dropped every session after the load.
        printf '%s\n' '' '[internal]' "listen = \"127.0.0.1:$((port + 1))\"" >>"$path"
    fi
}

# A connect can succeed against somebody else's listener while our child is alive.
# Check both sockets on the actual relay PID before treating that probe as readiness.
relay_owns_port() {
    local pid=$1 port=$2
    lsof -nP -a -p "$pid" -iTCP:"$port" -sTCP:LISTEN >/dev/null 2>&1 &&
        lsof -nP -a -p "$pid" -iUDP:"$port" >/dev/null 2>&1
}

start_relay() {
    local relay=$1
    local directory=$2
    local runtime=${3:-}
    local workers=${4:-1}
    local attempt port config log
    local -a environment=(RUST_LOG=warn)

    case $runtime in
        tokio-shared)
            environment+=("TOKIO_WORKER_THREADS=$workers")
            ;;
        tokio-workers | io-uring-workers)
            environment+=(TOKIO_WORKER_THREADS=1)
            ;;
    esac
    if [[ $PROFILE == heap ]]; then
        # Sample from startup, so the first snapshot holds the idle heap, and accumulate
        # allocations so two snapshots bound the churn between them. One sample per 64 KiB
        # allocated, not jemalloc's 512 KiB default, gives a 5 s window enough stacks.
        mkdir -p "$directory/heap"
        environment+=("MALLOC_CONF=prof:true,prof_active:true,prof_accum:true,lg_prof_sample:16,prof_prefix:$directory/heap/jeprof")
    fi

    if ! command -v lsof >/dev/null; then
        printf 'relay socket ownership requires lsof; run through nix develop\n' >&2
        return 1
    fi

    for attempt in {1..5}; do
        port=$((40000 + (RANDOM % 20000)))
        config=$directory/relay-$attempt.toml
        log=$directory/relay-$attempt.log
        relay_config "$config" "$port" "$runtime" "$workers"

        env "${environment[@]}" "$relay" "$config" >"$log" 2>&1 &
        RELAY_PID=$!

        for _ in {1..100}; do
            if ! kill -0 "$RELAY_PID" 2>/dev/null; then
                wait "$RELAY_PID" 2>/dev/null || true
                RELAY_PID=
                break
            fi
            if (: <>"/dev/tcp/127.0.0.1/$port") 2>/dev/null && relay_owns_port "$RELAY_PID" "$port"; then
                RELAY_PORT=$port
                return 0
            fi
            sleep 0.1
        done

        if [[ -n $RELAY_PID ]]; then
            kill "$RELAY_PID" 2>/dev/null || true
            wait "$RELAY_PID" 2>/dev/null || true
            RELAY_PID=
        fi
    done

    printf 'relay failed to start:\n' >&2
    cat "$log" >&2
    return 1
}

stop_relay() {
    if [[ -z $RELAY_PID ]]; then
        return 0
    fi

    local status=0
    local signaled=false
    if kill -0 "$RELAY_PID" 2>/dev/null && kill "$RELAY_PID" 2>/dev/null; then
        signaled=true
    fi
    wait "$RELAY_PID" 2>/dev/null || status=$?
    RELAY_PID=

    if [[ $signaled == true ]] && ((status == 0 || status == 143)); then
        return 0
    fi

    printf 'relay exited during workload (status %d)\n' "$status" >&2
    return 1
}

summarize_load() {
    local stats=$1
    local output=$2
    local workload=$3

    # Confirmed losses are monotonic; outstanding gaps can recover and hide loss.
    jq -s --arg workload "$workload" '
		if length < 2 then error("load benchmark produced fewer than two samples") else . end
		| .[-1] as $last
		| ($last.timestamp_ms - 5000) as $floor
		| map(select(.timestamp_ms >= $floor))
		| .[0] as $first
		| .[-1] as $last
		| (($last.timestamp_ms - $first.timestamp_ms) / 1000) as $seconds
		| if $seconds <= 0 then error("load benchmark time did not advance")
		elif $last.frames_recv <= $first.frames_recv then error("load benchmark delivered zero frames")
		elif $last.bytes_sent < $first.bytes_sent or $last.bytes_recv < $first.bytes_recv
			then error("load benchmark counters moved backwards")
		elif $last.latency_p50_ms == null or $last.latency_p99_ms == null
			or $last.latency_p50_ms < 0 or $last.latency_p99_ms < $last.latency_p50_ms
			then error("load benchmark latency sample is invalid")
		else {
			workload: $workload,
			throughput_window_start_ms: $first.timestamp_ms,
			throughput_window_end_ms: $last.timestamp_ms,
			latency_scope: "cumulative lifetime",
			loss_scope: "cumulative lifetime",
			send_mbps: (($last.bytes_sent - $first.bytes_sent) * 8 / $seconds / 1000000),
			recv_mbps: (($last.bytes_recv - $first.bytes_recv) * 8 / $seconds / 1000000),
			send_fps: (($last.frames_sent - $first.frames_sent) / $seconds),
			recv_fps: (($last.frames_recv - $first.frames_recv) / $seconds),
			loss_pct: (if $last.groups_expected == 0 then 0 else
				($last.groups_lost * 100 / $last.groups_expected)
			end),
			latency_p50_ms: $last.latency_p50_ms,
			latency_p99_ms: $last.latency_p99_ms
		}
		end
	' "$stats" >"$output"
}

add_host_summary() {
    local host=$1
    local summary=$2
    local relay=$3
    local -a generator=("${@:4}")
    python3 "${ROOT:-.}/bench/summary.py" host "$host" "$summary" "$relay" "${generator[@]}"

}

run_workload() {
    local label=$1
    local relay=$2
    local workload=$3
    local runtime=${4:-}
    local workers=${5:-1}
    local -a overrides=("${@:6}")
    local directory=$RUN/relay/$label/$workload
    local stats=$directory/load.jsonl
    local host=$directory/host.jsonl
    local summary=$directory/summary.json
    local config=$ROOT/bench/workloads/$workload.toml

    if [[ ! -f $config ]]; then
        printf 'unknown relay workload: %s\n' "$workload" >&2
        return 1
    fi

    mkdir -p "$directory"
    start_relay "$relay" "$directory" "$runtime" "$workers"

    local relay_pid=$RELAY_PID generator_pid='' finished
    local profile=$directory/$PROFILE
    local -a load=("$LOAD_BIN" --file "$config"
        --connect "https://localhost:$RELAY_PORT" --connect-tls-insecure
        --name "bench-$label-$workload" --startup 2s --report 500ms
        --output "$stats" "${overrides[@]}")

    if [[ $PROFILE == heap ]]; then
        python3 "$ROOT/bench/profile.py" snapshot "$relay_pid" "$profile" before
    fi
    if [[ $(uname -s) == Linux ]]; then
        # The sampler owns the measurement lifetime. The generator runs until
        # interrupted after sampling, so its normal exit cannot race /proc reads.
        "${load[@]}" >"$directory/load.log" 2>&1 &
        LOAD_PID=$!
        generator_pid=$LOAD_PID
        "$HOST_BIN" --pid "$relay_pid" --pid "$generator_pid" --threads \
            --interval 500ms --duration 10s --output "$host" \
            >"$directory/host.log" 2>&1 &
        HOST_PID=$!
        # The second half of the host window, matching the summary's throughput window.
        # Its own process group, so cleanup also stops perf and its workload.
        case $PROFILE in
            lock | cpu)
                setsid python3 "$ROOT/bench/profile.py" "$PROFILE" "$relay_pid" "$profile" --delay 5 --duration 5 \
                    >"$directory/profile.log" 2>&1 &
                PROFILE_PID=$!
                ;;
            heap)
                setsid python3 "$ROOT/bench/profile.py" snapshot "$relay_pid" "$profile" steady-start steady-end \
                    --delay 5 --interval 5 >"$directory/profile.log" 2>&1 &
                PROFILE_PID=$!
                ;;
        esac
        if ! wait -n -p finished "$HOST_PID" "$LOAD_PID"; then
            cat "$directory/host.log" "$directory/load.log" >&2
            return 1
        fi
        if [[ $finished != "$HOST_PID" ]]; then
            printf 'load generator exited before sampling completed\n' >&2
            return 1
        fi
        HOST_PID=
        if [[ -n $PROFILE ]]; then
            if ! wait "$PROFILE_PID"; then
                cat "$directory/profile.log" >&2
                return 1
            fi
            PROFILE_PID=
        fi
        kill -INT "$LOAD_PID"
        if ! wait "$LOAD_PID"; then
            cat "$directory/load.log" >&2
            return 1
        fi
        LOAD_PID=
    elif ! "${load[@]}" --duration 10s >"$directory/load.log" 2>&1; then
        cat "$directory/load.log" >&2
        return 1
    fi
    if [[ $PROFILE == heap ]]; then
        python3 "$ROOT/bench/profile.py" snapshot "$relay_pid" "$profile" teardown \
            --settled "http://127.0.0.1:$((RELAY_PORT + 1))/sessions"
    fi
    stop_relay
    if [[ $PROFILE == heap ]]; then
        python3 "$ROOT/bench/profile.py" heap "$relay" "$profile"
    fi

    if [[ -s $host ]]; then
        python3 "$ROOT/bench/summary.py" align "$host" "$stats" "$directory/load-window.jsonl"
        summarize_load "$directory/load-window.jsonl" "$summary" "$workload"
        add_host_summary "$host" "$summary" "$relay_pid" "$generator_pid"
    else
        summarize_load "$stats" "$summary" "$workload"
    fi

}

run_relay_suite() {
    local label=$1
    local relay=$2
    printf '\nRelay workloads: %s\n' "$label"
    run_workload "$label" "$relay" video
    run_workload "$label" "$relay" fanout
}
