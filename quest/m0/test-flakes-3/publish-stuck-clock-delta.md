# [XS] A stuck source clock's arrival deltas are exact

## Goal

`js/publish/src/video/processor.test.ts`'s "falls back to arrival time when the source clock is stuck" passes however the wall clock reads when it runs.

## Plan

It failed once in a loaded `just check` on [moq-dev/moq#9228](https://github.com/moq-dev/moq/pull/9228), which touches no `@moq/publish` code, with `Expected: 10000` and `Received: 9999.999999999767`, and passed on its own right after.
The fake worker stamps each frame's arrival as `origin().asUnixMillis() + #at + frames * arrivalStep` (line 48), with `#at` a fractional millisecond read from `now()`, so the 10ms steps sit on a float base near 1.7e12 and their microsecond deltas lose precision depending on that base.
It also fails alone: on `main` at 9828a20cab, `bun test src/video/processor.test.ts` in `js/publish` failed 3 of 5 runs with a delta of `10000.000000000233`.
The likely product cause: the arrival comes from `msg.at - performance.timeOrigin`, two Unix-scale millisecond floats, so the microseconds `MediaClock.capture(at).asMicros()` returns carry float error that depends on the wall clock.
A `VideoFrame` timestamp is an integer, so a browser truncates it, and a frame can land 1µs early.
Round to whole microseconds where the processor stamps a frame, the worker path and the `<video>` fallback alike, rather than loosening the assertion with `toBeCloseTo`; then freeze the test's base too if it still depends on the wall clock.
Find the PR that let the fraction through by blaming those lines, for `## Caused by`.

Public API: none. Wire: none.
