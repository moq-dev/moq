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

### What the m1 run (2026-10-22) fixed

Instruction counts are one optimized `--test --exact` iteration under callgrind,
collected inside a profiling boundary (see Method).

- The export consumer reaps queued aborted groups only when delivery blocks,
  not once per delivered frame. `annexb-export-queued-groups` (in the
  `containers` bench) sweeps queued groups x frames per group:
  64 groups x 16 frames went from 12.36M to 4.47M (-64%); one group stays
  within 1%.
- `group::Consumer` was 672 bytes because its prefetch batch held eight frames
  inline, and every layer that hands out a group (track cursor, subscriber,
  resume reader, mux consumer) moved it by value. The batch is now boxed on its
  first fill: `annexb-export-queued-groups/64/1` -14.8%,
  `track_recv_groups/arrival/512` -15.2%, `group_read_frames/single/4` +0.6%.
  This recovers the m0 run's `memcpy` attribution to
  [#6019](https://github.com/moq-dev/moq/pull/6019).
- Each catalog estimate read its rendition through a whole-catalog snapshot
  three times, so every frame cloned every rendition. It now clones only the
  entry: `catalog_estimate_fanout/hang/tracks_4/4` 106.2M to 59.0M (-44%),
  `tracks_16/16` -53%.

### What it attributed without a fix

- `catalog_estimate_fanout` +4% across `fc0df978c`..`4327505086`:
  [#6074](https://github.com/moq-dev/moq/pull/6074) +0.4%,
  [#5543](https://github.com/moq-dev/moq/pull/5543) +3.3% (a larger
  `VideoConfig` cloned on every estimate, which the fix above removed). The
  [#5677](https://github.com/moq-dev/moq/pull/5677) step was not bisected;
  the bench is now well below its Oct 19 baseline.
- Annex B export: per frame it is now below Oct 19 (`1tracks-64frames/256`
  1.185M vs 1.495M), but a per-export constant remains (+18.6k at one frame):
  +8.8k catalog decode, +11k `track::Subscriber::poll_group` through the
  resume reader, +2.6k track resolve. The duplicate-key check
  ([#5941](https://github.com/moq-dev/moq/pull/5941)) replaced an equal-cost
  merge check, so it adds nothing.
- `track_stamp_groups/append` (nightly +29% on all three depths, stayed):
  6,557 instructions per group at Oct 20 `f13283b302`, 7,729 at Oct 21
  `bc64b9c525`, 8,944 at this run. The first step is #6074's start index
  (`TrackState::index_start`, a `BTreeMap<u64, Timestamp>` insert per group);
  the second is `group::Handle::start` and `TrackState::wake_landed` taking the
  track lock again on each group's first frame, partly offset by a cheaper
  `write_frame`.

Published history had only three distinct commits (Oct 20 `f13283b302`, which
has two reruns, Oct 21 `bc64b9c525`, Oct 22 `33940b6e33`), so the five-night
comparison moved here. Across those three, 13 of 1,756 Rust and FFI cost series
stayed over 25% worse on both later nights and 8 stayed over 25% better, which
is close to noise. The worse ones beyond the above: `track_pending_tail/repoll`,
`session_churn_held/moq-transport-22`, `lite_varint/encode/Datagram`,
`origin/announce/1000p_1000s`, `kio/contended_repoll` (threaded, not
CPU-attributable), and Kotlin FFI (swings 10-40% nightly). Throughput series
did not move.

### What to check first

- History: compare at least five nights by commit, now that it exists. Nightly
  stores every series on `gh-pages` (`bench/costs`, `bench/rates`); re-runs add
  duplicate points for one commit. Read the
  [nightly benchmark regression report](/quest/m1/bench-ci.md)'s paired flags
  instead of raw history once it lands. The first JS series (841) appeared on
  Oct 22, and Dart still needs a recheck against the bindings history.
- The per-group write cost above, then the resume reader: at 16 single-frame
  groups `track::Subscriber::poll_group` is 44% of the Annex B export
  (174k of 397k), mostly `resume::Reader::poll_lowest`.
- `catalog::tracks::Rendition::stage` still clones and serializes the whole
  catalog on every estimate to validate it before retaining it (62% of
  `catalog_estimate_fanout/hang/tracks_4/4`). Validating only the edited entry
  would weaken that guarantee for custom configs, so it needs a decision.

### Method: attribute with instruction counts, not wall time

Shared hosts run at load 20-60, where Criterion intervals span +/-50% and a
wall-clock bisect blames the wrong commit. Count instructions instead:

- Export each commit with `git archive` into a scratch directory and build the
  bench there with its own `CARGO_TARGET_DIR`, the current dev shell's
  toolchain, `CARGO_PROFILE_BENCH_STRIP=none` and
  `CARGO_PROFILE_BENCH_DEBUG=line-tables-only`.
- Wrap the timed routine in an `#[inline(never)]` function (the `containers`
  bench's `export_queued_groups` is one) and run
  `valgrind --tool=callgrind --toggle-collect='*<boundary>*' <bench> --test --exact <id>`.
  One iteration is deterministic to about 1%; for steady state, print a marker
  per call and run `--bench --profile-time 5` instead. Without a boundary,
  toggle on the operations the setup repeats (`append_group`, `write_frame`,
  `finish` for `track_stamp_groups`) and divide by the count.
- `callgrind_annotate --inclusive=yes` diffs between two commits point at the
  step; workspace sources show as `${workspace}`, so read them by name.
- A bench that is not CPU-bound is not attributable this way:
  `relay_auth_refresh` runs real loopback sockets and moved +27-60% on CI with
  +0.4% instructions.

### Baselines (m1 run, instructions per iteration)

| Bench | Earliest measured | m1 PR |
|---|---|---|
| `annexb-export-frames/1tracks-16frames/256` | 374k (Oct 19 `ad87ad34e`) | 397k |
| `annexb-export-frames/1tracks-64frames/256` | 1.495M (Oct 19 `ad87ad34e`) | 1.185M |
| `cmaf-export-frames/4tracks-16frames/4096` | 7.77M (Oct 19 `ad87ad34e`) | 7.87M |
| `catalog_estimate_fanout/hang/tracks_4/4` | 102.1M (`fecb57131`, before #6074) | 59.0M |
| `track_stamp_groups/append/512`, per group | 6,557 (Oct 20 `f13283b30`) | 8,944 |
| `track_recv_groups/arrival/512` | 3.27M (main `2e3ea8484`) | 2.78M |

### Unbenchmarked hot and fan-out paths, ranked

Each needs a bench, swept over both axes where it fans out, or its own
follow-up quest. The m1 run added none of these:

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
regression attribution and the remaining coverage. Its fragmented-MKV child
specializes item 9's MKV work.

Single-axis fan-out benches to widen: `session_delivery_paced` (viewers only),
`track_fanout_*` (subscribers only), `memory` (lite-06 only), and
`bench/workloads/fanout*.toml` (connections only).

Fan-out paths need a sweep over both axes (AGENTS.md). JS perf is judged in a
browser, not node or bun. Benchmarks never gate the merge queue (decided
2026-10-20, maintainer): each Nightly compares them into the git.moq.dev
report, which the [nightly benchmark regression report](/quest/m1/bench-ci.md)
owns; don't duplicate it.

## Required

- [Cut and publish the prepared breaking release](/quest/m2/release/cut.md) - the release in flight ships before this run starts
