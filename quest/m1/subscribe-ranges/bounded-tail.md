# [M] A copy that stopped at a request's end bound accepts its tail

## Goal

A group received for a request whose end falls mid-group stays open to the
rest of that group. When later demand asks the same sequence past that end,
the tail is accepted instead of rejected as `Duplicate`, so a second
subscriber on the same relay reads the frames it asked for. This holds for
plain and compressed tracks on every version that carries frame bounds.

## Plan

Found on [moq-dev/moq#6242](https://github.com/moq-dev/moq/pull/6242) while
answering Codex's
[P1](https://github.com/moq-dev/moq/pull/6242#discussion_r4345457940): through
one relay, a subscriber reads group 5 frames 0..3 and a second then asks for
3..5. The tail never arrives, on a plain track and on a compressed one alike.

- `GroupRecv` (`rs/moq-net/src/lite/subscriber.rs`) calls `group.finish()`
  when a group stream ends cleanly, including one that ended only because the
  request's end bound was reached. The truncated copy then looks complete.
- `claim_sequence` and `covering_group` (`rs/moq-net/src/model/track.rs`)
  judge a copy only by its first frame, so every later stream for that
  sequence is `Duplicate`, whatever frame it was asked from.
- Fix it in the receive path: a stream that stopped at the request's end
  bound is not the group's end. Weigh leaving the copy unfinished so a tail
  appends from its end (one copy; a compressed group keeps inflating against
  its own prefix, which `widen_compressed` relies on when it keeps a request at
  N) against letting a bounded copy stop covering a request past its end so
  the new stream replaces it (compressed requests would then widen to frame 0
  again).
- A peer must not be able to pass off an early FIN as reaching the bound.

Test on mock time through a relay: a bounded first subscriber, then a second
asking past its end, on plain and compressed lite-07 tracks, and on
moq-transport draft-20+ if its Absolute end reaches the same path.

Public API: none expected. Wire: none.

## Related

- [Compressed tracks](/quest/m1/compress/README.md) - its Ranges rule keeps a warm request at N, which this makes reachable
