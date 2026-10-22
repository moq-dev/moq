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

### What to check first

- History: Nightly stores every series on `gh-pages` (`bench/costs`,
  `bench/rates`), and <https://git.moq.dev/bench/> flags each series that moved
  beyond its own noise. The m0 run had only three Rust nights (Oct 18 to 20)
  and no JS history. Compare at least five nights; a series that steps and
  stays there is a regression to attribute. Re-runs of the Benchmark site job
  add duplicate points for one commit, so compare by commit.
- Bindings: the m0 run found no binding regression across five nights of
  `bench-ffi`; Kotlin and Python swing 10-40% between nights. Dart had only two
  nights, so recheck it against the history.
- JS: the m0 run restored the JS leg (about 3 minutes) and scheduled every JS
  bench, checked by `test_every_js_bench_runs_nightly` in
  `sh/gh/nightly.test.py`. Its first history starts after that run's PR.

### Method: attribute with instruction counts, not wall time

The m0 host ran at load 20-60, where Criterion intervals spanned +/-50% and a
wall-clock bisect blamed the wrong commit. Count instructions instead:

- Build the bench at each commit in a scratch worktree with
  `CARGO_PROFILE_BENCH_STRIP=none` and `CARGO_PROFILE_BENCH_DEBUG=line-tables-only`.
  Touch the sources first, since worktrees sharing one target dir otherwise reuse
  a stale build.
- Wrap the timed routine in a `#[inline(never)] fn profiled_inner` that prints a
  marker after each call, run
  `valgrind --tool=callgrind --toggle-collect='*profiled_inner*' <bench> --bench --profile-time 5 --exact <id>`,
  and divide the collected instructions by the marker count. Pass `--bench`, or
  Criterion runs one test iteration. Run-to-run spread is about 0.5%.
- For a loop with no Criterion fixture, a scratch example run at two iteration
  counts gives instructions per iteration from the difference.
- A bench that is not CPU-bound is not attributable this way:
  `relay_auth_refresh` runs real loopback sockets with 10 samples of 300 ms,
  and moved +27-60% on CI with +0.4% instructions.

### Baselines (m0 run, instructions per iteration)

| Bench | Oct 19 `ad87ad34e` | Oct 20 `f13283b30` | main `4327505086` | m0 PR |
|---|---|---|---|---|
| `track_subscriber_join` loop | 3,318 | 6,950 | 7,135 | 4,492 |
| `annexb-export-frames/1tracks-16frames/256` | 370k | 460k | 492k | 484k |
| `cmaf-export-frames/4tracks-16frames/4096` | 7.75M | 8.08M | 8.21M | 8.18M |
| `catalog_estimate_fanout/hang/tracks_4/4` | 94.5M | 97.4M | ~106M | ~106M |
| `relay_auth_refresh/16/16` | 291.7k | 292.7k | 292.6k | 292.8k |

### Open regressions from the m0 run

The m0 run attributed these, but none has a fix yet. Fix each in this run's PR
with its own regression bench, unless it needs a maintainer decision or is too
large to review alongside the rest; those become `/quest-plan` follow-ups:

