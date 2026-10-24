#!/usr/bin/env bash
# Keep benchmark builds together while unrelated Nightly workloads run independently.
set -euo pipefail

usage='usage: just gh nightly rust-test|rust-bench|rust-shared|rust-extra|rust-uring|js'
if [[ $# != 1 ]]; then
    echo "$usage" >&2
    exit 2
fi
# The measured groups write a report for the benchmark site; rust-test only runs.
case "$1" in
    rust-test) report= ;;
    rust-bench | rust-shared | rust-extra | rust-uring) report=rust ;;
    js) report=js ;;
    *)
        echo "$usage" >&2
        exit 2
        ;;
esac

# MOQ_BENCH_BASE pairs every Rust bench target with its build at that revision, run on the
# same machine; `previous-nightly` is the commit of the latest other scheduled Nightly on main.
if [[ -n ${MOQ_BENCH_BASE:-} ]]; then
    if [[ $report != rust ]]; then
        echo "nightly: MOQ_BENCH_BASE applies only to the Rust bench groups" >&2
        exit 2
    fi
    if [[ $MOQ_BENCH_BASE == previous-nightly ]]; then
        MOQ_BENCH_BASE=$(gh run list --workflow nightly.yml --branch main --event schedule --limit 2 \
            --json databaseId,headSha --jq "map(select(.databaseId != ${GITHUB_RUN_ID:-0}))[0].headSha // empty")
        if [[ -z $MOQ_BENCH_BASE ]]; then
            echo "nightly: no previous scheduled Nightly on main" >&2
            exit 1
        fi
    fi
    # A shallow CI checkout has only its own commit. Fetching a branch name only updates
    # FETCH_HEAD, so the base is exported as the commit it resolved to.
    if ! base=$(git rev-parse --verify --quiet "$MOQ_BENCH_BASE^{commit}"); then
        git fetch --no-tags --depth=1 origin "$MOQ_BENCH_BASE"
        base=$(git rev-parse --verify 'FETCH_HEAD^{commit}')
    fi
    export MOQ_BENCH_BASE=$base
fi

if [[ -n $report ]]; then
    root=$(git rev-parse --show-toplevel)
    out=${MOQ_BENCH_RESULTS:-$root/target/bench/nightly/$1}
    mkdir -p "$out"
    export MOQ_BENCH_RUNS="$out/criterion-runs.txt"
    : >"$MOQ_BENCH_RUNS"
    rm -f "$out/report.json" "$out/paired.json"
fi

failed=()
run() {
    echo "nightly: $*"
    if ! "$@"; then failed+=("$*"); fi
}

criterion() {
    local label=$1
    shift
    run env MOQ_BENCH_LABEL="$label" MOQ_BENCH_ARTIFACTS="$out/runs" just gh criterion "$@"
}

javascript() {
    local label=$1
    shift
    mkdir -p "$out/raw"
    echo "nightly: $*"
    if ! "$@" | tee "$out/raw/$label.csv"; then failed+=("$*"); fi
}

