# [XS] A stuck source clock's arrival deltas are exact

## Goal

`js/publish/src/video/processor.test.ts`'s "falls back to arrival time when the source clock is stuck" passes however the wall clock reads when it runs.

## Plan

It failed once in a loaded `just check` on [moq-dev/moq#9228](https://github.com/moq-dev/moq/pull/9228), which touches no `@moq/publish` code, with `Expected: 10000` and `Received: 9999.999999999767`, and passed on its own right after.
The fake worker stamps each frame's arrival as `origin().asUnixMillis() + #at + frames * arrivalStep` (line 48), with `#at` a fractional millisecond read from `now()`, so the 10ms steps sit on a float base near 1.7e12 and their microsecond deltas lose precision depending on that base.
Freeze the base on a value the steps add to exactly, such as a whole millisecond or a mocked `origin()`, rather than loosening the assertion with `toBeCloseTo`; then check whether `TrackProcessor` itself turns arrival milliseconds into microseconds in a way a real browser clock trips over the same way, and fix it there if it does.

Public API: none. Wire: none.
