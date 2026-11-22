# Pre-cut full-codebase audit fixes

## Goal

Every non-breaking high-severity and impactful medium-severity bug, TLA+
sync/model drift, O(N²) relay auth-refresh scaling bottleneck, and CI/tooling
hardening item found in the 2026-11-21 full-codebase audit is fixed on `main`
without requiring a breaking API or wire change.

## Plan

Decided 2026-11-21 (maintainer, /quest-plan of the exhaustive pre-cut
full-codebase audit, covering both pre-release and new code):

- ✅ All high-severity non-breaking bugs land in `m1` (with `0.22.0` imminent,
  no separate `release` backports; rejected: `m0` with `release` backports;
  `m3`).
- ✅ Medium-severity non-breaking bugs split by impact: protocol, correctness,
  UB, and leak fixes join this `m1` epic, while softer edge cases and spec
  cleanups live in `m3` (rejected: all in `m1`; all in `m3`).
- ✅ TLA+ sync/model drift and `moq-relay`'s `Refresh::ended` O(N²) rebuild
  join this `m1` epic, while other unmeasured perf items fold into
  [audit-perf](/quest/m3/audit-perf.md) and new binary-parser fuzz targets go
  to [m4](/quest/m4/fuzz-binary-parsers.md) (rejected: standalone `m4` perf
  quests; moving all perf/TLA+ items to `m4`).
- ✅ Cut-blocking release/packaging bugs live in `m0` and gate
  [the release cut](/quest/m3/release/cut.md), while smaller CI and tooling
  hardening fixes live here in `m1` (rejected: all 9 in `m1`; only 3 in `m0`
  and dropping the rest).

Each child quest owns one independent subsystem area and merges on its own.
This epic has no work of its own and completes when its last child merges.
Breaking findings from the same audit live in [m2](/quest/m2/README.md).
