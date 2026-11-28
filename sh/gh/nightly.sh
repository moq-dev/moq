#!/usr/bin/env bash
# Keep benchmark builds together while unrelated Nightly workloads run independently.
set -euo pipefail

usage='usage: just gh nightly rust-test|rust-test-apps|rust-bench|rust-session|rust-track|rust-shared|rust-relay|rust-extra|rust-uring|js|bindings'
if [[ $# != 1 ]]; then
    echo "$usage" >&2
    exit 2
fi
# The measured groups write a report for the benchmark site; bindings only run, since
# rust2 measures them (see bench/README.md).
case "$1" in
    bindings) report= ;;
    rust-bench | rust-session | rust-track | rust-shared | rust-relay | rust-extra | rust-uring) report=rust ;;
    rust-test | rust-test-apps) report=rust-test ;;
    js) report=js ;;
    *)
        echo "$usage" >&2
        exit 2
        ;;
esac

# BENCH_BASE pairs every Rust benchmark with its build at that revision, timed on the same
# machine; `previous-nightly` is the commit of the latest other scheduled Nightly on main.
base=
if [[ -n ${BENCH_BASE:-} ]]; then
    if [[ $report != rust ]]; then
        echo "nightly: BENCH_BASE applies only to the Rust bench groups" >&2
        exit 2
    fi
    if [[ $BENCH_BASE == previous-nightly ]]; then
        BENCH_BASE=$(bash "$(dirname "${BASH_SOURCE[0]}")/previous-nightly.sh") || exit 1
    fi
    # A shallow CI checkout has only its own commit. Fetching a branch name only updates
    # FETCH_HEAD, so the base is passed on as the commit it resolved to.
    if ! base=$(git rev-parse --verify --quiet "$BENCH_BASE^{commit}"); then
        git fetch --no-tags --depth=1 origin "$BENCH_BASE"
        base=$(git rev-parse --verify 'FETCH_HEAD^{commit}')
    fi
fi

if [[ -n $report ]]; then
    root=$(git rev-parse --show-toplevel)
    out=${BENCH_RESULTS:-$root/target/bench/nightly/$1}
    mkdir -p "$out"
    rm -rf "$out/report.json" "$out/paired.json" "$out/raw" "$out/swole"
    mkdir -p "$out/raw" "$out/swole"
fi

# shellcheck source=sh/checks.sh
source "$(dirname "${BASH_SOURCE[0]}")/../checks.sh"

# Time one label's benchmarks with swole, paired with the base when there is one, keeping
# every measure in its report (see bench/README.md).
swole() {
    local label=$1
    shift
    run nix run --inputs-from "$root" swole -- bench ${base:+"$base"} --report "$out/swole/$label.json" -- --locked "$@"
}

javascript() {
    local label=$1
    shift
    echo "nightly: $*"
    if ! "$@" | tee "$out/raw/$label.csv"; then failed+=("$*"); fi
}

# The bench nextest profile stores each test's printed tables in its JUnit report,
# which every run overwrites; keep each run's copy.
nextest() {
    local junit=${CARGO_TARGET_DIR:-$root/target}/nextest/bench/junit.xml report name
    rm -f "$junit"
    run "$@"
    if [[ ! -s $junit ]]; then
        failed+=("no JUnit report from: $*")
        return
    fi
    report=$out/raw/nextest-$((++nextest_runs)).xml
    mv "$junit" "$report"
    # A filter that matches nothing passes, as when its test needs a feature the build lacks.
    while read -r name; do
        if ! grep -qE "<testcase name=\"([^\"]*::)?$name(::[^\"]*)?\"" "$report"; then
            failed+=("no test ran for $name in: $*")
        fi
    done < <(grep -oE 'test\(=?[[:alnum:]_:]+\)' <<<"$*" | sed -E 's/^test\(=?//; s/\)$//')
}
nextest_runs=0

