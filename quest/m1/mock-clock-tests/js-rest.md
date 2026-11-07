# [M] Other JS tests run on a mocked clock

## Goal

The JS tests outside `js/net` that wait on real time move to a mocked clock
or an awaited event. See the [epic](/quest/m1/mock-clock-tests/README.md)
for the rules.

## Plan

As of 2026-11-06:

- `js/hang/src/container/consumer*.test.ts`: 36 tests, most through
  `settle(20)` in `consumer.test.ts`. Fix the helper first.
- `js/publish/src/fanout.test.ts`: 9 tests through its `take` and `flush`
  helpers.
- `js/qmux/src/web-socket-stream.test.ts` (3), `js/qmux/src/session.test.ts`,
  and `js/watch/src/retention.test.ts` (2), which the guard found beyond the
  scan.
- `js/clock/src/index.test.ts` (2), `js/json/src/snapshot/snapshot.test.ts`,
  `js/signals/src/index.test.ts`, and
  `test/audio-quality/clients/js/src/capture.test.ts` (2). Its
  `performance.now` stub becomes a `Manual` clock once `capture.ts` reads
  time through `@moq/clock`'s `now()`.

`git grep -n "hostTime();$" -- js test demo ':!js/net'` lists the guard's
remaining opt-outs here.

Public API: none. Wire: none.
