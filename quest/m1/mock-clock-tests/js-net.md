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
converts the file's other cases.

The guard's bare `hostTime();` opt-outs are the tests it saw a real timer
fire in, by file: `lite/publisher` 26, `integration` 15, `ietf/publisher` 9,
`connection/reload` 8, `lite/track_stream` 6, `ietf/malformed` 5,
`lite/datagram-tracks` 4, `ietf/join` 4, `ietf/datagram` 4, `lite/tail` 3,
`ietf/goaway` 3, `connection/connect` 2, `ietf/subscriber` 2, and one each in
`track`, `lite/copy_fill`, `lite/connection`, `ietf/request_stream`, and
`auth`. The scan above also counts waits whose timer rarely fires, so convert
both; `git grep -n "hostTime();$" -- js/net` lists what remains.

Public API: none. Wire: none.
