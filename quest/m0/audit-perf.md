# [M] Performance audit

## Goal

Every benchmark that regressed since the previous run is explained, fixed, or
planned, and every hot or fan-out path without a benchmark gets one or a
follow-up.

## Plan

Recurs once per major release (decided 2026-10-19). The PR that finishes
this run also adds the next one with the same file name at the end of the next
milestone's `Required` (m0 to m1, m1 to m2). The successor keeps the Goal
and this paragraph; the rest of its Plan is rewritten with what this run
learned: baselines, results, and what to check first. A successor placed in m2
also lists the [release cut](/quest/m2/release/cut.md) under `Required`, so it
waits for the release in flight to ship. Only `/quest-delete` ends the
recurrence. Fixes land in the run's PR, one commit each, rather than as follow-ups.
Only a change that needs a maintainer decision (public API, scope) or is
independently landable and too large to review alongside the rest becomes a
follow-up through `/quest-plan`.

Starting points (2026-10-19): there are 55 `[[bench]]` targets across 14 crates
(counted from `rs/*/Cargo.toml`) and 27 JS bench files. Nightly stores their history with
`github-action-benchmark` (`bench/costs`, `bench/rates`), but both
steps set `fail-on-alert: false`, so regressions go unseen. Compare against
that history and against the numbers this Plan records. The audit found
`sh/gh/nightly.sh` skipped 13 targets: moq-e2ee, moq-json, moq-archive and
moq-cli benches, mux `jitter`, and 5 of 6 moq-uring benches. The current fix
schedules all 55 targets and checks the manifest inventory in the existing
nightly tooling tests. Supplemental and io_uring measurements have separate
one-hour jobs. Handwritten allocation, window, and playback-buffer tables
remain in the raw run logs; only Criterion estimates enter the chart report.
Fan-out paths need a sweep over both axes
(AGENTS.md). JS perf is judged in a browser, not node or bun. PR-time gating
belongs to [benchmark regressions in CI](/quest/m2/bench-ci.md); don't
duplicate it.

Audit evidence (2026-10-19, second checkpoint, incomplete):

- History: no night has reached `gh-pages`. The Benchmark site job built
  the full dev shell without the cachix substituter and hit its 15-minute
  timeout (jobs 117680366708 and 118138225497). This run takes only `just`
  from the pinned nixpkgs there, and a nightly tooling test requires
  every dev-shell job to trust the Nix cache. Pages itself is still off; see
  [benchmark trend site](/quest/m2/bench-site.md). Check that `gh-pages`
  exists and gains a commit per night before comparing against it.
- Rust: both the October 18 (`50da59e67`) and October 19 (`ad87ad34e`)
  Nightlies retain `bench-rust` (1,527 series each); the earlier note that
  the 19th had none was wrong. Across 1,682 cost series the median change is
  +0.2% (p5 -5%, p95 +16%). The largest moves go both ways in moq-net:
  `retirement` +36% median over 18 series, `track_wall/repoll/10000x1`
  +125%, moq-transport `session_idle` +40-90%, while
  `session_delivery_group_cost` improved 40-47%. kio `waiter_register_live`
  moved +46% with no kio change. The only moq-net source change between the
  commits is the fetch fix `5c3120840` (frame read and fetch routing), which
  none of `retirement`, `track_wall` or `session_idle` exercises; the rest is
  a Cargo.lock bump (`8f13d08d1`). Both nights ran on Neoverse N2 runners.
  Treat these as run-to-run variance of fat-LTO micro-benches, not
  regressions. A paired local A/B of the two commits was built but stopped:
  at a host load of 46-81 Criterion intervals spanned +/-50%. Re-check once
  `gh-pages` holds five or more nights; a series that steps and stays there
  is a regression to attribute.
- Bindings: five nights of `bench-ffi` (Oct 16, 17 twice, 18, 19) bound the
  noise. Kotlin 1 KiB 4x4 CPU reads 104.5, 146.1, 117.5, 79.1, 122.7 ms/MB,
  so the +55% flagged last checkpoint was a low outlier on the 18th. Go 1 KiB
  1x4 throughput reads 185.7, 205.7, 206.7, 208.0, 181.4 MB/s, back to the
  16th's level. Python swings 10-16% between nights. Dart has only two
  nights; its -12% on small frames sits inside that band, and both nights
  used Dart 3.13.0 (the nixpkgs bump `4109d3893` did not change it). No
  binding regression is established. Recheck Dart against `gh-pages`.
- JS: the Nightly JS leg was cancelled at one hour by the forward fixture,
  which the test-cost audit fixed in
  [#5976](https://github.com/moq-dev/moq/pull/5976). Until a Nightly runs
  with it there is no JS history. Nightly never runs these JS benches:
  net `observed`; json `codec` and `parse`; hang `slow-group`; watch
  `warmup`; clock `timers` (`qmux` and `serve-browser` run, uncharted, in
  the audio-quality job). Add them, with an inventory test like the Rust
  one, only after the leg fits its hour.
  Browser smoke artifacts remain one-round correctness runs.
- Inventory: moq-uring `timers` (`cdff3c8f9`) landed after the Rust
  inventory test and failed it; this run schedules it in the io_uring group.
- Unbenchmarked hot and fan-out paths, ranked (each needs a bench or a
  follow-up quest):
  1. HLS/DASH serving (`rs/moq-hls/src/export/rendition.rs` `segment()`,
     `media_playlist()`, `mpd.rs`): every request refetches and remuxes;
     sweep viewers x renditions x window segments.
  2. Session-level datagram delivery (`lite/publisher.rs` `serve_datagram`,
     `ietf/datagram.rs`): per object per subscriber; add a
     `session_delivery_datagram` group sweeping publishers x viewers.
  3. LOC and CMAF wire containers in moq-mux: `containers` and `export`
     only use `Container::Legacy`; sweep container x frame size x frames.
  4. JS hang container `Consumer` walks every buffered group per frame
     (`consumer.ts` `#updateBuffered`, `#checkMaxDelay`); browser bench.
  5. JS per-sample copies in CMAF decode and LOC encode; `js/loc`,
     `js/msf`, `js/flate` have no benches.
  6. moq-quic loss recovery and real congestion controllers: every bench
     pins `Unlimited`; sweep controller x flight x loss x streams.
  7. Relay `/announced` copies the route table per request (`web.rs`);
     sweep routes x pollers.
  8. JWT verification per connect and fetch (`moq-auth`, no benches);
     sweep algorithm x key-set size.
  9. MPEG-TS import with real repeated media (the CLI bench mostly skips
     null padding), and MKV, FLV, H.265, AV1 importers.
  10. JS watch audio ring buffer and publish `Fanout` (lower impact).
  Single-axis fan-out benches to widen: `session_delivery_paced` (viewers
  only), `track_fanout_*` (subscribers only), `memory` (lite-06 only),
  `bench/workloads/fanout*.toml` (connections only).
- Still required before completing this run: confirm `gh-pages` history
  accumulates and compare at least five nights of Rust and binding series,
  restore the JS leg and its history, and turn the ranked gaps into
  benchmarks or follow-up quests through `/quest-plan`. Keep this quest in
  m0 until then; add its m1 successor only when it completes.
