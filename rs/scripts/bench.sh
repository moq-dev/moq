#!/usr/bin/env bash
set -euo pipefail

# Runs every Criterion target plus two local relay workloads. With a base commit,
# Criterion compares matching cases directly and the relay summary prints the
# base/current delta. The current load generator drives both relay binaries so a
# generator change cannot make the two sides different workloads.

BASE=${1:-}
ROOT=$(git rev-parse --show-toplevel)
TARGET=${CARGO_TARGET_DIR:-$ROOT/target/bench}
if [[ $TARGET != /* ]]; then
    TARGET=$ROOT/$TARGET
fi

RUN=$(mktemp -d "${TMPDIR:-/tmp}/moq-bench.XXXXXX")
WORKTREE=
RELAY_PID=
HOST_PID=

cleanup() {
    if [[ -n $HOST_PID ]]; then
        kill "$HOST_PID" 2>/dev/null || true
        wait "$HOST_PID" 2>/dev/null || true
    fi
    if [[ -n $RELAY_PID ]]; then
        kill "$RELAY_PID" 2>/dev/null || true
        wait "$RELAY_PID" 2>/dev/null || true
    fi
    if [[ -n $WORKTREE ]]; then
        git -C "$ROOT" worktree remove --force "$WORKTREE" >/dev/null 2>&1 || true
    fi
    rm -rf -- "$RUN"
}
trap cleanup EXIT INT TERM

export CARGO_TARGET_DIR=$TARGET
export CRITERION_HOME=$RUN/criterion

# Shorter than Criterion's defaults, but still enough samples for confidence
# intervals and outlier detection. Comparisons run both revisions on one host.
CRITERION_ARGS=(
    --warm-up-time 1
    --measurement-time 2
    --sample-size 30
    --noplot
)

criterion_targets() {
    local checkout=$1
    (
        cd "$checkout" || exit 1
        cargo metadata --locked --format-version 1 --no-deps |
            jq -r '
                .packages[] as $package
                | $package.targets[]
                | select(any(.kind[]; . == "bench"))
                | [$package.name, .name]
                | @tsv
            ' |
            sort
    )
}

run_criterion() {
    local checkout=$1
    local package target
    local target_list=$RUN/criterion-targets
    local -a targets
    shift
    if ! criterion_targets "$checkout" >"$target_list"; then
        return 1
    fi
    targets=()
    while IFS=$'\t' read -r package target; do
        targets+=(--package "$package" --bench "$target")
    done <"$target_list"
    if ((${#targets[@]} == 0)); then
        printf 'no Criterion benchmark targets found\n' >&2
        return 1
    fi
    (
        cd "$checkout" || exit 1
        cargo bench --locked "${targets[@]}" -- "${CRITERION_ARGS[@]}" "$@"
    )
}

run_criterion_target() {
    local checkout=$1
    local package=$2
    local target=$3
    shift 3
    (
        cd "$checkout" || exit 1
        cargo bench --locked --package "$package" --bench "$target" -- \
            "${CRITERION_ARGS[@]}" "$@"
    )
}

criterion_cases() {
    local checkout=$1
    local package target benchmark
    local listing=$RUN/criterion-listing
    local target_list=$RUN/criterion-targets

    if ! criterion_targets "$checkout" >"$target_list"; then
        return 1
    fi
    if [[ ! -s $target_list ]]; then
        printf 'no Criterion benchmark targets found\n' >&2
        return 1
    fi
    while IFS=$'\t' read -r package target; do
        if ! (
            cd "$checkout" || exit 1
            cargo bench --locked --package "$package" --bench "$target" -- --list
        ) | sed -n 's/: benchmark$//p' >"$listing"; then
            return 1
        fi
        if [[ ! -s $listing ]]; then
            printf 'Criterion target has no benchmark cases: %s/%s\n' "$package" "$target" >&2
            return 1
        fi
        while IFS= read -r benchmark; do
            printf '%s\t%s\t%s\n' "$package" "$target" "$benchmark"
        done <"$listing"
    done <"$target_list"
}

compare_criterion() {
    local base_checkout=$1
    local current_checkout=$2
    local base_cases=$RUN/criterion-base.cases
    local current_cases=$RUN/criterion-current.cases
    local keys=$RUN/criterion-case.keys
    local package target benchmark base_match current_match

    criterion_cases "$base_checkout" >"$base_cases"
    criterion_cases "$current_checkout" >"$current_cases"
    sort -u "$base_cases" "$current_cases" >"$keys"

    while IFS=$'\t' read -r package target benchmark; do
        base_match=$(awk -F '\t' -v package="$package" -v target="$target" -v benchmark="$benchmark" \
            '$1 == package && $2 == target && $3 == benchmark { print; exit }' "$base_cases")
        current_match=$(awk -F '\t' -v package="$package" -v target="$target" -v benchmark="$benchmark" \
            '$1 == package && $2 == target && $3 == benchmark { print; exit }' "$current_cases")

        if [[ -n $base_match ]]; then
            printf '\nCriterion baseline: %s\n' "$benchmark"
            run_criterion_target "$base_checkout" "$package" "$target" \
                --save-baseline base "$benchmark" --exact
        fi
        if [[ -n $current_match ]]; then
            printf '\nCriterion current: %s\n' "$benchmark"
            if [[ -n $base_match ]]; then
                run_criterion_target "$current_checkout" "$package" "$target" \
                    --baseline-lenient base "$benchmark" --exact
            else
                run_criterion_target "$current_checkout" "$package" "$target" \
                    --discard-baseline "$benchmark" --exact
            fi
        fi
    done <"$keys"
}

collect_ids() {
    local sample=$1
    while IFS= read -r -d '' benchmark; do
        jq -r .full_id "$benchmark"
    done < <(find "$CRITERION_HOME" -type f -path "*/$sample/benchmark.json" -print0) |
        sort -u
}

report_set_changes() {
    local base_ids=$RUN/criterion-base.ids
    local current_ids=$RUN/criterion-current.ids
    collect_ids base >"$base_ids"
    collect_ids new >"$current_ids"

    local new removed
    new=$(comm -13 "$base_ids" "$current_ids")
    removed=$(comm -23 "$base_ids" "$current_ids")

    if [[ -n $new ]]; then
        printf '\nNEW benchmarks (no base sample):\n%s\n' "$new"
    fi
    if [[ -n $removed ]]; then
        printf '\nREMOVED benchmarks (no current sample):\n%s\n' "$removed"
    fi
}

relay_config() {
    local path=$1
    local port=$2
    printf '%s\n' \
        '[log]' \
        'level = "warn"' \
        '' \
        '[server]' \
        "bind = \"127.0.0.1:$port\"" \
        'tls.generate = ["localhost"]' \
        '' \
        '[web.http]' \
        "listen = \"127.0.0.1:$port\"" \
        '' \
        '[auth]' \
        'public = ""' >"$path"
}

start_relay() {
    local relay=$1
    local directory=$2
    local attempt port config log

    for attempt in {1..5}; do
        port=$((40000 + (RANDOM % 20000)))
        config=$directory/relay-$attempt.toml
        log=$directory/relay-$attempt.log
        relay_config "$config" "$port"

        RUST_LOG=warn "$relay" "$config" >"$log" 2>&1 &
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
		elif $last.groups_present > $last.groups_expected then error("load benchmark group counts are invalid")
		elif $last.latency_p50_ms == null or $last.latency_p99_ms == null
			or $last.latency_p50_ms < 0 or $last.latency_p99_ms < $last.latency_p50_ms
			then error("load benchmark latency sample is invalid")
		else {
			workload: $workload,
			send_mbps: (($last.bytes_sent - $first.bytes_sent) * 8 / $seconds / 1000000),
			recv_mbps: (($last.bytes_recv - $first.bytes_recv) * 8 / $seconds / 1000000),
			send_fps: (($last.frames_sent - $first.frames_sent) / $seconds),
			recv_fps: (($last.frames_recv - $first.frames_recv) / $seconds),
			loss_pct: (if $last.groups_expected == 0 then 0 else
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
			then error("host benchmark counters moved backwards")
		else {
			cpu_cores: ((($last.cpu_user + $last.cpu_system) - ($first.cpu_user + $first.cpu_system)) / $seconds),
			rss_bytes: ($window | map(.rss_bytes) | max)
		}
		end
	' "$host" | jq -s '.[0] * .[1]' "$summary" - >"$enriched"
    mv "$enriched" "$summary"
}

run_workload() {
    local label=$1
    local relay=$2
    local workload=$3
    local directory=$RUN/relay/$label/$workload
    local stats=$directory/load.jsonl
    local host=$directory/host.jsonl
    local summary=$directory/summary.json
    local -a shape

    mkdir -p "$directory"
    start_relay "$relay" "$directory"

    if [[ $(uname -s) == Linux ]]; then
        "$HOST_BIN" --pid "$RELAY_PID" --interval 500ms --duration 10s --output "$host" \
            >"$directory/host.log" 2>&1 &
        HOST_PID=$!
    fi

    case $workload in
        video)
            shape=(
                --connections 16
                --broadcasts 1
                --subscribe 2
                --fps 30
                --frame-size 1200
                --group-size 59
            )
            ;;
        fanout)
            shape=(
                --connections 65
                --fanout fanout
                --fps 10
                --frame-size 400
                --group-size 0
            )
            ;;
        *)
            printf 'unknown relay workload: %s\n' "$workload" >&2
            return 1
            ;;
    esac

    "$LOAD_BIN" \
        --client-connect "https://localhost:$RELAY_PORT" \
        --client-tls-disable-verify \
        --name "bench-$label-$workload" \
        --startup 2s \
        --duration 10s \
        --report 500ms \
        --output "$stats" \
        "${shape[@]}" >"$directory/load.log" 2>&1

    if [[ -n $HOST_PID ]]; then
        wait "$HOST_PID"
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

print_current_relay() {
    printf '\n%-10s %11s %11s %10s %10s %9s %9s %10s %10s\n' \
        workload send-Mbps recv-Mbps send-fps recv-fps loss-% p99-ms CPU-cores RSS-MiB
    for workload in video fanout; do
        jq -r '[
			.workload,
			(.send_mbps | tostring),
			(.recv_mbps | tostring),
			(.send_fps | tostring),
			(.recv_fps | tostring),
			(.loss_pct | tostring),
			(.latency_p99_ms // "n/a" | tostring),
			(.cpu_cores // "n/a" | tostring),
			(if .rss_bytes then (.rss_bytes / 1048576 | tostring) else "n/a" end)
		] | @tsv' "$RUN/relay/current/$workload/summary.json" |
            while IFS=$'\t' read -r name send recv send_fps recv_fps loss p99 cpu rss; do
                printf '%-10s %11.3f %11.3f %10.1f %10.1f %9.3f %9s %10s %10s\n' \
                    "$name" "$send" "$recv" "$send_fps" "$recv_fps" "$loss" "$p99" "$cpu" "$rss"
            done
    done
}

print_relay_comparison() {
    printf '\nRelay comparison (change is current versus base)\n'
    printf '%-10s %-16s %12s %12s %10s\n' workload metric base current change
    for workload in video fanout; do
        jq -r -s '
			.[0] as $base | .[1] as $current
			| def change($a; $b): if $a == null or $a == 0 or $b == null then "n/a" else
				((($b / $a - 1) * 10000 | round) / 100 | tostring) + "%"
			end;
			[
				["send Mbps", $base.send_mbps, $current.send_mbps],
				["recv Mbps", $base.recv_mbps, $current.recv_mbps],
				["send fps", $base.send_fps, $current.send_fps],
				["recv fps", $base.recv_fps, $current.recv_fps],
				["loss %", $base.loss_pct, $current.loss_pct],
				["latency p50 ms", $base.latency_p50_ms, $current.latency_p50_ms],
				["latency p99 ms", $base.latency_p99_ms, $current.latency_p99_ms],
				["CPU cores", $base.cpu_cores, $current.cpu_cores],
				["RSS MiB", (($base.rss_bytes // null) | if . then . / 1048576 else null end),
					(($current.rss_bytes // null) | if . then . / 1048576 else null end)]
			]
			| .[]
			| [.[0], (.[1] // "n/a" | tostring), (.[2] // "n/a" | tostring), change(.[1]; .[2])]
			| @tsv
		' "$RUN/relay/base/$workload/summary.json" "$RUN/relay/current/$workload/summary.json" |
            while IFS=$'\t' read -r metric base current change; do
                printf '%-10s %-16s %12s %12s %10s\n' "$workload" "$metric" "$base" "$current" "$change"
            done
    done
}

cd "$ROOT"

if [[ -n $BASE ]]; then
    BASE_COMMIT=$(git rev-parse --verify "$BASE^{commit}")
    WORKTREE=$RUN/base
    git worktree add --detach "$WORKTREE" "$BASE_COMMIT" >/dev/null

    printf 'Criterion comparison: %s (%s) versus current\n' "$BASE" "$BASE_COMMIT"
    compare_criterion "$WORKTREE" "$ROOT"
    report_set_changes

    printf '\nBuilding relay binaries...\n'
    (
        cd "$WORKTREE"
        cargo build --locked --release -p moq-relay
    )
    cp "$TARGET/release/moq-relay" "$RUN/moq-relay-base"
    chmod +x "$RUN/moq-relay-base"
else
    printf 'Criterion current: %s\n' "$(git rev-parse --short HEAD)"
    run_criterion "$ROOT" --discard-baseline
    printf '\nBuilding relay binaries...\n'
fi

cargo build --locked --release -p moq-relay -p moq-bench
LOAD_BIN=$TARGET/release/moq-bench
HOST_BIN=$TARGET/release/moq-bench-host

if [[ -n $BASE ]]; then
    printf '\nRelay workloads: paired base/current\n'
    for workload in video fanout; do
        run_workload base "$RUN/moq-relay-base" "$workload"
        run_workload current "$TARGET/release/moq-relay" "$workload"
    done
else
    run_relay_suite current "$TARGET/release/moq-relay"
fi

if [[ -n $BASE ]]; then
    print_relay_comparison
else
    print_current_relay
fi