1. Export reap goes quadratic,
   [#5942](https://github.com/moq-dev/moq/pull/5942) (`49448d2ef`), +42k on
   Annex B export. The lookahead no longer decodes groups ahead of the cursor,
   so on every `poll_event` the reap of aborted groups in
   `rs/moq-mux/src/container/consumer.rs` now calls `group::Consumer::poll_finished`
   on every unbuffered pending group: about 1,070 calls per 16-frame export, so
   frames x pending groups. Recommendation: reap only where an abort can block
   delivery (a hole at the cursor, or before parking), and add an export bench
   swept over buffered groups so the slope shows.
2. [#6019](https://github.com/moq-dev/moq/pull/6019) (`9f1fc92fe`), reading lower
   buffered groups across route copies, adds +27k to Annex B export, of which
   +12k is `memcpy`: larger values move through the resume read path.
   Recommendation: profile `resume::Reader` and `track::Subscriber::poll_group`
   for by-value moves of the cursor state.
3. `catalog_estimate_fanout` +5% at [#5677](https://github.com/moq-dev/moq/pull/5677)
   (`ad54a9721`, data consumers return timestamps, which reworked the moq-flate
   snapshot consumer), and +4% across `fc0df978c` and `4327505086`
   ([#6074](https://github.com/moq-dev/moq/pull/6074) time anchors,
   [#5543](https://github.com/moq-dev/moq/pull/5543) VR catalog metadata).
   Bisect the second pair first, and treat a larger catalog as intended.
4. `annexb` export also gained +9k in `c389bd051`..`eb6ac57cb` and +10k in
   `75d9bf1c0`..`dcd7d2cbb`, and +27k at `fe722cec9` (#5929), of which the m0 PR recovered 8.5k; attribute the rest only if it persists.
5. The subscribe path's remaining ~1,260 instructions over pre-#5929 stay:
   an inline single-range window ([#6241](https://github.com/moq-dev/moq/pull/6241))
   removed three allocations but regressed join time about 6%, so it was
   dropped (maintainer, 2026-10-22).

### Unbenchmarked hot and fan-out paths, ranked

Each needs a bench, swept over both axes where it fans out, or its own
follow-up quest:

1. [HLS request fan-out](/quest/m2/performance-survey/hls-serving.md) owns
   repeated segment fetch/remux and playlist rendering, swept over viewers,
   renditions and window segments.
2. [Session datagrams](/quest/m2/performance-survey/datagram-delivery.md) owns
   complete adapter delivery across publishers and viewers.
3. LOC and CMAF wire containers in moq-mux: `containers` and `export` only use
   `Container::Legacy`; sweep container x frame size x frames.
4. [Buffered browser groups](/quest/m2/performance-survey/hang-buffered.md)
   owns per-frame scans and bulk skip work in the Hang consumer.
5. [Browser media copies](/quest/m2/performance-survey/browser-media-copies.md)
   owns CMAF sample copies and LOC encode allocation. Generic `js/msf` and
   `js/flate` benchmark coverage remains here.
6. moq-quic loss recovery and real congestion controllers: every bench pins
   `Unlimited`; sweep controller x flight x loss x streams.
7. [Announcement snapshots](/quest/m2/performance-survey/announced-snapshots.md)
   owns HTTP snapshot/rendering costs across routes and pollers.
8. [JWT verification](/quest/m2/performance-survey/jwt-verification.md) owns
   key-source I/O, import, lookup, parsing and crypto attribution.
9. MPEG-TS import with real repeated media (the CLI bench mostly skips null
   padding), and MKV, FLV, H.265, AV1 importers.
10. JS watch audio ring buffer and publish `Fanout` (lower impact). Include
    capture-to-encoder staging: `audio/framer.ts` copies planes before
    `audio/encoder.ts` joins them. AudioData already uses a transfer list;
    do not count that transfer as another mandatory copy. Sweep publishers
    x renditions, channels and frame duration in a browser.

The [2026-10-22 source survey](/quest/m2/performance-survey/README.md) owns
the extracted items above and additional algorithmic hypotheses, with paired
benchmarks required before optimization. This audit still owns nightly
regression attribution and the remaining coverage; the source survey does not
complete this run. Its fragmented-MKV child specializes item 9's MKV work.

Single-axis fan-out benches to widen: `session_delivery_paced` (viewers only),
`track_fanout_*` (subscribers only), `memory` (lite-06 only), and
`bench/workloads/fanout*.toml` (connections only).

Fan-out paths need a sweep over both axes (AGENTS.md). JS perf is judged in a
browser, not node or bun. Benchmarks never gate the merge queue (decided
2026-10-20, maintainer): each Nightly compares them into the git.moq.dev
report, which the [nightly benchmark regression report](/quest/m1/bench-ci.md)
owns; don't duplicate it.
