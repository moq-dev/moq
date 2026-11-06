# [L] js/net tests run on a mocked clock

## Goal

About 120 `js/net` tests stop proving nothing happened by waiting, sleeping
to settle, or asserting on elapsed time. See the
[epic](/quest/m1/mock-clock-tests/README.md) for the rules.

## Plan

Start with the shared helpers, which cover most of them:

- `nextUni` (`src/ietf/publisher.test.ts`) and `nextStream`
  (`src/ietf/subscriber.test.ts`) resolve `undefined` when their timeout
  runs out.
- `closeCode` (`src/ietf/malformed.test.ts`) races `Bun.sleep(50)`.
- `settlesWithin` (`src/lite/track_stream.test.ts`).

Then the rest, by file, as of 2026-11-06: `ietf/publisher` 31,
`ietf/subscriber` 22, `lite/publisher` 19, `ietf/malformed` 10,
`ietf/join` 9, `lite/tail` 8, `lite/track_stream` 7, `ietf/goaway` 2, and
one each in `auth`, `connection/connect`, `connection/reload`,
`ietf/datagram`, `ietf/request_stream`, `ietf/tail`, `lite/connection`,
`tail`, and `track`. The `copy_fill` `idle()` flake belongs to
[test-flakes-2](/quest/m0/test-flakes-2/copy-fill-idle.md); this quest
converts the file's other cases.

Public API: none. Wire: none.

## Required

- [JS tests fail when a real timer fires](/quest/m0/clock-guard/js.md) - `@moq/clock` fails a test whose real-clock timer fires, with today's offenders opted out
