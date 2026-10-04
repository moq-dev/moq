# [S] The stalled flag holds long enough to act on

## Goal

A rendition's catalog `stalled` flag changes only on a sustained stall or a
sustained recovery, so a publisher stops republishing the catalog on every
flip and viewers on automatic selection stop switching between renditions.

## Plan

`Stalled.Detector` (`js/hang/src/catalog/stalled.ts`,
`rs/hang/src/catalog/stalled.rs`) uses frame-count hysteresis: three late
frame intervals set it, three on-time frames clear it, about 100 ms each way.
`js/publish/src/catalog.ts` treats only jitter and delay as estimates, so each
flip publishes at once, and `js/watch/src/video/source.ts` drops stalled
renditions with no dwell.

Decided (2026-10-04):

- Fix the signal at its source: set and clear thresholds become durations, in
  both detectors, with the same constants. Pick the values from the
  reporter's trace (31 republishes and 11 switches in 40 s) and a test.
- No viewer dwell timer, and `stalled` stays out of the estimate rate limiter,
  which would delay a real stall by up to a second.

Tests: with a mocked clock, a lag that alternates every 100 ms never sets the
flag; a lag held past the set duration sets it once; recovery clears it once.

## Closes

- [#4772](https://github.com/moq-dev/moq/issues/4772) - close this issue when the quest finishes
- [#4776](https://github.com/moq-dev/moq/issues/4776) - close this issue when the quest finishes

## Related

- [Rendition preference](/quest/m1/rendition-preference.md) - reorders the same stalled filter in viewer selection
- [Ladder](/quest/m2/ladder/README.md) - deferred a dwell timer until the catalog state was shown to flap
