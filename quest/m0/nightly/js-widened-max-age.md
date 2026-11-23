# [S] JS widened_max_age test fails in Nightly coverage

## Goal

`js/net/src/lite/widened_max_age.test.ts` passes reliably, so `Nightly (JS coverage)` completes across every JS package instead of aborting in `js/net`.

## Plan

Found in Nightly [45242597151](https://github.com/moq-dev/moq/actions/runs/45242597151) (2026-11-23, `b29b90be29`, job `137930913986`):

```text
js/net/src/lite/widened_max_age.test.ts:165:36
(fail) a widening recovers the stale groups for a bounded lite-07 reader
Expected: [3n, 4n]
Received: [1n, 3n]
```

Because `Nightly (JS coverage)` failed in `js/net`, every JS package after `js/net` (`js/hang`, `js/watch`, `js/publish`, `js/auth`, etc.) produced no coverage data. Reproduce the failure in `js/net/src/lite/widened_max_age.test.ts` (note the test writes groups 1..4 in a synchronous loop after subscribing with `max: 1500ms`, so group 1 may be served before group 4 makes it stale, or a real max-age race exists), fix the root cause in code or test determinism without sleeps or loosened assertions, and confirm `just js coverage` runs every package.

Public API: none. Wire: none.
