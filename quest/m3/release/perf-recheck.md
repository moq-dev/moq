# [M] Re-run release performance baseline after m2

## Goal

`main` is measured fresh against `origin/release` once `m2` finishes, `.github/release-perf.md` is updated from the current cut candidate, and every regression (including the 2026-11-23 Nightly paired regressions) is fixed or explicitly accepted with the feature it pays for before 0.22 cuts.

## Plan

Decided 2026-11-25 (maintainer, /quest-run): ✅ keep (rejected: drop); ✅ it blocks the 0.22 cut (rejected: not a blocker).

Found by the 2026-11-23 pre-0.22 performance audit (`890ea4e531`):

- `.github/release-perf.md` was last updated 2026-11-17 (`8c9e13577c`), **479 commits** ago, before the late `m1`/`m2` routing, time-addressing, `Max Age`, and `NodeId` changes landed.
- Nightly [45242597151](https://github.com/moq-dev/moq/actions/runs/45242597151) (`b29b90be29` vs `628ff8199d`) reported **18 confirmed paired regressions** across 3 suites:
  - Relay session benchmarks (6 regressions, +12.0% to +23.3%): `relay_session_churn_sessions`, `relay_session_dash_projects`, `relay_session_delivery_watch` (lite-06 and lite-07), `relay_session_join_broadcasts`, `relay_session_join_relays`.
  - Rust shared bench targets (11 regressions, +12.3% to +28.5%): `annexb-export-frames`, `container-export-idle/{flv,mkv,ts}`, `container-export/flv`.
  - Rust supplemental benchmarks (1 regression).
- Nightly [45459585582](https://github.com/moq-dev/moq/actions/runs/45459585582) (`5d86f05321` vs `b29b90be29`) confirmed 5 more: 4 in the relay session benchmarks and 1 in the Rust shared bench targets.
  - Triaged 2026-11-25: the `relay_session_delivery_fetch_frames` and `fetch_range` regressions bisect to [moq-dev/moq#9422](https://github.com/moq-dev/moq/pull/9422) (+5 to 7% locally on one pinned core, no single hotspot, spread across its new accounting), and it costs `video-reorder/flat/64x256` about 4%, the rest of that +11.1% being noise. `relay_session_dash_projects` is noise (5.0 to 8.3 ms across past Nightlies). Decided 2026-11-25 (maintainer, /quest-plan): ✅ spike kio's waiter cost in [kio-waiter-spike](/quest/m3/kio-waiter-spike.md) (rejected: accept #9422's cost outright; a quest on #9422's accounting alone).

Once `m2` finishes, follow the Release baseline method in [Performance audit](/quest/m3/audit-perf.md):

```bash
BENCH_ARTIFACTS=<scratch> just bench origin/release
bun bench/browser/micro.ts --bench frames --out <scratch>/frames.json base=<release> current=. aa=.
```

Record `/proc/loadavg`, re-run every slower or larger case in alternating rounds with instruction counts (or heap profiles for RSS), fix accidental regressions, record accepted costs with the feature and PR each pays for in `.github/release-perf.md`, and confirm the Nightly paired regressions above are resolved or explained.

Public API: none. Wire: none.

## Required

- [Breaking release readiness](/quest/m2/README.md) - run on the final m2 candidate so the baseline does not go stale again before the cut
