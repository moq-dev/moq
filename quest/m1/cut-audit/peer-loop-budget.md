# [S] Peer-driven receive loops yield on a per-message budget

## Goal

A peer streaming small messages at line rate (SUBSCRIBE_UPDATEs, announces,
datagrams, or moq-transport PADDING) cannot keep a session's driver busy
until its source runs dry. Every peer-driven receive loop yields on a
per-message `kio::coop::Budget`, as lite's probe, padding, and TRACKS loops
already do, and a burst of updates that change nothing does no work.

## Plan

Found by the 2026-10-24 pre-cut audit at 151a529670. Source-inferred,
inherited.

The lite session is one `Driver::poll`, so a loop that never yields starves
the session's other arms (accept, PROBE, other prefixes) and its worker.
Loops without a per-message budget:

- `RequestServe::poll_requester` (`rs/moq-net/src/lite/publisher.rs:1770`):
  the budget at about line 1640 is charged once per outer loop. Each 9-byte
  SUBSCRIBE_UPDATE runs `TrackRun::update` (about line 3771), which wakes
  every in-flight group task, re-ranks, and on a relay republishes upstream,
  even when nothing changed.
- The announce prefix loop (`lite/subscriber.rs:1782`) and `DatagramRecv`
  (`lite/subscriber.rs:1403`).
- moq-transport's draft-18+ PADDING drain (`rs/moq-net/src/ietf/session.rs:1146`)
  awaits ready chunks in a loop; lite added a budget for the same reason in
  [moq-dev/moq#6233](https://github.com/moq-dev/moq/pull/6233). Share one
  padding drain between lite and IETF.

Charge the budget per message. For updates, each replaces the last: keep
only the newest (and the largest `wanted_group`) and skip the steer write
when nothing changed. A test floods each loop and asserts a sibling arm
still makes progress.

Public API: none. Wire: none.
