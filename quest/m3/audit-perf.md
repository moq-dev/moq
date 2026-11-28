# [M] Performance audit

## Goal

Every benchmark that regressed since the previous run is explained, fixed, or
planned, and every hot or fan-out path without a benchmark gets one or a
follow-up. `main` ships no slower than `release`: every case where the cut
this run gates would be slower is fixed or accepted with the feature it pays
for.

## Plan

Recurs each major release: the PR that finishes a run keeps its results in
its own body and re-adds this file at the end of m3's `Required`, with its own `Required`
naming the [release cut](/quest/m3/release/cut.md) and Check first rewritten. Fixes land
in that PR; only maintainer decisions (public API, scope) and changes too
large to review alongside become follow-ups.

Fan-out paths need a sweep over both axes, and JS perf is judged in a
browser. Benchmarks never gate the merge queue (decided 2026-10-20,
maintainer); Nightly's [paired comparison](/bench/README.md#paired-comparison)
covers night-to-night drift.

Previous run: [moq-dev/moq#6281](https://github.com/moq-dev/moq/pull/6281),
and the release comparison in [release-perf.md](/.github/release-perf.md).

### Release baseline

The run's last step, once the rest of its milestone has merged, so `main` is
what the cut ships. Pair it with the last cut, keeping the evidence outside
the checkout, and record `/proc/loadavg` around each run:

```bash
BENCH_ARTIFACTS=<scratch> just bench-relay origin/release
bun bench/browser/micro.ts --bench frames --out <scratch>/frames.json base=<release> current=. aa=.
```

`<release>` is an export of `origin/release` with its own
`bun install --frozen-lockfile`. The relay comparison reports relay RSS
(`rss_bytes`) beside CPU, and the report gives both per workload; memory that
grows with connections shows only in `chat`. Re-run every slower or larger case in alternating
rounds and count instructions for any that holds, or take a heap profile
(`just bench-profile heap chat 1 tokio-workers origin/release`) for memory. `main` carries features
`release` lacks, so a flat per-operation cost usually pays for one, while a
cost that grows with `main`'s own sweep is accidental. Fix accidental cost in
this run, and record each accepted cost with the feature and PR it pays for.
A browser row inside the `aa` spread is noise.

### Method

Shared hosts make wall-clock bisects blame the wrong commit, so count
instructions: build each commit from `git archive` with its own
`CARGO_TARGET_DIR`, `CARGO_PROFILE_BENCH_STRIP=none`, and
`CARGO_PROFILE_BENCH_DEBUG=line-tables-only`, wrap the timed routine in an
`#[inline(never)]` boundary, and run
`valgrind --tool=callgrind --toggle-collect='*<boundary>*' <bench> --test --exact <id>`.
Diff `callgrind_annotate --inclusive=yes` between commits. A wall-time hold
at equal instructions may be page faults; a bench that is not CPU-bound,
such as `relay_auth_refresh`, is not attributable this way.

### Check first

- At least five nights of history by commit, read through the paired
  comparison rather than raw `gh-pages` series. Nightly pairs only the Rust
  bench targets: pick JS and Rust benchmark-test candidates from their history
  and compare each against the release separately. Dart still needs a recheck
  against the bindings history.
- `track_stamp_groups/append`'s per-group cost (a start-index insert and a
  second track lock per group), then the resume reader, which dominates a
  short Annex B export.
- The TS export's `Schedule::index` and `Schedule::admit` scan and sort every
  queued unit per PCR slot, so cost grows with track count.
- Verified already fixed (2026-11-23 at `890ea4e531`): `moq-e2ee/src/protect.rs`
  now takes `&aead::Key` (no per-frame key expansion), and `Rendition::stage` is
  gone. Still open at `890ea4e531`: `Lines::make_room`, `Catalog::from_slice`,
  `moq-archive` `Object::encode`, `moq-loc::encode`, `h264/h265/av1` keyframe
  copies, `js/hang/src/cmaf/decode.ts`, `js/qmux` `TransformStream` and
  `Array.shift()`, and `js/signals` `useValue` mount re-render. Pre-0.22 release
  re-measurement is tracked in [perf-recheck](/quest/m3/release/perf-recheck.md).
