# [M] A spinning loop fails a sim test

## Goal

A moq-net sim test fails, naming the loop, when one task poll makes more than a
fixed number of kio `Ready` polls, and every loop it flags yields through a
`kio::coop::Budget`. Today a loop with no budget that keeps finding work ready
holds its thread with nothing to flag it, the way `RequestServe::poll_serve`
starved a go publisher's runtime (#5088).

## Plan

Decided 2026-10-09 while reworking #5088:

- #5088 gives only the publish serve loops a `Budget`: lite `RequestServe` and
  `GroupServe`, and ietf `TrackServe`, `GroupServe`, `read_fetch`, and
  `write_fetch_group`. A search found about 31 more loops that can keep
  finding work in one poll (receive, accept, announce, and request loops in
  `lite/subscriber.rs`, `ietf/subscriber.rs`, `ietf/session.rs`, and
  `model/origin.rs`). This guard decides which of them actually spin.
- The count is test-only, so kio's hot path stays untouched: a feature or
  `cfg` that only the sim runtime enables, reset on each task poll.
- A loop takes a `Budget` only where its `Pending` goes straight up to the
  task. Three known sites need care: lite `FetchServeRun` reads a
  `FrameIngest` yield as "still short" and may `abort_unused`;
  `Announced::poll_serve` must re-queue the route through `route_waiter` and
  yield in the outer loop too; and ietf `poll_datagrams` still spins on a
  datagram backlog after `TrackServe` yields, and its drain arm would skip
  datagrams after a yield unless it returns `Poll`.

## Related

- [Session burst hang](/quest/m1/session-burst-hang.md) - the same bench shapes that hang
- [FFI runtime](/quest/m1/ffi-runtime.md) - FFI apps run on more than one thread
