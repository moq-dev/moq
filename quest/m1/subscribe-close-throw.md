# [S] A local close never throws for a missing SUBSCRIBE_END

## Goal

On `main`, the `@moq/net` lite subscriber no longer raises
`ProtocolViolation: subscribe stream ended without SUBSCRIBE_END` for an end
of stream it caused itself by closing the subscription. A publisher that FINs
without SUBSCRIBE_END on a track the subscriber still holds open still fails
with it. The lite draft states that a subscriber's cancel owes no
SUBSCRIBE_END.

Non-goals: the release line, which
[the backport](/quest/m1/subscribe-close-backport.md) carries, and skipping
the SUBSCRIBE for a producer closed during setup.

## Plan

Found on `@moq/net` 0.4.3 against moq-relay `main`: a peer that closes a
subscription while it still waits for TRACK_INFO gets that ProtocolViolation
as an unhandled rejection (a console error in browsers, a failed test in
Bun's runner, an exit in Node). Three client defects combine:

1. On `release`, `#openSubscribe` sends SUBSCRIBE for a track producer that
   `request.close()` already closed. `main` retires that setup instead.
2. `race` stopped attaching handlers once an entry settled, so the
   serve loop's `subscriptionUpdates.then(() => closed)` rejected unobserved.
   Fixed on `main` with a test in ca8995280 (#5232); no `@moq/signals` release
   carries it yet.
3. `#runResponses` throws when the subscribe stream ends without
   SUBSCRIBE_END, including on every ordinary unsubscribe. The serve loop runs
   `producer.close(); stream.close()`, and `Stream.close()` cancels the reader,
   so the decode sees end of stream. With a FIN alone the relay answers with
   its own FIN and no SUBSCRIBE_END, which is right: the draft has a peer echo
   a FIN, and `rs/moq-net/src/lite/publisher.rs` treats the requester's FIN as
   the normal end. Today `closed.catch` swallows the throw, so it is harmless
   only while every caller keeps swallowing it. This quest fixes 3.

Decided 2026-10-14:

- Skip the throw when the track producer is already closed:
  `entry.end === undefined && entry.track.closed.peek() === undefined`.
  Every local close closes the producer before the stream (the serve loop,
  `Subscriber.close()`), and a locally closed track owes no end. Chosen over
  an explicit cancel flag on the entry, which every close path would have to
  remember to set.
- One sentence in `drafts/draft-lcurley-moq-lite.md`, next to the subscribe
  stream's SUBSCRIBE_END and FIN text: a subscriber cancels by closing its
  side (FIN), the publisher answers with its own FIN, and no SUBSCRIBE_END is
  owed. No wire change.

Tests in `js/net/src/lite/subscriber.test.ts`, with mock streams:

- An ordinary unsubscribe (subscribe, receive a group, close) whose peer
  echoes the FIN without SUBSCRIBE_END ends without a ProtocolViolation.
  `closed.catch` swallows the rejection today, so the test observes the
  responses promise itself through a narrow seam; a track-closure or log
  assertion would pass without the fix.
- A publisher FIN without SUBSCRIBE_END on a track still open closes the
  track with ProtocolViolation.
- Closing the broadcast request after the track subscribe is dequeued but
  before TRACK_INFO leaves no unhandled rejection, using the
  `unhandledRejection` pattern from `js/net/src/lite/publisher.test.ts`. The
  mock withholds TRACK_INFO until the close, so the ordering is explicit.

## Related

- [Backport to 0.4.x](/quest/m1/subscribe-close-backport.md) - carries this fix and the `race` fix onto `release`