case "$1" in
    # Ignored benchmark tests under nextest, all in release builds and one at a time, so
    # none measures beside another. Each prints CSV tables whose unit-suffixed columns
    # the report records (see bench/README.md). A release build links every test binary
    # with fat LTO, so each group is one build of only the targets holding its benchmarks,
    # and the transport stack and the crates above it split across jobs.
    rust-test)
        export NEXTEST_PROFILE=bench NEXTEST_TEST_THREADS=1
        nextest just rs bench-tests --no-fail-fast --run-ignored only \
            -p moq-bench -p moq-congestion -p moq-quic -p moq-net -p moq-tokio -p moq-uring \
            --features moq-net/async,moq-congestion/lab --lib --bin moq-bench --test integration \
            -E 'test(benchmark_observation_contention) | test(benchmark_readiness_establishment) | test(benchmark_per_packet) | test(benchmark_feed_fetch_wakes) | test(=mesh_sweep::benchmark_route_mesh) | test(benchmark_route_rules) | test(benchmark_route_churn) | test(benchmark_receive_credit) | test(benchmark_receive_windows) | test(benchmark_join_lag) | test(benchmark_datagram_delivery) | test(benchmark_socket_queues)'
        ;;
    rust-test-apps)
        export NEXTEST_PROFILE=bench NEXTEST_TEST_THREADS=1
        nextest just rs bench-tests --no-fail-fast --run-ignored only \
            -p moq-audio -p moq-mux -p libmoq -p moq-relay -p moq-cli -p moq-ffi \
            --features moq-audio/playback --lib --bin moq \
            -E 'test(benchmark_mixer_callbacks) | test(benchmark_resampler) | test(benchmark_raw_audio) | test(benchmark_damage_pid_fanout) | test(benchmark_pes_staging) | test(benchmark_handles) | test(benchmark_stream_import) | test(benchmark_timebase_reservation_fanout)'
        ;;
    # Cargo bench targets of swole's harness. A paired run builds and times each group
    # twice, so the groups split across jobs to fit an hour each.
    # moq-net's session and track targets hold most of its benchmarks, many with setups of
    # seconds that each side's process runs, so each takes a job of its own.
    rust-bench)
        # Fuzz exposes moq-net's codec benches, and moq-quic's plaintext crypto in rust-shared.
        swole net -p moq-net --features fuzz \
            --bench origin --bench viewers --bench announce --bench synced --bench interests \
            --bench routes --bench codec --bench parameters --bench varint --bench frame --bench ranges \
            --bench backing --bench group --bench tracks --bench demand --bench tail --bench publisher \
            --bench priority --bench schedule --bench stats --bench admission --bench refresh \
            --bench bandwidth --bench retirement --bench auth --bench memory --bench path --bench canonical \
            --bench edges
        swole kio -p kio --benches
        ;;
    rust-session)
        swole net-session -p moq-net --features fuzz --bench session
        ;;
    rust-track)
        swole net-track -p moq-net --features fuzz --bench track
        ;;
    rust-shared)
        # One invocation unifies this release build instead of rebuilding shared crates
        # independently for each target. Keep the explicit targets: --benches would add coverage.
        swole shared -p moq-relay -p moq-stats -p moq-mux -p moq-rtmp -p moq-uring -p moq-pattern \
            -p moq-quic --features moq-stats/bench,moq-quic/fuzz \
            --bench http_idle --bench auth --bench http_auth --bench producer_tick --bench decode --bench aggregate \
            --bench catalog --bench ts_schedule --bench export --bench containers --bench matching \
            --bench chunk_interleaving --bench worker_turns \
            --bench streams --bench receive --bench endpoint --bench wire
        ;;
    rust-relay)
        swole relay -p moq-relay --features test-support --bench session
        ;;
    # Keep added coverage out of the rust-bench and rust-shared legs. qmux and moq-auth are
    # selected by manifest, which swole finds in the base's worktree only by an absolute path.
    rust-extra)
        swole qmux --manifest-path "$root/rs/qmux/Cargo.toml" --bench receive
        swole msf -p moq-msf --bench catalog
        # moq-gst's catalog bench shares a target name with moq-mux's, so it runs on its own.
        swole gst -p moq-gst --bench catalog
        swole extra -p moq-e2ee -p moq-json -p moq-archive -p moq-cli -p moq-mux -p moq-audio \
            -p moq-quic -p moq-sock -p moq-tokio -p moq-transcode -p moq-hls -p moq-video \
            --features moq-json/bench,moq-quic/fuzz,moq-video/openh264 \
            --bench protect --bench codec --bench table --bench allocations --bench window \
            --bench producer --bench play_buffer --bench jitter --bench sync --bench decode_clock --bench ts_programs --bench ts_video \
            --bench bus --bench batches --bench parameter_sets --bench sent_packets --bench demux --bench receive_bursts --bench ladder --bench fanout --bench include \
            --bench openh264
        swole audio-capture -p moq-audio --features capture --bench capture
        # moq-auth's published release is its own dev-dependency, so `-p moq-auth` is ambiguous.
        swole auth --manifest-path "$root/rs/moq-auth/Cargo.toml" --bench verify --bench jwt
        ;;
    rust-uring)
        # One run per target makes an unsupported/skipped ring fail report
        # validation instead of hiding behind the Tokio target's measurements.
        for target in udp_tokio udp_uring echo_quic session_lite unreachable_dials timers web_headers; do
            swole "uring-$target" -p moq-uring --bench "$target"
        done
        ;;
    js)
        for benchmark in broadcasts requests fills fill-probe forward reader frames drift ledger bandwidth replay varint lite-varint track resume priority subscribe-drop window-drop publisher-update refresh observed compression datagrams routed-claims; do
            javascript "$benchmark" just js bench "$benchmark"
        done
        javascript catalog just js catalog-bench
        javascript qmux-record just js qmux-record-bench
        javascript qmux-datagrams just js qmux-datagrams-bench
        javascript cmaf just js cmaf-bench
        javascript e2ee just js e2ee-bench
        javascript json-stream just js json-stream-bench
        javascript signals just js signals-bench
        javascript json-codec just js json-codec-bench
        javascript json-parse just js json-parse-bench
        javascript slow-group just js slow-group-bench
        javascript buffered just js buffered-bench
        javascript jitter just js jitter-bench
        javascript audio-warmup just js audio-warmup-bench
        javascript audio-support just js audio-support-bench
        javascript pattern just js pattern-bench
        javascript clock just js clock-bench
        ;;
    # The binding benchmark's runners at a token budget, so they keep working at this
    # commit; rust2's `just bench-ffi` measures them at its pinned moq revision. Swift
    # runs on macOS in the Platform workflow.
    bindings)
        export MOQ_BENCH_SECONDS=0.05
        MOQ_BENCH_DIR=${BENCH_RESULTS:-$(git rev-parse --show-toplevel)/target/bench/nightly/bindings}
        export MOQ_BENCH_DIR
        rm -rf "$MOQ_BENCH_DIR"
        run just rs test -p moq-ffi --cargo-profile release -E 'test(bench::benchmark_ffi) | test(bench::benchmark_background)' --run-ignored only
        run just go bench -benchtime="${MOQ_BENCH_SECONDS}s"
        for runner in py kt cpp dart; do
            run just "$runner" bench
        done
        for result in rust-ffi rust-net rust-background go python kotlin cpp dart; do
            if [[ ! -s $MOQ_BENCH_DIR/$result.json ]]; then failed+=("no $MOQ_BENCH_DIR/$result.json"); fi
        done
        ;;
esac

finish

if [[ -n $report ]]; then
    just gh benchmark-report "$report" "$out" "$out/report.json"
fi
