# [XS] Recheck HLS segment timing and fetch max_age once lite-07 settles

## Goal

Once lite-07 is published, `moq-hls` gives the same segment bytes and manifest timing for one
segment URL on every edge and relay path, and a segment fetch refuses groups past the track's
`max_age` just as the old `fetch_group()` did. Each gap still there is fixed or turned into a
quest of its own.

## Plan

[moq-dev/moq#9700](https://github.com/moq-dev/moq/pull/9700) lets the exporter read lite-07
`group.end()` (GROUP_END), so a segment lists as soon as its newest group ends at or past the
next grid line. The maintainer accepted three review findings on that change instead of holding
the PR. Each only shows up around pauses or on a mixed-version path:

1. **Different bytes under one URL across edges.** An edge that gets the group over moq-lite-06
   sees no GROUP_END, so its fragment's last sample runs to the next group's start. A lite-07
   edge ends that sample at GROUP_END instead (`Fetch` in `rs/moq-hls/src/export/rendition.rs`,
   `declared.or(content)`).
2. **DASH `S@d` can disagree with its fragment (on pure `lite-07` paths too).** The grid uses GROUP_END for a segment's
   content end only when the watcher sees it before the next group starts
   (`Grid::place` / `Grid::end` in `rs/moq-hls/src/export/grid.rs:448`, `open.stop.unwrap_or(group.media)`). The fragment reads the
   group's own end, so the two diverge whenever the successor arrives before `GROUP_END` even on an all-`lite-07` path.
3. **The resize guard drops the declared end.** `Follow::poll_end` in `rendition.rs:2289-2297` skips
   `grid.end()` while an in-band resize may still be handed to a catalog update. So a final
   off-grid GOP's segment metadata can fall back to an inferred end.

Decided 2026-11-28 (maintainer, `/quest-plan` of the pre-0.22 design-gate audit): ✅ pulled back from `m3` to `m2` so mixed `lite-06`/`lite-07` rollout URL byte divergence and pure `lite-07` DASH `S@d` mismatches are fixed before `0.22.0` ships (reversing [moq-dev/moq#9842](https://github.com/moq-dev/moq/pull/9842)'s move to `m3`).

Recheck once [lite-07 is finalized](/quest/m2/lite07-finalize.md):

- If lite-06 hops can still sit between a publisher and an exporter, (1) and (2) need one
  last-sample rule shared by the fragment and the manifest on every path. One option is to take
  the last sample's duration from in-band durations or a sample-based fallback rather than from
  GROUP_END. If no deployed path mixes versions any more, (1) goes away, and (2) only needs the
  grid to use the end the fragment reads.
- (3) looks like a one-line fix: record the declared end whatever the resize guard decides, and
  only skip early completion.
- Also from [moq-dev/moq#9700](https://github.com/moq-dev/moq/pull/9700): `Fetching::Groups` now fetches a segment with one bounded `SUBSCRIBE`
  instead of `fetch_group()`. Before [moq-dev/moq#9687](https://github.com/moq-dev/moq/pull/9687),
  a bounded subscription served a cached group past `max_age` that `fetch_group` refused as
  `Old`. Confirm on main that a segment fetch no longer serves such a group.

## Required

- [Finalize moq-lite-07](/quest/m2/lite07-finalize.md) - settles which relay paths still carry
  lite-06, which decides whether findings (1) and (2) still happen

## Related

- [HLS server lifecycle](/quest/m3/hls-server-lifecycle.md) - also times segments from wire
  GROUP_END
