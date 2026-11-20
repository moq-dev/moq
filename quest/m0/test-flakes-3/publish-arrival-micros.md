# [XS] A stuck video source's arrival timestamps are whole microseconds

## Goal

`@moq/publish`'s `falls back to arrival time when the source clock is stuck`
(`js/publish/src/video/processor.test.ts`) passes every run, and the frames
the fallback stamps carry whole-microsecond timestamps.

## Plan

Seen 2026-11-20 on `main` (9828a20cab): `bun test src/video/processor.test.ts`
in `js/publish` failed 3 of 5 runs, with a delta of `10000.000000000233`
against `10000`. The arrival comes from `msg.at - performance.timeOrigin`, two
Unix-scale millisecond floats, so the microseconds `MediaClock.capture(at).asMicros()`
returns carry float error that depends on the wall clock. A `VideoFrame`
timestamp is an integer, so a browser truncates it, and a frame can land 1µs
early. Round to whole microseconds where the processor stamps a frame (the
worker path and the `<video>` fallback alike) rather than loosen the test.
Find the PR that let the fraction through by blaming those lines.

Public API: none. Wire: none.
