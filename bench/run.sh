#!/usr/bin/env bash
set -euo pipefail

# Runs every Criterion target plus two local relay workloads. With a base commit,
# Criterion compares matching cases directly and the relay summary prints the
# base/current delta. `--runtime` instead compares one multi-threaded Tokio
# runtime with the same number of independent Tokio/epoll and io_uring workers
# on the current tree. The current load generator drives every relay so the
# workloads stay identical. `--audio` isolates the audio grouping/load-shape matrix.
# `--lock` records relay worker off-CPU stacks to attribute std mutex waits.
# `--shard` sweeps the QUIC shard count against the connection count.

MODE=${1:-}
ROUNDS=${MOQ_BENCH_RUNTIME_ROUNDS:-3}
if [[ ! $ROUNDS =~ ^[0-9]+$ ]] || ((10#$ROUNDS < 1)); then
    printf 'MOQ_BENCH_RUNTIME_ROUNDS must be a positive integer, got %q\n' "$ROUNDS" >&2
    exit 1
fi
ROUNDS=$((10#$ROUNDS))
case $MODE in
    --audio)
        BASE=
        ;;
    --lock)
        BASE=
        LOCK_WORKLOAD=${MOQ_BENCH_LOCK_WORKLOAD:-fanout}
        LOCK_WORKERS=${MOQ_BENCH_LOCK_WORKERS:-1 4 16}
        # Without frame pointers, perf's DWARF unwinder stops at the first relay frame.
        export RUSTFLAGS="${RUSTFLAGS:+$RUSTFLAGS }-C force-frame-pointers=yes"
        ;;
    --shard)
        BASE=
        SHARD_COUNTS=${MOQ_BENCH_SHARD_COUNTS:-0 1 4 16}
        SHARD_CONNECTIONS=${MOQ_BENCH_SHARD_CONNECTIONS:-16 64 256}
        for value in $SHARD_COUNTS $SHARD_CONNECTIONS; do
            if [[ ! $value =~ ^[0-9]+$ ]]; then
                printf 'shard benchmark counts must be integers, got %q\n' "$value" >&2
                exit 1
            fi
        done
        ;;
    --runtime)
        BASE=
        RUNTIME_WORKERS=${MOQ_BENCH_RUNTIME_WORKERS:-}
        if [[ -z $RUNTIME_WORKERS ]]; then
            RUNTIME_WORKERS=$(getconf _NPROCESSORS_ONLN)
            if ((RUNTIME_WORKERS > 256)); then
                RUNTIME_WORKERS=256
            fi
        fi
        if [[ ! $RUNTIME_WORKERS =~ ^[0-9]+$ ]] || ((10#$RUNTIME_WORKERS < 1 || 10#$RUNTIME_WORKERS > 256)); then
            printf 'runtime benchmark workers must be an integer from 1 to 256, got %q\n' "$RUNTIME_WORKERS" >&2
            exit 1
        fi
        RUNTIME_WORKERS=$((10#$RUNTIME_WORKERS))
        ;;
    *)
        BASE=$MODE
        ;;
esac
ROOT=$(git rev-parse --show-toplevel)
TARGET=${CARGO_TARGET_DIR:-$ROOT/target/bench}
if [[ $TARGET != /* ]]; then
    TARGET=$ROOT/$TARGET
fi
BASE_TARGET=$TARGET/base
CURRENT_TARGET=$TARGET/current

ARTIFACTS=${MOQ_BENCH_ARTIFACTS:-$TARGET/runs}
mkdir -p "$ARTIFACTS"
ARTIFACTS=$(cd "$ARTIFACTS" && pwd -P)
# Each run copies bench/ and untracked sources, so it must land in neither.
if [[ $ARTIFACTS/ == "$ROOT"/bench/* ]] ||
    { [[ $ARTIFACTS/ == "$ROOT"/* ]] && ! git -C "$ROOT" check-ignore -q "$ARTIFACTS"; }; then
    printf 'MOQ_BENCH_ARTIFACTS must be outside the repository, or git-ignored and outside bench/: %s\n' \
        "$ARTIFACTS" >&2
    exit 1
fi
RUN=$(mktemp -d "$ARTIFACTS/run.XXXXXX")
printf 'Benchmark evidence: %s\n' "$RUN"
WORKTREE=
RELAY_PID=
HOST_PID=
LOAD_PID=
PROFILE_PID=
STDOUT_PID=
STDERR_PID=

cleanup() {
    if [[ -n $PROFILE_PID ]]; then
        # setsid made the profiler a group leader without forking, so its PID is the group.
        kill -- -"$PROFILE_PID" 2>/dev/null || true
        wait "$PROFILE_PID" 2>/dev/null || true
    fi
    if [[ -n $LOAD_PID ]]; then
        kill "$LOAD_PID" 2>/dev/null || true
        wait "$LOAD_PID" 2>/dev/null || true
    fi
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
    # Close both pipe writers before waiting for the owned log copiers.
    if [[ -n $STDOUT_PID ]]; then
        exec 1>&3 2>&4
        wait "$STDOUT_PID" || true
        wait "$STDERR_PID" || true
        exec 3>&- 4>&-
    fi
    printf 'Benchmark evidence retained: %s\n' "$RUN"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
exec 3>&1 4>&2
exec > >(tee "$RUN/stdout.log")
STDOUT_PID=$!
exec 2> >(tee "$RUN/stderr.log" >&4)
STDERR_PID=$!

python3 "$ROOT/bench/metadata.py" "$RUN" "$BASE" "$MODE" "${RUNTIME_WORKERS:-}" "$ROUNDS"
cp -R "$ROOT/bench/workloads" "$RUN/workloads"
cp -R "$ROOT/bench" "$RUN/harness"

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
            jq -r --arg root "$checkout/" '
                .packages[] as $package
                | $package.targets[]
                | select(any(.kind[]; . == "bench"))
                | [($package.manifest_path | ltrimstr($root)), .name]
                | @tsv
            ' |
            sort
    )
}

target_dir() {
    local checkout=$1
    if [[ $checkout == "$ROOT" ]]; then
        printf '%s\n' "$CURRENT_TARGET"
    else
        printf '%s\n' "$BASE_TARGET"
    fi
}

run_criterion() {
    local checkout=$1
    local manifest target
    local target_list=$RUN/criterion-targets
    shift
    if ! criterion_targets "$checkout" >"$target_list"; then
        return 1
    fi
    if [[ ! -s $target_list ]]; then
        printf 'no Criterion benchmark targets found\n' >&2
        return 1
    fi
    while IFS=$'\t' read -r manifest target; do
        run_criterion_target "$checkout" "$manifest" "$target" "$@"
    done <"$target_list"
}

run_criterion_target() {
    local checkout=$1
    local manifest=$2
    local target=$3
    shift 3
    (
        cd "$checkout" || exit 1
        CARGO_TARGET_DIR=$(target_dir "$checkout") \
            cargo bench --locked --manifest-path "$manifest" --bench "$target" -- \
            "${CRITERION_ARGS[@]}" "$@"
    )
}

criterion_cases() {
    local checkout=$1
    local manifest target benchmark
    local listing=$RUN/criterion-listing
    local target_list=$RUN/criterion-targets

    if ! criterion_targets "$checkout" >"$target_list"; then
        return 1
    fi
    if [[ ! -s $target_list ]]; then
        printf 'no Criterion benchmark targets found\n' >&2
        return 1
    fi
    while IFS=$'\t' read -r manifest target; do
        if ! (
            cd "$checkout" || exit 1
            CARGO_TARGET_DIR=$(target_dir "$checkout") \
                cargo bench --locked --manifest-path "$manifest" --bench "$target" -- --list
        ) | sed -n 's/: benchmark$//p' >"$listing"; then
            return 1
        fi
        if [[ ! -s $listing ]]; then
            printf 'Criterion target has no benchmark cases: %s/%s\n' "$manifest" "$target" >&2
            return 1
        fi
        while IFS= read -r benchmark; do
            printf '%s\t%s\t%s\n' "$manifest" "$target" "$benchmark"
        done <"$listing"
    done <"$target_list"
}

compare_criterion() {
    local base_checkout=$1
    local current_checkout=$2
    local base_cases=$RUN/criterion-base.cases
    local current_cases=$RUN/criterion-current.cases
    local keys=$RUN/criterion-case.keys
    local manifest target benchmark base_match current_match

    criterion_cases "$base_checkout" >"$base_cases"
    criterion_cases "$current_checkout" >"$current_cases"
    sort -u "$base_cases" "$current_cases" >"$keys"

    while IFS=$'\t' read -r manifest target benchmark; do
        base_match=$(awk -F '\t' -v manifest="$manifest" -v target="$target" -v benchmark="$benchmark" \
            '$1 == manifest && $2 == target && $3 == benchmark { print; exit }' "$base_cases")
        current_match=$(awk -F '\t' -v manifest="$manifest" -v target="$target" -v benchmark="$benchmark" \
            '$1 == manifest && $2 == target && $3 == benchmark { print; exit }' "$current_cases")

        if [[ -n $base_match ]]; then
            printf '\nCriterion baseline: %s\n' "$benchmark"
            run_criterion_target "$base_checkout" "$manifest" "$target" \
                --save-baseline base "$benchmark" --exact
        fi
        if [[ -n $current_match ]]; then
            printf '\nCriterion current: %s\n' "$benchmark"
            if [[ -n $base_match ]]; then
                run_criterion_target "$current_checkout" "$manifest" "$target" \
                    --baseline-lenient base "$benchmark" --exact
            else
                run_criterion_target "$current_checkout" "$manifest" "$target" \
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

# shellcheck source=bench/relay.sh
source "$ROOT/bench/relay.sh"

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

aggregate_runtime() {
    local runtime=$1
    local workload=$2
    local output=$RUN/relay/$runtime/$workload.json
    local -a samples=("$RUN"/relay/"$runtime"/round-*/"$workload"/summary.json)

    jq -s --arg runtime "$runtime" --arg workload "$workload" '
		def median:
			sort as $values
			| ($values | length) as $count
			| if $count % 2 == 1 then $values[($count / 2 | floor)]
			  else (($values[$count / 2 - 1] + $values[$count / 2]) / 2)
			  end;
		{
			runtime: $runtime,
			workload: $workload,
			samples: length,
			recv_mbps: (map(.recv_mbps) | median),
			latency_p99_ms: (map(.latency_p99_ms) | median),
			loss_pct: (map(.loss_pct) | median),
			cpu_user_cores: (map(.cpu_user_cores) | median),
			cpu_system_cores: (map(.cpu_system_cores) | median),
			cpu_cores: (map(.cpu_cores) | median),
			ctx_voluntary_s: (map(.ctx_voluntary_s) | median),
			ctx_involuntary_s: (map(.ctx_involuntary_s) | median),
			rss_bytes: (map(.rss_bytes) | median),
			threads: (map(.threads) | median)
		}
	' "${samples[@]}" >"$output"
}

print_runtime_comparison() {
    local workload runtime

    for workload in video fanout video-heavy fanout-heavy; do
        for runtime in tokio-shared tokio-workers io-uring-workers; do
            aggregate_runtime "$runtime" "$workload"
        done
    done

    printf '\nRuntime comparison (%d workers, median of %d rounds)\n' "$RUNTIME_WORKERS" "$ROUNDS"
    printf 'tokio-shared uses %d runtime threads; worker modes use %d data threads plus one control thread.\n' \
        "$RUNTIME_WORKERS" "$RUNTIME_WORKERS"
    printf '%-13s %-16s %10s %8s %7s %8s %8s %9s %9s %9s %9s %7s\n' \
        workload runtime recv-Mbps p99-ms loss-% CPU user-CPU sys-CPU vol-ctx/s invol/s RSS-MiB threads
    for workload in video fanout video-heavy fanout-heavy; do
        for runtime in tokio-shared tokio-workers io-uring-workers; do
            jq -r '[
				.workload,
				.runtime,
				.recv_mbps,
				.latency_p99_ms,
				.loss_pct,
				.cpu_cores,
				.cpu_user_cores,
				.cpu_system_cores,
				.ctx_voluntary_s,
				.ctx_involuntary_s,
				(.rss_bytes / 1048576),
				.threads
			] | @tsv' "$RUN/relay/$runtime/$workload.json" |
                while IFS=$'\t' read -r name mode recv p99 loss cpu user system voluntary involuntary rss threads; do
                    printf '%-13s %-16s %10.3f %8.1f %7.3f %8.3f %8.3f %9.3f %9.1f %9.1f %9.1f %7.1f\n' \
                        "$name" "$mode" "$recv" "$p99" "$loss" "$cpu" "$user" "$system" \
                        "$voluntary" "$involuntary" "$rss" "$threads"
                done
        done
    done

    printf '\nMedian deltas (negative CPU means the second mode used less)\n'
    printf '%-13s %-29s %12s %12s %12s\n' workload comparison CPU p99-ms sys-CPU
    for workload in video fanout video-heavy fanout-heavy; do
        jq -r -s '
			def change($before; $after):
				if $before == 0 then "n/a" else
					((($after / $before - 1) * 10000 | round) / 100 | tostring) + "%"
				end;
			def comparison($before; $after):
				[
					$after.workload,
					($before.runtime + " -> " + $after.runtime),
					change($before.cpu_cores; $after.cpu_cores),
					change($before.latency_p99_ms; $after.latency_p99_ms),
					change($before.cpu_system_cores; $after.cpu_system_cores)
				] | @tsv;
			comparison(.[0]; .[1]), comparison(.[2]; .[3])
		' "$RUN/relay/tokio-shared/$workload.json" "$RUN/relay/tokio-workers/$workload.json" \
            "$RUN/relay/tokio-workers/$workload.json" "$RUN/relay/io-uring-workers/$workload.json" |
            while IFS=$'\t' read -r name comparison cpu p99 system; do
                printf '%-13s %-29s %12s %12s %12s\n' "$name" "$comparison" "$cpu" "$p99" "$system"
            done
    done
}

run_audio_comparison() {
    printf 'Building relay and audio load generator...\n'
    CARGO_TARGET_DIR=$CURRENT_TARGET cargo build --locked --release -p moq-relay -p moq-bench
    LOAD_BIN=$CURRENT_TARGET/release/moq-bench
    HOST_BIN=$CURRENT_TARGET/release/moq-bench-host

    printf '\nAudio grouping: 50 / 400 fps, 200-byte frames, per-packet / 20 / 100 / 200 ms groups\n'
    printf 'shape\trecv-fps\tp99-ms\tCPU\tRSS-MiB\n'
    local connections subscribers group_size label shape fps
    for shape in 50:0 50:4 50:9 400:0 400:7; do
        fps=${shape%:*}
        group_size=${shape#*:}
        for connections in 16 32; do
            for subscribers in 2 8; do
                label=room-$fps-$connections-$subscribers-$group_size
                run_workload "$label" "$CURRENT_TARGET/release/moq-relay" audio '' 1 \
                    --connections "$connections" --subscribe "$subscribers" --fps "$fps" --group-size "$group_size"
                print_audio_sample "$label" audio
            done
        done
        for connections in 65 201; do
            label=fanout-$fps-$connections-$group_size
            run_workload "$label" "$CURRENT_TARGET/release/moq-relay" audio-fanout '' 1 \
                --connections "$connections" --fps "$fps" --group-size "$group_size"
            print_audio_sample "$label" audio-fanout
        done
    done
}

print_audio_sample() {
    local label=$1
    local workload=$2
    jq -r --arg shape "$label" '[
        $shape, .recv_fps, .latency_p99_ms, (.cpu_cores // "n/a"),
        (if .rss_bytes then .rss_bytes / 1048576 else "n/a" end)
    ] | @tsv' "$RUN/relay/$label/$workload/summary.json"
}

# Worker runtimes read their certificate from disk instead of generating one.
generate_certificate() {
    if ! command -v openssl >/dev/null; then
        printf 'worker runtimes need openssl to generate a temporary certificate\n' >&2
        return 1
    fi

    umask 077
    printf '[req]\ndistinguished_name = req_dn\n[req_dn]\n' >"$RUN/openssl.cnf"
    if ! OPENSSL_CONF=$RUN/openssl.cnf \
        openssl req -x509 -sha256 -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 -nodes \
        -days 1 -subj '/CN=localhost' -addext 'subjectAltName=DNS:localhost' \
        -keyout "$RUN/localhost.key" -out "$RUN/localhost.crt" >"$RUN/openssl.log" 2>&1; then
        printf 'failed to generate the temporary benchmark certificate:\n' >&2
        cat "$RUN/openssl.log" >&2
        return 1
    fi
}

run_runtime_comparison() {
    if [[ $(uname -s) != Linux ]]; then
        printf 'runtime comparison needs Linux for io_uring and /proc metrics\n' >&2
        return 1
    fi
    generate_certificate

    printf 'Building one relay binary with the Tokio and io_uring paths...\n'
    CARGO_TARGET_DIR=$CURRENT_TARGET cargo build --locked --release \
        -p moq-relay -p moq-bench \
        --features moq-relay/io-uring,moq-bench/uring
    LOAD_BIN=$CURRENT_TARGET/release/moq-bench
    HOST_BIN=$CURRENT_TARGET/release/moq-bench-host

    printf 'Checking io_uring support...\n'
    if ! "$CURRENT_TARGET/release/moq-bench-uring-check"; then
        printf 'runtime comparison needs Linux 6.12+ with io_uring permitted by the host\n' >&2
        return 1
    fi

    local -a runtimes=(tokio-shared tokio-workers io-uring-workers)
    local round workload offset index runtime
    for workload in video fanout video-heavy fanout-heavy; do
        printf '\nRelay runtime workload: %s\n' "$workload"
        for ((round = 1; round <= ROUNDS; round++)); do
            # Rotate the order so thermal drift and other time-dependent noise do
            # not consistently favor the same runtime.
            offset=$(((round - 1) % ${#runtimes[@]}))
            for ((index = 0; index < ${#runtimes[@]}; index++)); do
                runtime=${runtimes[$(((index + offset) % ${#runtimes[@]}))]}
                printf '  round %d/%d: %s\n' "$round" "$ROUNDS" "$runtime"
                run_workload "$runtime/round-$round" "$CURRENT_TARGET/release/moq-relay" \
                    "$workload" "$runtime" "$RUNTIME_WORKERS"
            done
        done
    done

    print_runtime_comparison
}

# Each workload's load axis is its connection count: every room connection
# publishes and subscribes, while fanout adds subscribers to one publisher. A
# cost that grows with the shard count or the load shows up as a slope across
# the shard counts or the connection counts, which one default-versus-single-
# socket run cannot show.
run_shard_sweep() {
    if [[ $(uname -s) != Linux ]]; then
        printf 'shard sweep needs Linux: only it steers a reuseport group\n' >&2
        return 1
    fi
    generate_certificate

    printf 'Building relay and load generator...\n'
    CARGO_TARGET_DIR=$CURRENT_TARGET cargo build --locked --release -p moq-relay -p moq-bench
    LOAD_BIN=$CURRENT_TARGET/release/moq-bench
    HOST_BIN=$CURRENT_TARGET/release/moq-bench-host

    local cpus workload connections shards label
    cpus=$(getconf _NPROCESSORS_ONLN)
    for workload in video fanout; do
        for connections in $SHARD_CONNECTIONS; do
            for shards in $SHARD_COUNTS; do
                label=shard-$shards-$connections
                printf '  %s: %d connections, %d shards\n' "$workload" "$connections" "$shards"
                # Zero shards is the shared runtime, given every CPU.
                if ((shards == 0)); then
                    run_workload "$label" "$CURRENT_TARGET/release/moq-relay" "$workload" tokio-shared "$cpus" \
                        --connections "$connections"
                else
                    run_workload "$label" "$CURRENT_TARGET/release/moq-relay" "$workload" tokio-workers "$shards" \
                        --connections "$connections"
                fi
            done
        done
    done

    printf '\nShard sweep (0 shards is the shared runtime on %d threads)\n' "$cpus"
    printf '%-8s %11s %7s %10s %10s %8s %7s %8s %9s %9s\n' \
        workload connections shards recv-Mbps recv-fps p99-ms loss-% CPU CPU/conn RSS-MiB
    for workload in video fanout; do
        for connections in $SHARD_CONNECTIONS; do
            for shards in $SHARD_COUNTS; do
                jq -r --arg connections "$connections" --arg shards "$shards" '[
                    .workload, $connections, $shards, .recv_mbps, .recv_fps, .latency_p99_ms, .loss_pct,
                    .cpu_cores, (.cpu_cores / ($connections | tonumber)), (.rss_bytes / 1048576)
                ] | @tsv' "$RUN/relay/shard-$shards-$connections/$workload/summary.json" |
                    while IFS=$'\t' read -r name conns count recv fps p99 loss cpu per rss; do
                        printf '%-8s %11s %7s %10.3f %10.1f %8.1f %7.3f %8.3f %9.5f %9.1f\n' \
                            "$name" "$conns" "$count" "$recv" "$fps" "$p99" "$loss" "$cpu" "$per" "$rss"
                    done
            done
        done
    done
}

run_lock_profile() {
    if [[ $(uname -s) != Linux ]]; then
        printf 'lock profiling needs Linux for perf and /proc schedstat\n' >&2
        return 1
    fi
    # Refuse before the build when the kernel would refuse the capture.
    python3 "$ROOT/bench/profile.py" check
    generate_certificate

    printf 'Building the relay with the profiling profile and frame pointers...\n'
    CARGO_TARGET_DIR=$TARGET/profile cargo build --locked --profile profiling -p moq-relay -p moq-bench
    LOAD_BIN=$TARGET/profile/profiling/moq-bench
    HOST_BIN=$TARGET/profile/profiling/moq-bench-host
    local relay=$TARGET/profile/profiling/moq-relay

    printf '\nLock wait: %s, tokio workers; each profiled run follows an unprofiled control\n' "$LOCK_WORKLOAD"
    printf '%-8s %5s %8s %13s %13s %10s %10s %13s %9s\n' \
        workers idle switches control-fps profiled-fps control-CPU profiled-CPU mutex-wait-ms wait/CPU
    local workers
    for workers in $LOCK_WORKERS; do
        run_workload "control-$workers" "$relay" "$LOCK_WORKLOAD" tokio-workers "$workers"
        PROFILE_LOCK=true
        run_workload "lock-$workers" "$relay" "$LOCK_WORKLOAD" tokio-workers "$workers"
        PROFILE_LOCK=
        jq -r -s '[
            .[0].recv_fps, .[1].recv_fps, .[0].cpu_cores, .[1].cpu_cores, (.[2].idle | length),
            ([.[2].workers[].switches] | min),
            ([.[2].workers[].mutex_wait_ns] | add / 1000000),
            ([.[2].workers[].mutex_wait_ns] | add) / ([.[2].workers[].cpu_ns] | add)
        ] | @tsv' "$RUN/relay/control-$workers/$LOCK_WORKLOAD/summary.json" \
            "$RUN/relay/lock-$workers/$LOCK_WORKLOAD/summary.json" \
            "$RUN/relay/lock-$workers/$LOCK_WORKLOAD/lock/lock.json" |
            while IFS=$'\t' read -r control profiled control_cpu profiled_cpu idle switches wait share; do
                printf '%-8s %5s %8s %13.1f %13.1f %10.3f %10.3f %13.1f %9.4f\n' \
                    "$workers" "$idle" "$switches" "$control" "$profiled" "$control_cpu" "$profiled_cpu" "$wait" "$share"
            done
    done
    printf '\nMutex stacks (flamegraph folded format): %s/relay/lock-*/%s/lock/mutex.folded\n' "$RUN" "$LOCK_WORKLOAD"
}

cd "$ROOT"

if [[ $MODE == --audio ]]; then
    run_audio_comparison
    exit 0
elif [[ $MODE == --runtime ]]; then
    run_runtime_comparison
    exit 0
elif [[ $MODE == --lock ]]; then
    run_lock_profile
    exit 0
elif [[ $MODE == --shard ]]; then
    run_shard_sweep
    exit 0
elif [[ -n $BASE ]]; then
    BASE_COMMIT=$(git rev-parse --verify "$BASE^{commit}")
    WORKTREE=$RUN/base
    git worktree add --detach "$WORKTREE" "$BASE_COMMIT" >/dev/null
    mkdir -p "$RUN/base-context"
    (
        cd "$WORKTREE"
        python3 "$ROOT/bench/metadata.py" "$RUN/base-context" "$BASE_COMMIT" comparison-base
    )

    printf 'Criterion comparison: %s (%s) versus current\n' "$BASE" "$BASE_COMMIT"
    compare_criterion "$WORKTREE" "$ROOT"
    report_set_changes

    printf '\nBuilding relay binaries...\n'
    (
        cd "$WORKTREE"
        CARGO_TARGET_DIR=$BASE_TARGET cargo build --locked --release -p moq-relay
    )
    cp "$BASE_TARGET/release/moq-relay" "$RUN/moq-relay-base"
    chmod +x "$RUN/moq-relay-base"
else
    printf 'Criterion current: %s\n' "$(git rev-parse --short HEAD)"
    run_criterion "$ROOT" --discard-baseline
    printf '\nBuilding relay binaries...\n'
fi

CARGO_TARGET_DIR=$CURRENT_TARGET cargo build --locked --release -p moq-relay -p moq-bench
LOAD_BIN=$CURRENT_TARGET/release/moq-bench
HOST_BIN=$CURRENT_TARGET/release/moq-bench-host

if [[ -n $BASE ]]; then
    printf '\nRelay workloads: paired base/current\n'
    for ((round = 1; round <= ROUNDS; round++)); do
        if ((round % 2)); then order=(base current); else order=(current base); fi
        printf '%s\t%s\n' "$round" "${order[*]}" >>"$RUN/execution-order.tsv"
        for workload in video fanout; do
            for label in "${order[@]}"; do
                if [[ $label == base ]]; then relay=$RUN/moq-relay-base; else relay=$CURRENT_TARGET/release/moq-relay; fi
                run_workload "$label/round-$round" "$relay" "$workload"
            done
        done
    done
else
    run_relay_suite current "$CURRENT_TARGET/release/moq-relay"
fi

if [[ -n $BASE ]]; then
    python3 "$ROOT/bench/compare.py" "$RUN"
else
    print_current_relay
fi
