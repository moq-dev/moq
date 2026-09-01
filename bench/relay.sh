#!/usr/bin/env bash

# Sourced by run.sh, which owns these process and output paths.
# shellcheck disable=SC2153,SC2154

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
        '[server]' \
        "bind = \"127.0.0.1:$port\"" \
        "${tls[@]}" \
        '' \
        '[web.http]' \
        "listen = \"127.0.0.1:$port\"" \
        '' \
        '[auth]' \
        'public = ""' \
        "${runtime_config[@]}" >"$path"
}

start_relay() {
    local relay=$1
    local directory=$2
    local runtime=${3:-}
    local workers=${4:-1}
    local attempt port config log tokio_threads

    case $runtime in
        tokio-shared)
            tokio_threads=$workers
            ;;
        tokio-workers | io-uring-workers)
            tokio_threads=1
            ;;
        '')
            tokio_threads=
            ;;
    esac

    for attempt in {1..5}; do
        port=$((40000 + (RANDOM % 20000)))
        config=$directory/relay-$attempt.toml
        log=$directory/relay-$attempt.log
        relay_config "$config" "$port" "$runtime" "$workers"

        if [[ -n $tokio_threads ]]; then
            env TOKIO_WORKER_THREADS="$tokio_threads" RUST_LOG=warn \
                "$relay" "$config" >"$log" 2>&1 &
        else
            RUST_LOG=warn "$relay" "$config" >"$log" 2>&1 &
        fi
        RELAY_PID=$!

        for _ in {1..100}; do
            if ! kill -0 "$RELAY_PID" 2>/dev/null; then
                wait "$RELAY_PID" 2>/dev/null || true
                RELAY_PID=
                break
            fi
            if (: <>"/dev/tcp/127.0.0.1/$port") 2>/dev/null; then
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

    # The counters are sampled independently, so mirror moq-bench's saturating
    # subtraction when a snapshot catches one update between the two loads.
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
			send_mbps: (($last.bytes_sent - $first.bytes_sent) * 8 / $seconds / 1000000),
			recv_mbps: (($last.bytes_recv - $first.bytes_recv) * 8 / $seconds / 1000000),
			send_fps: (($last.frames_sent - $first.frames_sent) / $seconds),
			recv_fps: (($last.frames_recv - $first.frames_recv) / $seconds),
			loss_pct: (if $last.groups_expected == 0 or $last.groups_present >= $last.groups_expected then 0 else
				(($last.groups_expected - $last.groups_present) * 100 / $last.groups_expected)
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
    local enriched=$summary.enriched

    jq -s '
		if length < 2 then error("host benchmark produced fewer than two samples") else . end
		| .[-1] as $last
		| ($last.timestamp_ms - 5000) as $floor
		| map(select(.timestamp_ms >= $floor)) as $window
		| $window[0] as $first
		| $window[-1] as $last
		| (($last.timestamp_ms - $first.timestamp_ms) / 1000) as $seconds
		| if $seconds <= 0 then error("host benchmark time did not advance")
		elif $last.cpu_user < $first.cpu_user or $last.cpu_system < $first.cpu_system
			or $last.ctx_voluntary < $first.ctx_voluntary
			or $last.ctx_involuntary < $first.ctx_involuntary
			then error("host benchmark counters moved backwards")
		else {
			cpu_user_cores: (($last.cpu_user - $first.cpu_user) / $seconds),
			cpu_system_cores: (($last.cpu_system - $first.cpu_system) / $seconds),
			cpu_cores: ((($last.cpu_user + $last.cpu_system) - ($first.cpu_user + $first.cpu_system)) / $seconds),
			ctx_voluntary_s: (($last.ctx_voluntary - $first.ctx_voluntary) / $seconds),
			ctx_involuntary_s: (($last.ctx_involuntary - $first.ctx_involuntary) / $seconds),
			rss_bytes: ($window | map(.rss_bytes) | max),
			threads: ($window | map(.threads) | max)
		}
		end
	' "$host" | jq -s '.[0] * .[1]' "$summary" - >"$enriched"
    mv "$enriched" "$summary"
}

run_workload() {
    local label=$1
    local relay=$2
    local workload=$3
    local runtime=${4:-}
    local workers=${5:-1}
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

    if [[ $(uname -s) == Linux ]]; then
        "$HOST_BIN" --pid "$RELAY_PID" --interval 500ms --duration 10s --output "$host" \
            >"$directory/host.log" 2>&1 &
        HOST_PID=$!
    fi

    if ! "$LOAD_BIN" \
        --file "$config" \
        --connect "https://localhost:$RELAY_PORT" \
        --connect-tls-insecure \
        --name "bench-$label-$workload" \
        --startup 2s \
        --duration 10s \
        --report 500ms \
        --output "$stats" >"$directory/load.log" 2>&1; then
        printf 'load benchmark failed: %s/%s\n' "$label" "$workload" >&2
        cat "$directory/load.log" >&2
        return 1
    fi

    if [[ -n $HOST_PID ]]; then
        if ! wait "$HOST_PID"; then
            printf 'host benchmark failed: %s/%s\n' "$label" "$workload" >&2
            cat "$directory/host.log" >&2
            HOST_PID=
            return 1
        fi
        HOST_PID=
    fi
    stop_relay

    summarize_load "$stats" "$summary" "$workload"
    if [[ -s $host ]]; then
        add_host_summary "$host" "$summary"
    fi
}

run_relay_suite() {
    local label=$1
    local relay=$2
    printf '\nRelay workloads: %s\n' "$label"
    run_workload "$label" "$relay" video
    run_workload "$label" "$relay" fanout
}
