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
- `js/clock/src/index.test.ts` (2), `js/json/src/snapshot/snapshot.test.ts`,
  `js/signals/src/index.test.ts`, and
  `test/audio-quality/clients/js/src/capture.test.ts` (2).

Public API: none. Wire: none.

## Required

- [JS tests fail when a real timer fires](/quest/m0/clock-guard/js.md) - `@moq/clock` fails a test whose real-clock timer fires, with today's offenders opted out
