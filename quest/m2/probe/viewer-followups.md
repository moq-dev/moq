# [M] Viewer targets are tested end to end and documented in Python

## Goal

Three gaps [moq-dev/moq#7090](https://github.com/moq-dev/moq/pull/7090)
left are closed:

- A `js/net` test shows a viewer's target reaching a fake peer that
  advertises PROBE `Increase` over a pooled connection, and reaching it
  again after the connection reconnects.
- A test in each language stalls the PROBE writer and shows one target in
  flight, with the latest target sent once the writer drains.
- `py/moq-rs/docs/index.md` has a Bandwidth section listing `Allocator`,
  `Reservation`, `BandwidthConsumer`, `Viewers`, and `Viewer`.

## Plan

Decided 2026-10-31 (user, /quest-plan of the follow-ups from the 2026-10-31 /quest-complete run), from [moq-dev/moq#7090](https://github.com/moq-dev/moq/pull/7090),
which finishes [viewer targets](/quest/m2/probe/viewer-target.md).

- Pooled reconnect: the `Reload`/pool `viewers` wiring
  (`js/net/src/connection/reload.ts`) is covered only by type checks and
  the subscriber's wire tests. Drive a reconnect through the pool against a
  fake peer and assert the live handles' sum arrives on the new session.
- Stalled PROBE: [moq-dev/moq#7090](https://github.com/moq-dev/moq/pull/7090) fixed targets piling up behind a blocked PROBE
  writer but couldn't test it, since the test setups can't stall a stream.
  Use a transport double whose PROBE stream stops granting credit (Rust:
  the in-memory session; JS: a fake writer whose pending write the test
  resolves by hand). Assert one target is in flight, change the target
  several times, release the write, and assert exactly the latest target is
  sent.
- Python docs: list the five types with one-line descriptions, following
  the file's existing sections.

Native players (`moq play`, `moqsrc`, and the OBS source) holding a
viewer once they adapt renditions is [players](/quest/m2/probe/players.md)'
work, not this quest's.

Public API: none (tests and docs). Wire: none.

## Required

- [Viewer probe targets and grants](/quest/m2/probe/viewer-target.md) - its PR, [moq-dev/moq#7090](https://github.com/moq-dev/moq/pull/7090), adds the handles and PROBE wiring these test
