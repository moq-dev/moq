# [M] Track tail hardening

## Goal

moq-net and `@moq/net` wait out a subscription's tail by the same rules, and
the known holes in those rules are closed. A finished IETF subscription never
hangs its request task or under-reports its stream count, a group whose
header arrived is never silently dropped from a clean end, the grace never
cuts off a stream still being read, and tail bookkeeping stays bounded on a
lossy track.

## Plan

Track tail landed in https://github.com/moq-dev/moq/pull/4086 (JS) and
https://github.com/moq-dev/moq/pull/4116 (Rust), and Codex's findings on both
were left for this pass. Fix each in the language named and check whether
the other has the same hole.

Bugs:

- **Rust IETF publisher, END_OF_TRACK not cancellable.** After the group
  streams drain, `write_end_of_track` in `rs/moq-net/src/ietf/publisher.rs`
  runs outside the race against `stream.reader.poll_closed` and the session,
  so with uni-stream credit exhausted an unsubscribe leaves the request task
  parked forever, never sending PUBLISH_DONE.
- **Rust IETF publisher, stream count.** END_OF_TRACK is counted only if the
  whole write succeeds, while every other stream counts once opened. A reset
  END_OF_TRACK whose header still arrives can then land after the subscriber
  met the count and retired the alias.
- **Rust IETF subscriber, truncated first object.** `open_group` peeks the
  first object with `?` before creating the group, so a stream reset or
  truncated after its header drops that group, and since the stream still
  counts, the track can end clean without it. Keep the END_OF_TRACK peek, but
  create and abort the named group on any other peek failure.
- **Rust grace cuts off an arrived stream.** `Settle::poll` in
  `rs/moq-net/src/tail.rs` returns when the grace fires even if an
  END_OF_TRACK stream is mid-read, so the track finishes at the live edge and
  the later marker is ignored. JS's `Tail.settle` already waits for active
  streams; do the same.
- **JS lite floor.** `Math.max(entry.start, bounds.start)` in
  `js/net/src/lite/subscriber.ts` ignores a SUBSCRIBE_UPDATE that lowered the
  floor after SUBSCRIBE_START, so a newly requested lower group reordered
  behind the FIN is dropped. `bounds.start` alone is wrong too: a subscriber
  that asked below the announced start would wait for groups never promised.
  The owed start has to remember the floor each request was answered at.
  Check whether Rust's `SubStream::owed` has the same shape.

Open questions. The maintainer chose to plan these without settling them;
decide in the PR, applying the choice to both languages:

- **Are lost datagrams owed before a track ends?** Both `Tail`s account a
  datagram only when it arrives, so a lost one leaves a hole in the owed span
  and the end waits out the whole grace (JS was reported to wait and Rust
  not, but Rust's `covers(owed)` reads the same way despite the comment in
  `route_datagram`; confirm with a test first). Recommendation: no, datagrams
  are best effort. [SUBSCRIBE_DROP](/quest/m1/subscribe-drop.md) decided
  the same: datagram groups are not owed, so an uncovered hole waits out the
  grace on every version.
- **Does a group at or past the declared end abort the track, or only that
  group?** Rust aborts the track with `ProtocolViolation`; JS aborts the group
  and ends clean. Recommendation: abort the track in both. The peer
  contradicted its own end, and the repo fails loud on malformed input.
- **How is tail memory bounded?** Rust `Tail.accounted` gains a range per
  permanent gap for the life of the subscription, and so does JS's. A gap
  older than the grace can no longer be waited for, so folding it in as
  accounted loses nothing. Recommendation: that, which bounds the ranges by
  the gaps inside the grace window. Once publishers send SUBSCRIBE_DROP for
  every gap, only datagram holes remain.

Regression tests go in `rs/moq-net/tests/track_tail.rs` (its `hold_unis`
mock makes the reorder deterministic) and the JS tail tests. Wire output only
changes if the stream-count fix does, and that is a correction to what the
drafts already require.

## Related

- [Track tail interop](/quest/m1/track-tail-interop.md) - the Rust-JS check that both sides now agree
- [SUBSCRIBE_DROP](/quest/m1/subscribe-drop.md) - every group is a stream or a drop, replacing lite-07's stream count
- [Session death](/quest/m1/session-death.md) - how a tail ends when the session dies under it
