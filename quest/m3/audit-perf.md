# [M] Performance audit

## Goal

Every benchmark that regressed since the previous run is explained, fixed, or
planned, and every hot or fan-out path without a benchmark gets one or a
follow-up. `main` ships no slower than `release`: every case where the cut
this run gates would be slower is fixed or accepted with the feature it pays
for.

## Plan

Recurs each major release: the PR that finishes a run keeps its results in
its own body and re-adds this file at the end of m3's `Required`, behind the
[release cut](/quest/m3/release/cut.md), with Check first rewritten. Fixes land
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
BENCH_ARTIFACTS=<scratch> just bench origin/release
bun bench/browser/micro.ts --bench frames --out <scratch>/frames.json base=<release> current=. aa=.
```

`<release>` is an export of `origin/release` with its own
`bun install --frozen-lockfile`. Re-run every slower case in alternating
rounds and count instructions for any that holds. `main` carries features
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
  comparison rather than raw `gh-pages` series. Dart still needs a recheck
  against the bindings history.
- `track_stamp_groups/append`'s per-group cost (a start-index insert and a
  second track lock per group), then the resume reader, which dominates a
  short Annex B export.
- `catalog::tracks::Rendition::stage` clones and serializes the whole catalog
  on every estimate; validating only the edited entry weakens custom configs,
  so it needs a decision.
- The TS export's `Schedule::index` and `Schedule::admit` scan and sort every
  queued unit per PCR slot, so cost grows with track count.
- e2ee expands the AES-GCM key on every frame in `protect`.
- Unbenchmarked paths, by impact: HLS request fan-out; LOC and CMAF in
  moq-mux's benches, which only use `Container::Legacy`; browser media
  copies; moq-quic loss recovery and real congestion controllers; HTTP
  announcement snapshots; MPEG-TS, MKV, FLV, H.265, and AV1 import with real
  media; JS watch audio ring and publish `Fanout`.
- Single-axis fan-out benches to widen: `session_delivery_paced`,
  `track_fanout_*`, `memory`, and `bench/workloads/fanout*.toml`.

## Required

- [Release cut](/quest/m3/release/cut.md) - the release in flight ships before this run starts
