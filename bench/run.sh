#!/usr/bin/env bash
set -euo pipefail

# Runs every Criterion target plus the local relay workloads: video over QUIC, and
# chat and fanout over each of BENCH_TRANSPORTS. With a base commit, Criterion
# compares matching cases directly and the relay summary prints the base/current delta. `--runtime` instead compares one multi-threaded Tokio
# runtime with the same number of independent Tokio/epoll and io_uring workers
# on the current tree. The current load generator drives every relay so the
# workloads stay identical. `--audio` isolates the audio grouping/load-shape matrix.
# `--profile` captures relay profiles: std mutex waits by stack (lock), CPU stacks
# (cpu), or jemalloc heap snapshots and allocation churn (heap).
# `--shard` sweeps the QUIC shard count against the connection count.

MODE=${1:-}
ROUNDS=${BENCH_RUNTIME_ROUNDS:-3}
if [[ ! $ROUNDS =~ ^[0-9]+$ ]] || ((10#$ROUNDS < 1)); then
    printf 'BENCH_RUNTIME_ROUNDS must be a positive integer, got %q\n' "$ROUNDS" >&2
    exit 1
fi
ROUNDS=$((10#$ROUNDS))

# One case measures far apart on performance and efficiency cores, so on a
# hybrid CPU pin this shell, and with it every build and benchmark process, to
# the performance cores it may already use. The kernel only lists them on Intel
# hybrid parts.
PINNED=
if [[ -r /sys/devices/cpu_core/cpus ]]; then
    PINNED=$(
        python3 - "$$" <<'EOF'
import os, sys
pid = int(sys.argv[1])
cores = set()
for part in open("/sys/devices/cpu_core/cpus").read().strip().split(","):
    first, _, last = part.partition("-")
    cores.update(range(int(first), int(last or first) + 1))
allowed = cores & os.sched_getaffinity(pid)
if not allowed:
    sys.exit("cannot pin to the performance cores: none are in the allowed CPU set")
os.sched_setaffinity(pid, allowed)
print(",".join(map(str, sorted(allowed))))
EOF
    )
fi

# nproc counts the allowed CPUs, unless OpenMP variables override it.
usable_cpus() {
    env -u OMP_NUM_THREADS -u OMP_THREAD_LIMIT nproc
}
case $MODE in
    --criterion)
        # BENCH_BASE pairs each selected target with the same target built at that revision.
        BASE=
        if [[ -n ${BENCH_BASE:-} ]]; then
            BASE=$(git rev-parse --verify "$BENCH_BASE^{commit}")
        fi
        CRITERION_CARGO_ARGS=()
        CRITERION_FILTER_ARGS=()
        parsing_filters=false
        for argument in "${@:2}"; do
            if [[ $argument == -- && $parsing_filters == false ]]; then
                parsing_filters=true
            elif [[ $parsing_filters == true ]]; then
                CRITERION_FILTER_ARGS+=("$argument")
            else
                CRITERION_CARGO_ARGS+=("$argument")
            fi
        done
        ;;
    --audio)
        BASE=
        ;;
    --profile)
        BASE=${BENCH_BASE:-}
        if [[ -n $BASE ]]; then
            BASE=$(git rev-parse --verify "$BASE^{commit}")
        fi
        PROFILE_RUNTIME=${BENCH_PROFILE_RUNTIME:-tokio-workers}
        if [[ ! $PROFILE_RUNTIME =~ ^(tokio-workers|io-uring-workers)$ ]]; then
            printf 'profile runtime must be tokio-workers or io-uring-workers, got %q\n' "$PROFILE_RUNTIME" >&2
            exit 1
        fi
        PROFILE_MODES=${BENCH_PROFILE_MODES:-}
        PROFILE_WORKLOAD=${BENCH_PROFILE_WORKLOAD:-fanout}
        PROFILE_TRANSPORT=${BENCH_PROFILE_TRANSPORT:-quic}
        if [[ ! $PROFILE_TRANSPORT =~ ^(quic|ws|wss|tcp|tls)$ ]]; then
            printf 'profile transport must be quic, ws, wss, tcp, or tls, got %q\n' "$PROFILE_TRANSPORT" >&2
            exit 1
        fi
        PROFILE_WORKERS=${BENCH_PROFILE_WORKERS:-1 4 16}
        if [[ -z $PROFILE_MODES ]]; then
            printf 'BENCH_PROFILE_MODES must name one or more of: lock cpu heap\n' >&2
            exit 1
        fi
        for value in $PROFILE_MODES; do
            if [[ ! $value =~ ^(lock|cpu|heap)$ ]]; then
                printf 'unknown profile mode %q; expected lock, cpu, or heap\n' "$value" >&2
                exit 1
            fi
        done
        for value in $PROFILE_WORKERS; do
            if [[ ! $value =~ ^[1-9][0-9]{0,2}$ ]] || ((value > 256)); then
                printf 'profile workers must be integers from 1 to 256, got %q\n' "$value" >&2
                exit 1
            fi
        done
        # Without frame pointers, perf's DWARF unwinder stops at the first relay frame,
        # and CPU stacks unwind by frame pointer alone.
        export RUSTFLAGS="${RUSTFLAGS:+$RUSTFLAGS }-C force-frame-pointers=yes"
        ;;
    --shard)
        BASE=
        SHARD_COUNTS=${BENCH_SHARD_COUNTS:-0 1 4 16}
        SHARD_CONNECTIONS=${BENCH_SHARD_CONNECTIONS:-16 64 256}
        for value in $SHARD_COUNTS $SHARD_CONNECTIONS; do
            if [[ ! $value =~ ^[0-9]+$ ]]; then
                printf 'shard benchmark counts must be integers, got %q\n' "$value" >&2
                exit 1
            fi
        done
        ;;
    --runtime)
        BASE=
        RUNTIME_WORKERS=${BENCH_RUNTIME_WORKERS:-}
        if [[ -z $RUNTIME_WORKERS ]]; then
            RUNTIME_WORKERS=$(usable_cpus)
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
        # Video stays on QUIC; chat (per-message cost) and fanout (per-byte cost) sweep the transports.
        RELAY_CASES=(quic/video)
        for value in ${BENCH_TRANSPORTS:-quic ws wss tcp tls}; do
            if [[ ! $value =~ ^(quic|ws|wss|tcp|tls)$ ]]; then
                printf 'unknown transport %q; expected quic, ws, wss, tcp, or tls\n' "$value" >&2
                exit 1
            fi
            RELAY_CASES+=("$value/chat" "$value/fanout")
        done
        ;;