case "$1" in
    # Ignored benchmark tests under nextest: the release builds first, then the test-profile ones.
    rust-test)
        run just rs bench-latency
        run just rs test -p moq-bench --release --run-ignored only \
            -E 'test(benchmark_readiness_establishment)' --success-output immediate
        run just rs bench-playback
        run just rs test --release -p moq-congestion -E 'test(benchmark_ack_cleanup)' \
            --run-ignored only --success-output immediate
        run just rs bench-feed-fetch
        run just rs test -p moq-quic --release -E 'test(benchmark_receive_credit)' --run-ignored only --success-output immediate
        run just rs test -p moq-tokio --release -E 'test(benchmark_receive_windows)' --run-ignored only --success-output immediate
        run just rs test -p moq-relay -E 'test(benchmark_stun_sources)' --run-ignored only --success-output immediate
        run just rs test -p moq-archive -p moq-mux -p moq-hls --no-fail-fast \
            -E 'test(benchmark_timeline_fetch_fanout) | test(benchmark_damage_pid_fanout) | test(benchmark_pes_staging) | test(benchmark_reference_handoff_fanout)' \
            --run-ignored only --success-output immediate
        # Codecs are irrelevant to timebase reservation, so skip building them.
        run just rs test -p moq-ffi --no-default-features -E 'test(benchmark_timebase_reservation_fanout)' \
            --run-ignored only --success-output immediate
        run just rs test -p moq-cli -E 'test(benchmark_stream_import)' --run-ignored only --success-output immediate
        ;;
    # Measured Cargo bench targets, all in the bench profile. A paired run builds and
    # measures each group twice, so the groups split across jobs to fit an hour each.
    rust-bench)
        # Fuzz exposes moq-net's codec benches, and moq-quic's plaintext crypto in rust-shared.
        criterion net --no-fail-fast -p moq-net --features fuzz --bench '*'
        # Measure the optional thread-CPU clock as well as the normal elapsed time, reusing that build.
        SESSION_CPU=1 criterion net-cpu -p moq-net --features fuzz --bench session -- session_delivery_group_cost_cpu
        # Measure each kio routine, including the contended bench's worker threads.
        criterion kio --no-fail-fast -p kio --bench '*'
        # moq-gst's catalog bench shares a target name with moq-mux's, so it runs on its own.
        criterion gst -p moq-gst --bench catalog
        ;;
    rust-shared)
        # One invocation unifies this release build instead of rebuilding shared crates
        # independently for each target. Keep the explicit targets: '*' would add coverage.
        criterion shared --no-fail-fast -p moq-relay -p moq-stats -p moq-mux -p moq-rtmp -p moq-uring -p moq-pattern \
            -p moq-quic --features moq-stats/bench,moq-quic/fuzz \
            --bench http_idle --bench auth --bench http_auth --bench producer_tick --bench decode --bench aggregate \
            --bench catalog --bench ts_schedule --bench export --bench containers --bench matching \
            --bench chunk_interleaving --bench worker_turns \
            --bench streams --bench receive --bench endpoint --bench wire
        ;;
    # Keep added coverage out of the rust-bench and rust-shared legs.
    rust-extra)
        # The allocation/window/play-buffer tables keep their stdout in the run;
        # only Criterion estimates enter the trend report.
        criterion extra --no-fail-fast -p moq-e2ee -p moq-json -p moq-archive -p moq-cli -p moq-mux \
            --bench protect --bench codec --bench table --bench allocations --bench window \
            --bench producer --bench play_buffer --bench jitter
        ;;
    rust-uring)
        # One run per target makes an unsupported/skipped ring fail report
        # validation instead of hiding behind the Tokio target's measurements.
        for target in udp_tokio udp_uring echo_quic session_lite unreachable_dials timers; do
            criterion "uring-$target" -p moq-uring --bench "$target"
        done
        ;;
    js)
        for benchmark in broadcasts request-ids requests forward reader frames drift varint lite-varint track resume interests priority subscribe-drop subscribe-tracks refresh observed compression; do
            javascript "$benchmark" just js bench "$benchmark"
        done
        javascript catalog just js catalog-bench
        javascript cmaf just js cmaf-bench
        javascript arrivals just js arrivals-bench
        javascript p2p just js p2p-bench
        javascript e2ee just js e2ee-bench
        javascript json-stream just js json-stream-bench
        javascript signals just js signals-bench
        javascript json-codec just js json-codec-bench
        javascript json-parse just js json-parse-bench
        javascript slow-group just js slow-group-bench
        javascript jitter just js jitter-bench
        javascript audio-warmup just js audio-warmup-bench
        javascript clock just js clock-bench
        ;;
esac

if ((${#failed[@]})); then
    printf 'nightly: failed: %s\n' "${failed[@]}" >&2
    exit 1
fi

if [[ -n $report ]]; then
    just gh benchmark-report "$report" "$out" "$out/report.json"
fi
