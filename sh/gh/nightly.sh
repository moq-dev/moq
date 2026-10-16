#!/usr/bin/env bash
# Keep benchmark builds together while unrelated Nightly workloads run independently.
set -euo pipefail

usage='usage: just gh nightly rust|js'
if [[ $# != 1 ]]; then
    echo "$usage" >&2
    exit 2
fi

failed=()
run() {
    echo "nightly: $*"
    if ! "$@"; then failed+=("$*"); fi
}

case "${1:-}" in
    rust)
        run just rs bench-latency
        run just rs test -p moq-bench --release --run-ignored only \
            -E 'test(benchmark_readiness_establishment)' --success-output immediate
        run just rs bench-playback
        run just rs test --release -p moq-quic -E 'test(benchmark_ack_cleanup)' \
            --run-ignored only --success-output immediate
        run just rs bench-feed-fetch
        run just rs test -p moq-relay -E 'test(benchmark_stun_sources)' --run-ignored only --success-output immediate
        run just rs test -p moq-quic --release -E 'test(benchmark_receive_credit)' --run-ignored only --success-output immediate
        # Fuzz exposes moq-net's codec benches; it is not a feature of the other crates.
        run cargo bench --locked --no-fail-fast -p moq-net --features fuzz --bench '*' -- --test
        # Exercise the optional thread-CPU clock as well as the normal elapsed-time smoke.
        run env SESSION_CPU=1 cargo bench --locked -p moq-net --bench session -- session_delivery_group_cost_cpu --test
        # Each kio routine once, including the contended bench's worker threads.
        run cargo bench --locked --no-fail-fast -p kio --bench '*' -- --test
        # One invocation unifies this release build instead of rebuilding shared crates
        # independently for each target. Keep the explicit targets: '*' would add coverage.
        run cargo bench --locked --no-fail-fast -p moq-relay -p moq-stats -p moq-mux -p moq-rtmp -p moq-uring -p moq-pattern \
            --features moq-stats/bench \
            --bench http_idle --bench auth --bench producer_tick --bench decode --bench aggregate \
            --bench catalog --bench ts_schedule --bench export --bench containers --bench matching \
            --bench chunk_interleaving --bench worker_turns -- --test
        run just rs test -p moq-archive -p moq-mux -p moq-hls --no-fail-fast \
            -E 'test(benchmark_timeline_fetch_fanout) | test(benchmark_damage_pid_fanout) | test(benchmark_pes_staging) | test(benchmark_reference_handoff_fanout)' \
            --run-ignored only --success-output immediate
        # Codecs are irrelevant to timebase reservation, so skip building them.
        run just rs test -p moq-ffi --no-default-features -E 'test(benchmark_timebase_reservation_fanout)' \
            --run-ignored only --success-output immediate
        # moq-gst's catalog bench shares a target name with moq-mux's, so it runs on its own.
        run just rs bench-gst-catalog -- --quick
        run just rs test -p moq-cli -E 'test(benchmark_stream_import)' --run-ignored only --success-output immediate
        ;;
    js)
        for benchmark in broadcasts request-ids requests forward reader frames varint lite-varint track interests priority; do
            run just js bench "$benchmark"
        done
        run just js catalog-bench
        run just js arrivals-bench
        run just js p2p-bench
        run just js signals-bench
        ;;
    *)
        echo "$usage" >&2
        exit 2
        ;;
esac

if ((${#failed[@]})); then
    printf 'nightly: failed: %s\n' "${failed[@]}" >&2
    exit 1
fi