esac
ROOT=$(git rev-parse --show-toplevel)
TARGET=${CARGO_TARGET_DIR:-$ROOT/target/bench}
if [[ $TARGET != /* ]]; then
    TARGET=$ROOT/$TARGET
fi
BASE_TARGET=$TARGET/base
CURRENT_TARGET=$TARGET/current

ARTIFACTS=${BENCH_ARTIFACTS:-$ROOT/.tmp/bench}
mkdir -p "$ARTIFACTS"
ARTIFACTS=$(cd "$ARTIFACTS" && pwd -P)
# Each run copies bench/ and untracked sources, so it must land in neither.
if [[ $ARTIFACTS/ == "$ROOT"/bench/* ]] ||
    { [[ $ARTIFACTS/ == "$ROOT"/* ]] && ! git -C "$ROOT" check-ignore -q "$ARTIFACTS"; }; then
    printf 'BENCH_ARTIFACTS must be outside the repository, or git-ignored and outside bench/: %s\n' \
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
    if [[ -n $PINNED ]]; then
        printf 'Pinned to performance cores: %s\n' "$PINNED"
    fi
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
                | [($package.manifest_path | ltrimstr($root)), .name, (.["required-features"] // [] | join(","))]
                | @tsv
            ' |
            sort
    )
}

# The features a bench target requires, comma-separated; cargo refuses to build it without them.
target_features() {
    local checkout=$1
    local manifest=$2
    local target=$3
    criterion_targets "$checkout" |
        awk -F '\t' -v manifest="$manifest" -v target="$target" '$1 == manifest && $2 == target { print $3; exit }'
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
    while IFS=$'\t' read -r manifest target _; do
        run_criterion_target "$checkout" "$manifest" "$target" "$@"
    done <"$target_list"
}

run_criterion_target() {
    local checkout=$1
    local manifest=$2
    local target=$3
    local features
    shift 3
    features=$(target_features "$checkout" "$manifest" "$target") || return 1
    (
        cd "$checkout" || exit 1
        CARGO_TARGET_DIR=$(target_dir "$checkout") \
            cargo bench --locked --manifest-path "$manifest" --bench "$target" ${features:+--features "$features"} -- \
            "${CRITERION_ARGS[@]}" "$@"
    )
}

# The nightly trend's settings, which also screen every case of a paired run.
CRITERION_QUICK=(--quick --warm-up-time 0.05 --measurement-time 0.2 --noplot)

# Build the selected bench targets in a checkout, listing each in OUT as
# "package-directory/target<TAB>package path<TAB>executable". OUT is written here rather
# than by redirecting the call: an errexit inside a redirected function would leave the
# shell's saved stdout open, and cleanup would wait forever on the log copier.
criterion_build() {
    local out=$1 checkout=$2
    shift 2
    (
        cd "$checkout" || exit 1
        CARGO_TARGET_DIR=$(target_dir "$checkout") \
            cargo bench --locked --no-run --message-format=json-render-diagnostics "$@"
    ) | jq -r 'select(.reason == "compiler-artifact" and .executable != null and any(.target.kind[]; . == "bench"))
        | (.manifest_path | rtrimstr("/Cargo.toml")) as $package
        | [($package | split("/") | last) + "/" + .target.name, $package, .executable] | @tsv' | sort >"$out"
}

# Build the base with the current cargo arguments, minus bench targets its selected packages
# do not have yet: naming one would fail the whole build. Lists nothing in OUT when every
# selected target is new.
criterion_build_base() {
    local out=$1 names name index selected=0 count=${#CRITERION_CARGO_ARGS[@]}
    local args=() packages=()
    : >"$out"
    for ((index = 0; index + 1 < count; index++)); do
        if [[ ${CRITERION_CARGO_ARGS[index]} == -p || ${CRITERION_CARGO_ARGS[index]} == --package ]]; then
            packages+=("${CRITERION_CARGO_ARGS[index + 1]}")
        fi
    done
    # Another package's target of the same name must not count, or cargo rejects the base build.
    names=$(cd "$WORKTREE" && cargo metadata --locked --format-version 1 --no-deps |
        jq -r '.packages[] | select(($ARGS.positional | length) == 0 or (.name as $name | any($ARGS.positional[]; . == $name)))
            | .targets[] | select(any(.kind[]; . == "bench")) | .name' --args ${packages[@]+"${packages[@]}"}) || return 1
    for ((index = 0; index < count; index++)); do
        name=${CRITERION_CARGO_ARGS[index]}
        if [[ $name == --bench ]] && ((index + 1 < count)); then
            index=$((index + 1))
            name=${CRITERION_CARGO_ARGS[index]}
            selected=$((selected + 1))
            # A glob selects whatever the base has.
            if [[ $name == *[*?[]* ]] || grep -qxF -- "$name" <<<"$names"; then
                args+=(--bench "$name")
            fi
        else
            args+=("$name")
        fi
    done
    if ((selected)) && [[ " ${args[*]} " != *" --bench "* ]]; then
        return 0
    fi
    criterion_build "$out" "$WORKTREE" ${args[@]+"${args[@]}"}
}

# Run one built target as cargo bench would, from its package directory, with results under HOME/KEY.
criterion_exec() {
    local list=$1 home=$2 key=$3 package executable
    shift 3
    IFS=$'\t' read -r _ package executable < <(target_line "$list" "$key") || return 1
    printf '\nCriterion %s: %s\n' "${home#"$RUN"/}" "$key"
    (cd "$package" && CRITERION_HOME=$home/$key "$executable" --bench "$@" </dev/null)
}

target_line() {
    MOQ_CASE_TARGET=$2 awk -F '\t' '$1 == ENVIRON["MOQ_CASE_TARGET"] { print; exit }' "$1"
}

# A comparison that cannot run fails the paired gate rather than the measurement.
unavailable() {
    printf '%s\t%s\n' "$1" "$2" >>"$RUN/paired-unavailable.tsv"
    printf 'Paired comparison unavailable for %s: %s\n' "$1" "$2" >&2
}

# Measure the selected targets in quick mode. With a base, run each target's base build
# beside it, alternating which goes first, then confirm the screen's worst slowdowns.
# A failed current target fails the run; the base only marks its targets unavailable.
measure_criterion() {
    local current=$RUN/criterion-current.targets base=$RUN/criterion-base.targets
    local key side targets index=0 status=0
    local filters=(${CRITERION_FILTER_ARGS[@]+"${CRITERION_FILTER_ARGS[@]}"})
    criterion_build "$current" "$ROOT" ${CRITERION_CARGO_ARGS[@]+"${CRITERION_CARGO_ARGS[@]}"}
    mapfile -t targets < <(cut -f1 "$current")
    if ((${#targets[@]} == 0)); then
        printf 'no Criterion bench targets selected\n' >&2
        return 1
    fi
    : >"$base"
    if [[ -n $BASE ]]; then
        WORKTREE=$RUN/base
        git worktree add --detach "$WORKTREE" "$BASE" >/dev/null
        mkdir -p "$RUN/base-context"
        (cd "$WORKTREE" && python3 "$ROOT/bench/metadata.py" "$RUN/base-context" "$BASE" comparison-base)
        printf 'Criterion paired with base %s\n' "$BASE"
        if ! criterion_build_base "$base"; then
            : >"$base"
            for key in "${targets[@]}"; do unavailable "$key" "base build failed"; done
        fi
    fi
    for key in "${targets[@]}"; do
        local order=(current)
        if [[ -n $(target_line "$base" "$key") ]]; then
            if ((index++ % 2)); then order=(current base); else order=(base current); fi
        fi
        for side in "${order[@]}"; do
            if [[ $side == current ]]; then
                criterion_exec "$current" "$RUN/criterion" "$key" "${CRITERION_QUICK[@]}" \
                    ${filters[@]+"${filters[@]}"} || status=1
            else
                criterion_exec "$base" "$RUN/criterion-base" "$key" "${CRITERION_QUICK[@]}" \
                    ${filters[@]+"${filters[@]}"} || unavailable "$key" "base run failed"
            fi
        done
    done
    if [[ -n $BASE ]] && ((status == 0)); then
        confirm_criterion || return 1
    fi
    return "$status"
}

# One quick run per side can land on noise, so each slowdown the screen flags is
# re-measured in alternating rounds with the comparison settings before it can fail.
confirm_criterion() {
    local key benchmark round side index=0
    python3 "$ROOT/bench/compare.py" --criterion-screen "$RUN" >"$RUN/confirm.tsv" || return 1
    while IFS=$'\t' read -r key benchmark; do
        index=$((index + 1))
        for ((round = 1; round <= ROUNDS; round++)); do
            local order=(base current)
            if ((round % 2 == 0)); then order=(current base); fi
            for side in "${order[@]}"; do
                if ! criterion_exec "$RUN/criterion-$side.targets" "$RUN/confirm/$index/round-$round/$side" "$key" \
                    "${CRITERION_ARGS[@]}" "$benchmark" --exact; then
                    unavailable "$key" "confirmation failed"
                    continue 3
                fi
            done
        done
    done <"$RUN/confirm.tsv"
    python3 "$ROOT/bench/compare.py" --criterion-paired "$RUN"
}

criterion_cases() {
    local checkout=$1
    local manifest target benchmark discovery
    local target_list=$RUN/criterion-targets
    local label=base
    if [[ $checkout == "$ROOT" ]]; then
        label=current
    fi

    if ! criterion_targets "$checkout" >"$target_list"; then
        return 1
    fi
    if [[ ! -s $target_list ]]; then
        printf 'no Criterion benchmark targets found\n' >&2
        return 1
    fi
    while IFS=$'\t' read -r manifest target _; do
        discovery=$RUN/criterion-discovery/$label/$manifest/$target
        # --list prints truncated display titles. A separate short measurement
        # writes full IDs without polluting the actual comparison samples.
        (
            CRITERION_ARGS=(--warm-up-time 0.001 --measurement-time 0.001 --sample-size 10 --nresamples 1 --noplot)
            CRITERION_HOME=$discovery run_criterion_target "$checkout" "$manifest" "$target" --save-baseline discovery
        ) >&2 || return 1
        # A harness = false target without Criterion, such as moq-cli's play_buffer, writes no samples.
        if [[ -z $(find "$discovery" -name benchmark.json -print -quit 2>/dev/null) ]]; then
            printf 'Not a Criterion target, skipped: %s %s\n' "$manifest" "$target" >&2
            continue
        fi
        python3 "$ROOT/bench/compare.py" --criterion-cases "$discovery" >"$RUN/criterion-listing" || return 1
        while IFS= read -r benchmark; do
            printf '%s\t%s\t%s\n' "$manifest" "$target" "$benchmark"
        done <"$RUN/criterion-listing"
    done <"$target_list"
}

compare_criterion() {
    local base_checkout=$1
    local current_checkout=$2
    local base_cases=$RUN/criterion-base.cases
    local current_cases=$RUN/criterion-current.cases
    local keys=$RUN/criterion-case.keys
    local manifest target benchmark base_match current_match home
    local index=0

    criterion_cases "$base_checkout" >"$base_cases"
    criterion_cases "$current_checkout" >"$current_cases"
    sort -u "$base_cases" "$current_cases" >"$keys"

    while IFS=$'\t' read -r manifest target benchmark; do
        index=$((index + 1))
        home=$RUN/criterion/$index
        base_match=$(MOQ_CASE_MANIFEST=$manifest MOQ_CASE_TARGET=$target MOQ_CASE_ID=$benchmark awk -F '\t' \
            '$1 == ENVIRON["MOQ_CASE_MANIFEST"] && $2 == ENVIRON["MOQ_CASE_TARGET"] && $3 == ENVIRON["MOQ_CASE_ID"] { print; exit }' "$base_cases")
        current_match=$(MOQ_CASE_MANIFEST=$manifest MOQ_CASE_TARGET=$target MOQ_CASE_ID=$benchmark awk -F '\t' \
            '$1 == ENVIRON["MOQ_CASE_MANIFEST"] && $2 == ENVIRON["MOQ_CASE_TARGET"] && $3 == ENVIRON["MOQ_CASE_ID"] { print; exit }' "$current_cases")

        if [[ -n $base_match ]]; then
            printf '\nCriterion baseline: %s\n' "$benchmark"
            CRITERION_HOME=$home run_criterion_target "$base_checkout" "$manifest" "$target" \
                --save-baseline base "$benchmark" --exact
        fi
        if [[ -n $current_match ]]; then
            printf '\nCriterion current: %s\n' "$benchmark"
            if [[ -n $base_match ]]; then
                # Truncated directory names can receive different uniqueness suffixes
                # when cases are added or reordered between revisions.
                python3 "$ROOT/bench/compare.py" --criterion-baseline \
                    "$RUN/criterion-discovery/current/$manifest/$target" "$benchmark" "$home"
                CRITERION_HOME=$home run_criterion_target "$current_checkout" "$manifest" "$target" \
                    --baseline base "$benchmark" --exact
            else
                CRITERION_HOME=$home run_criterion_target "$current_checkout" "$manifest" "$target" \
                    --save-baseline current "$benchmark" --exact
            fi
        fi
    done <"$keys"
}

report_set_changes() {
    local base_ids=$RUN/criterion-base.ids
    local current_ids=$RUN/criterion-current.ids
    # Baseline measurements also write new/, so saved sample directories cannot
    # distinguish a removed case from a case measured on the current revision.
    cut -f3- "$RUN/criterion-base.cases" | sort -u >"$base_ids"
    cut -f3- "$RUN/criterion-current.cases" | sort -u >"$current_ids"

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
    printf '\n%-12s %11s %11s %10s %10s %9s %9s %10s %11s %11s %10s\n' \
        case send-Mbps recv-Mbps send-fps recv-fps loss-% p99-ms CPU-cores CPU-us/frame cores/Gbps RSS-MiB
    for case in "${RELAY_CASES[@]}"; do
        # Host CPU and RSS exist only where the sampler ran (Linux).
        jq -r --arg case "$case" 'def fixed: if . == null then "n/a" else (. * 1000 | round / 1000 | tostring) end; [
			$case,
			(.send_mbps | tostring),
			(.recv_mbps | tostring),
			(.send_fps | tostring),
			(.recv_fps | tostring),
			(.loss_pct | tostring),
			(.latency_p99_ms // "n/a" | tostring),
			(.cpu_cores | fixed),
			(.cpu_us_per_frame | fixed),
			(.cpu_cores_per_gbps | fixed),
			(.rss_bytes | if . then . / 1048576 else null end | fixed)
		] | @tsv' "$RUN/relay/current/$case/summary.json" |
            while IFS=$'\t' read -r name send recv send_fps recv_fps loss p99 cpu per_frame per_gbps rss; do
                printf '%-12s %11.3f %11.3f %10.1f %10.1f %9.3f %9s %10s %11s %11s %10s\n' \
                    "$name" "$send" "$recv" "$send_fps" "$recv_fps" "$loss" "$p99" "$cpu" "$per_frame" "$per_gbps" "$rss"
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
    generate_certificate
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

# Every relay reads its certificate from disk: worker runtimes and the wss listener need a file.
generate_certificate() {
    if ! command -v openssl >/dev/null; then
        printf 'relay workloads need openssl to generate a temporary certificate\n' >&2
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

# The runtime matrix and profiling use identical feature selection and support checks.
build_workers() {
    local checkout=$1 target=$2 profile=$3 features=moq-relay/io-uring,moq-bench/uring
    if [[ ${4:-} == heap ]]; then
        features+=,moq-relay/jemalloc
    fi
    printf 'Building worker runtimes: %s (%s)\n' "$checkout" "$profile"
    (
        cd "$checkout"
        CARGO_TARGET_DIR=$target cargo build --locked --profile "$profile" \
            -p moq-relay -p moq-bench --features "$features"
    )
}

check_uring() {
    printf 'Checking io_uring support...\n'
    if ! "$1/moq-bench-uring-check"; then
        printf 'worker benchmarks need Linux 6.12+ with io_uring permitted by the host\n' >&2
        return 1
    fi
}

run_runtime_comparison() {
    if [[ $(uname -s) != Linux ]]; then
        printf 'runtime comparison needs Linux for io_uring and /proc metrics\n' >&2
        return 1
    fi
    generate_certificate

    build_workers "$ROOT" "$CURRENT_TARGET" release
    LOAD_BIN=$CURRENT_TARGET/release/moq-bench
    HOST_BIN=$CURRENT_TARGET/release/moq-bench-host
    check_uring "$CURRENT_TARGET/release"

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
    cpus=$(usable_cpus)
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

run_profiles() {
    if [[ $(uname -s) != Linux ]]; then
        printf 'relay profiling needs Linux for perf and /proc\n' >&2
        return 1
    fi
    # Refuse before the build when the host would refuse a capture.
    local mode
    for mode in $PROFILE_MODES; do
        python3 "$ROOT/bench/profile.py" check "$mode"
    done
    generate_certificate

    if [[ -n $BASE ]]; then
        WORKTREE=$RUN/base
        git worktree add --detach "$WORKTREE" "$BASE" >/dev/null
        mkdir -p "$RUN/base-context"
        (cd "$WORKTREE" && python3 "$ROOT/bench/metadata.py" "$RUN/base-context" "$BASE" --profile)
    fi
    local label checkout target
    local -a revisions=(current)
    if [[ -n $BASE ]]; then revisions=(base current); fi
    for mode in $PROFILE_MODES; do
        # The current generator and sampler drive both revisions, including old bases
        # whose own harness cannot select the io_uring runtime.
        build_workers "$ROOT" "$CURRENT_TARGET/profile" profiling "$mode"
        LOAD_BIN=$CURRENT_TARGET/profile/profiling/moq-bench
        HOST_BIN=$CURRENT_TARGET/profile/profiling/moq-bench-host
        for label in "${revisions[@]}"; do
            checkout=$ROOT target=$CURRENT_TARGET/profile
            if [[ $label == base ]]; then
                checkout=$WORKTREE target=$BASE_TARGET/profile
                build_workers "$checkout" "$target" profiling "$mode"
            fi
            if [[ $PROFILE_RUNTIME == io-uring-workers ]]; then
                check_uring "$target/profiling"
            fi
            run_profile "$mode" "$label" "$target/profiling/moq-relay"
        done
    done
}

# Each profiled run follows an unprofiled control on the same binary, so the
# throughput and CPU difference shows the profiler's overhead.
run_profile() {
    local mode=$1 label=$2 relay=$3
    local workers control profiled
    TRANSPORT=$PROFILE_TRANSPORT
    case $mode in
        lock)
            printf '\nLock wait: %s over %s, %s, %s\n' "$PROFILE_WORKLOAD" "$TRANSPORT" "$PROFILE_RUNTIME" "$label"
            printf '%-8s %5s %8s %13s %13s %10s %10s %13s %9s\n' \
                workers idle switches control-fps profiled-fps control-CPU profiled-CPU mutex-wait-ms wait/CPU
            ;;
        cpu)
            printf '\nCPU stacks: %s over %s, %s, %s, sampled at 999 Hz\n' "$PROFILE_WORKLOAD" "$TRANSPORT" "$PROFILE_RUNTIME" "$label"
            printf '%-8s %13s %13s %10s %10s %9s %11s\n' \
                workers control-fps profiled-fps control-CPU profiled-CPU samples sampled-CPU
            ;;
        heap)
            printf '\nHeap: %s over %s, %s, %s, jemalloc retained MiB and churn over the steady window\n' "$PROFILE_WORKLOAD" "$TRANSPORT" "$PROFILE_RUNTIME" "$label"
            printf '%-8s %13s %13s %10s %10s %10s %10s %9s %9s %9s %11s %10s\n' \
                workers control-fps profiled-fps control-CPU profiled-CPU control-RSS profiled-RSS \
                before steady teardown churn-MiB/s allocs/s
            ;;
    esac
    for workers in $PROFILE_WORKERS; do
        run_workload "$label/control-$mode-$workers" "$relay" "$PROFILE_WORKLOAD" "$PROFILE_RUNTIME" "$workers"
        PROFILE=$mode
        run_workload "$label/$mode-$workers" "$relay" "$PROFILE_WORKLOAD" "$PROFILE_RUNTIME" "$workers"
        PROFILE=
        control=$RUN/relay/$label/control-$mode-$workers/$PROFILE_WORKLOAD/summary.json
        profiled=$RUN/relay/$label/$mode-$workers/$PROFILE_WORKLOAD
        case $mode in
            lock)
                jq -r -s '[
                    .[0].recv_fps, .[1].recv_fps, .[0].cpu_cores, .[1].cpu_cores, (.[2].idle | length),
                    ([.[2].workers[].switches] | min),
                    ([.[2].workers[].mutex_wait_ns] | add / 1000000),
                    ([.[2].workers[].mutex_wait_ns] | add) / ([.[2].workers[].cpu_ns] | add)
                ] | @tsv' "$control" "$profiled/summary.json" "$profiled/lock/lock.json" |
                    while IFS=$'\t' read -r control_fps profiled_fps control_cpu profiled_cpu idle switches wait share; do
                        printf '%-8s %5s %8s %13.1f %13.1f %10.3f %10.3f %13.1f %9.4f\n' "$workers" "$idle" "$switches" \
                            "$control_fps" "$profiled_fps" "$control_cpu" "$profiled_cpu" "$wait" "$share"
                    done
                ;;
            cpu)
                jq -r -s '[
                    .[0].recv_fps, .[1].recv_fps, .[0].cpu_cores, .[1].cpu_cores, .[2].samples, .[2].sampled_cores
                ] | @tsv' "$control" "$profiled/summary.json" "$profiled/cpu/cpu.json" |
                    while IFS=$'\t' read -r control_fps profiled_fps control_cpu profiled_cpu samples sampled; do
                        printf '%-8s %13.1f %13.1f %10.3f %10.3f %9s %11.3f\n' "$workers" \
                            "$control_fps" "$profiled_fps" "$control_cpu" "$profiled_cpu" "$samples" "$sampled"
                    done
                ;;
            heap)
                jq -r -s '[
                    .[0].recv_fps, .[1].recv_fps, .[0].cpu_cores, .[1].cpu_cores,
                    (.[0].rss_bytes / 1048576), (.[1].rss_bytes / 1048576),
                    (.[2].retained_bytes | .before, .["steady-end"], .teardown | . / 1048576),
                    (.[2].churn_bytes_per_s / 1048576), .[2].churn_allocs_per_s
                ] | @tsv' "$control" "$profiled/summary.json" "$profiled/heap/heap.json" |
                    while IFS=$'\t' read -r control_fps profiled_fps control_cpu profiled_cpu control_rss profiled_rss \
                        before steady teardown churn allocs; do
                        printf '%-8s %13.1f %13.1f %10.3f %10.3f %10.1f %10.1f %9.1f %9.1f %9.1f %11.1f %10.0f\n' \
                            "$workers" "$control_fps" "$profiled_fps" "$control_cpu" "$profiled_cpu" \
                            "$control_rss" "$profiled_rss" "$before" "$steady" "$teardown" "$churn" "$allocs"
                    done
                ;;
        esac
    done
    case $mode in
        lock) printf '\nMutex stacks (flamegraph folded format): %s/relay/%s/lock-*/%s/lock/mutex.folded\n' "$RUN" "$label" "$PROFILE_WORKLOAD" ;;
        cpu) printf '\nCPU stacks (flamegraph folded format): %s/relay/%s/cpu-*/%s/cpu/cpu.folded\n' "$RUN" "$label" "$PROFILE_WORKLOAD" ;;
        heap) printf '\nHeap stacks (flamegraph folded format): %s/relay/%s/heap-*/%s/heap/{churn,retained,teardown}.folded\n' \
            "$RUN" "$label" "$PROFILE_WORKLOAD" ;;
    esac
}