- Unbenchmarked paths, by impact: HLS request fan-out; LOC and CMAF in
  moq-mux's benches, which only use `Container::Legacy`; browser media
  copies; moq-quic loss recovery and real congestion controllers; HTTP
  announcement snapshots; MPEG-TS, MKV, FLV, H.265, and AV1 import with real
  media; JS watch audio ring and publish `Fanout`.
- From the 2026-11-17 pre-cut correctness audit, from source, each needing a
  measurement before a fix: `Lines::make_room` (`rs/moq-net/src/transport/open.rs`)
  rebuilds the session's preemption map on every poll of a waiting open,
  which a credit-starved viewer of K tracks re-polls per frame (O(K²), from
  #8103); `follow_demand` in `model/origin.rs` locks every reader to rebuild
  demand on each wake (`resume.rs`); `frame.rs` compares a second copy of a
  group byte for byte only to catch a reused identity, which AGENTS.md says
  not to spend cycles on; TS passthrough (`ts/passthrough.rs`) re-sums its
  whole object queue per packet, quadratic when a sender flags every packet
  random-access and sends rare PCRs (#5003); and payload copies: the JS CMAF
  decoder copies each sample twice (`js/hang/src/cmaf/decode.ts`), the FLV
  import each tag body twice, fMP4 and MKV export several times per payload,
  and WebRTC egress twice per frame per viewer (`rs/moq-webrtc/src/egress.rs`).
- From the 2026-11-27 watch replay flake, profiled: `js/hang`'s container
  `Consumer` visits every buffered group on each decoded frame (`#look` tries
  each group's `tryReadFrame`, `#updateBuffered` rebuilds every range with an
  `isSkipped` per pair, then `#checkMaxDelay`), so a long lookahead of
  one-frame audio groups costs O(buffered groups) per frame: about a third of
  the CPU of a replay whose 60 s lookahead held over a thousand groups
  (`js/watch/src/audio/replay.test.ts`'s stalled utterance). Check whether
  `rs/moq-mux`'s consumer shares it.
- Single-axis fan-out benches to widen: `session_delivery_paced`,
  `track_fanout_*`, `memory`, and `bench/workloads/fanout*.toml`.
- From the 2026-11-21 pre-cut full-codebase audit (maintainer, /quest-plan;
  rejected: standalone m4 quests for these, since each needs measurement
  alongside the release perf run):
  - `rs/moq-mux/src/codec/{h264,h265,av1}/import.rs` (`h264/import.rs:307,396-398`,
    `h265/import.rs:310,357-359`, `av1/import.rs:292,628-642`): copies the full
    keyframe `Bytes` on every Annex-B/AV1 keyframe instead of zero-copy
    `Bytes::slice_ref` (cf. `find_hdr` at `av1/import.rs:603`).
  - `rs/hang/src/catalog/root.rs:394-427`: `Catalog::from_slice` parses and
    traverses catalog JSON four times.
  - `rs/moq-archive/src/layout.rs:194-219`: `Object::encode` copies every media
    payload twice.
  - `rs/moq-loc/src/lib.rs:170-181`: `encode` performs two heap allocations per
    frame.
  - `js/qmux/src/web-socket-stream.ts:357-366` and
    `js/qmux/src/scheduler.ts:201,411`: redundant `TransformStream` pipe on
    Chromium's native `WebSocketStream` (`session.ts:936` already normalizes
    `ArrayBuffer`) and `Array.shift()` in `Scheduler.#datagrams` instead of
    `Queue`.
  - `js/signals/src/react.ts:15-34`: `useValue` triggers a redundant synchronous
    re-render on mount for `Derived` signals returning fresh objects.

## Required

- [Release cut](/quest/m3/release/cut.md) - the release in flight ships before this run starts
