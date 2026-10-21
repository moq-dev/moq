#!/usr/bin/env bash
# Keep benchmark builds together while unrelated Nightly workloads run independently.
set -euo pipefail

usage='usage: just gh nightly rust-test|rust-bench|rust-extra|rust-uring|js'
if [[ $# != 1 ]]; then
    echo "$usage" >&2
    exit 2
fi
# The measured groups write a report for the benchmark site; rust-test only runs.
case "$1" in
    rust-test) report= ;;
    rust-bench | rust-extra | rust-uring) report=rust ;;
    js) report=js ;;
    *)
        echo "$usage" >&2
        exit 2
        ;;
esac

if [[ -n $report ]]; then
    root=$(git rev-parse --show-toplevel)
    out=${MOQ_BENCH_RESULTS:-$root/target/bench/nightly/$1}
    mkdir -p "$out"
    export MOQ_BENCH_RUNS="$out/criterion-runs.txt"
    : >"$MOQ_BENCH_RUNS"
    rm -f "$out/report.json"
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
        run just rs test --release -p moq-quic -E 'test(benchmark_ack_cleanup)' \
            --run-ignored only --success-output immediate
        run just rs bench-feed-fetch
        run just rs test -p moq-quic --release -E 'test(benchmark_receive_credit)' --run-ignored only --success-output immediate
        run just rs test -p moq-relay -E 'test(benchmark_stun_sources)' --run-ignored only --success-output immediate
        run just rs test -p moq-archive -p moq-mux -p moq-hls --no-fail-fast \
            -E 'test(benchmark_timeline_fetch_fanout) | test(benchmark_damage_pid_fanout) | test(benchmark_pes_staging) | test(benchmark_reference_handoff_fanout)' \
            --run-ignored only --success-output immediate
        # Codecs are irrelevant to timebase reservation, so skip building them.
        run just rs test -p moq-ffi --no-default-features -E 'test(benchmark_timebase_reservation_fanout)' \
            --run-ignored only --success-output immediate
        run just rs test -p moq-cli -E 'test(benchmark_stream_import)' --run-ignored only --success-output immediate
        ;;
    # Measured Cargo bench targets, all in the bench profile.
    rust-bench)
        # Fuzz exposes moq-net's codec benches, and moq-quic's plaintext crypto below.
        criterion net --no-fail-fast -p moq-net --features fuzz --bench '*'
        # Measure the optional thread-CPU clock as well as the normal elapsed time.
        SESSION_CPU=1 criterion net-cpu -p moq-net --bench session -- session_delivery_group_cost_cpu
        # Measure each kio routine, including the contended bench's worker threads.
        criterion kio --no-fail-fast -p kio --bench '*'
        # One invocation unifies this release build instead of rebuilding shared crates
        # independently for each target. Keep the explicit targets: '*' would add coverage.
        criterion shared --no-fail-fast -p moq-relay -p moq-stats -p moq-mux -p moq-rtmp -p moq-uring -p moq-pattern \
            -p moq-quic --features moq-stats/bench,moq-quic/fuzz \
            --bench http_idle --bench auth --bench producer_tick --bench decode --bench aggregate \
            --bench catalog --bench ts_schedule --bench export --bench containers --bench matching \
            --bench chunk_interleaving --bench worker_turns \
            --bench streams --bench receive --bench endpoint --bench wire
        # moq-gst's catalog bench shares a target name with moq-mux's, so it runs on its own.
        criterion gst -p moq-gst --bench catalog
        ;;
    # Keep added coverage out of the existing forty-minute benchmark leg.
    rust-extra)
        # The allocation/window/play-buffer tables keep their stdout in the run;
        # only Criterion estimates enter the trend report.
        criterion extra --no-fail-fast -p moq-e2ee -p moq-json -p moq-archive -p moq-cli -p moq-mux \
            --bench protect --bench codec --bench table --bench allocations --bench window \
            --bench writer --bench play_buffer --bench jitter
        ;;
    rust-uring)
        # One run per target makes an unsupported/skipped ring fail report
        # validation instead of hiding behind the Tokio target's measurements.
        for target in udp_tokio udp_uring echo_quic session_lite unreachable_dials timers; do
            criterion "uring-$target" -p moq-uring --bench "$target"
        done
        ;;
    js)
        for benchmark in broadcasts request-ids requests forward reader frames varint lite-varint track resume interests priority subscribe-drop refresh; do
            javascript "$benchmark" just js bench "$benchmark"
        done
        javascript catalog just js catalog-bench
        javascript arrivals just js arrivals-bench
        javascript p2p just js p2p-bench
        javascript e2ee just js e2ee-bench
        javascript json-stream just js json-stream-bench
        javascript signals just js signals-bench
        ;;
esac

if ((${#failed[@]})); then
    printf 'nightly: failed: %s\n' "${failed[@]}" >&2
    exit 1
fi

if [[ -n $report ]]; then
    just gh benchmark-report "$report" "$out" "$out/report.json"
fi
