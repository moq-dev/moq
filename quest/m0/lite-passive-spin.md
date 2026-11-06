# [S] A lite subscriber parks once only passive readers remain

## Goal

A lite datagram subscription whose last demanding reader leaves while
passive readers remain parks until demand returns or the readers go, instead
of busy-looping. Today `DatagramState::Live` and `Shared` in
`rs/moq-net/src/lite/subscriber.rs` call
`producer.poll_priority(waiter, Some(*priority))`. Once only passive
consumers are left, the aggregate priority is `None`, so that call returns
`Ready(None)` on every pass. `close_unused()` returns false because the
passive slots remain, and the loop `continue`s, burning a core.

Non-goals: changing what a passive reader is, or the IETF subscriber, which
already tracks `Option<i8>` (`rs/moq-net/src/ietf/subscriber/datagrams.rs`)
and does not spin.

## Plan

Decided 2026-11-06 (maintainer, /quest-complete, a follow-up of
[moq-dev/moq#8000](https://github.com/moq-dev/moq/pull/8000), first seen in
[moq-dev/moq#7959](https://github.com/moq-dev/moq/pull/7959)): ✅ m0, since a
relay can burn a core in shipped code. Not chosen: m1; m3.

- Reproduce it first in a test that fails on the spin: a relayed lite-07
  datagram copy whose demanding reader leaves while a lite-05/06 stream
  subscriber's passive join, or a TRACK hold, remains. Assert progress by
  event or poll count on a paused clock, not by wall time.
- Track the last priority as `Option<i8>`, as the IETF subscriber does, so a
  `None` priority parks on the waiter instead of re-polling. The fix lands
  for both `Live` and `Shared`.

Public API: none. Wire: none.