cd "$ROOT"

if [[ $MODE == --criterion ]]; then
    # Each invocation owns fresh results; previous runs cannot become new points.
    python3 -c 'import json,sys; from pathlib import Path; Path(sys.argv[1]).write_text(json.dumps(sys.argv[2:]))' \
        "$RUN/criterion-command.json" "${@:2}"
    if [[ -n ${BENCH_RUNS:-} ]]; then
        printf '%s\n' "$RUN" >>"$BENCH_RUNS"
    fi
    measure_criterion
    exit
elif [[ $MODE == --audio ]]; then
    run_audio_comparison
    exit 0
elif [[ $MODE == --runtime ]]; then
    run_runtime_comparison
    exit 0
elif [[ $MODE == --profile ]]; then
    run_profiles
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
generate_certificate

CARGO_TARGET_DIR=$CURRENT_TARGET cargo build --locked --release -p moq-relay -p moq-bench
LOAD_BIN=$CURRENT_TARGET/release/moq-bench
HOST_BIN=$CURRENT_TARGET/release/moq-bench-host

if [[ -n $BASE ]]; then
    printf '\nRelay workloads: paired base/current\n'
    for ((round = 1; round <= ROUNDS; round++)); do
        if ((round % 2)); then order=(base current); else order=(current base); fi
        printf '%s\t%s\n' "$round" "${order[*]}" >>"$RUN/execution-order.tsv"
        for case in "${RELAY_CASES[@]}"; do
            for label in "${order[@]}"; do
                if [[ $label == base ]]; then relay=$RUN/moq-relay-base; else relay=$CURRENT_TARGET/release/moq-relay; fi
                run_case "$label/round-$round" "$relay" "$case"
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
