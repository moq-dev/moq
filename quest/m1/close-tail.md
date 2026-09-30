# [M] A graceful close waits for the subscriber to read the tail

## Goal

When `Session::close()` returns `Ok` on moq-lite-07, every subscriber has read
each finished track to its end: every frame of the final group, then a clean
end. Today close counts a group delivered once the peer's QUIC stack acks it,
then sends `CONNECTION_CLOSE`, and a real QUIC stack discards stream data the
subscriber's moq-net has not read yet. The subscriber loses the final group and
sees `Session(Cancel)`, while the publisher is told the close succeeded.

## Plan

Root cause: a lite serve decrements the publisher's `owed` count once its
group streams and Subscribe Stream FIN are acked (`poll_close` in
`rs/moq-net/src/lite/publisher.rs`), and `Publisher::drained()` reads only that
count. `poll_drain` in `rs/moq-net/src/session.rs` then closes the session with
`Cancel`. The lost final group is unread data the subscriber's QUIC stack
discards on `CONNECTION_CLOSE`; the subscriber's abort then ends the track with
that group still open. `tests/session_close.rs` passes only because the mock
transport keeps unread data after close.

Decided (maintainer, 2026-09-30):

- The fix is a FIN handshake, per subscription. On lite-07 the subscriber FINs
  its side of the Subscribe Stream once its tail accounting settles the track's
  end, never earlier: every group below the end has been read to its FIN,
  reset, or dropped. Today that accounting is SUBSCRIBE_END's `Stream Count`;
  once [SUBSCRIBE_DROP](/quest/m1/subscribe-drop.md) lands it is a received
  group or a SUBSCRIBE_DROP per sequence. The FIN rule rides on whichever is in
  place, so neither quest waits on the other. The publisher's
  drain waits until each served Subscribe Stream is closed both ways (the
  subscriber's FIN or a reset), under the same `CLOSE_TIMEOUT`, before
  `CONNECTION_CLOSE`. A session-level GOAWAY handshake was rejected: the peer
  would need the same per-subscription knowledge to know when to close.
- lite-07 only, since it is still wip and the draft can require the
  subscriber FIN there. On lite-05 and lite-06 an old subscriber never FINs,
  so close keeps today's ack-based drain rather than time out every close.
  #4508's repro is on lite-05, and that path stays as it is: this quest closes
  the issue by fixing the close on the version that can carry the FIN rule.
  IETF sessions stay with [IETF drain before close](/quest/m1/ietf-drain-before-close.md).
- Both subscribers change: moq-net and `@moq/net`.

Update `drafts/draft-lcurley-moq-lite.md`: the Subscribe section gains the
subscriber FIN rule, with a changelog entry. Check that a lite-07 publisher
already treats a subscriber FIN after SUBSCRIBE_END as the end of a finished
subscription, not a cancel of one still in flight
([Request stream cancel](/quest/m1/request-stream-serve.md)).

Tests: the reporter's `close_tail` case (one session, paused clock, a mock
switch that acks a FIN as soon as it is sent, as a real transport does) fails
today and passes with the fix; a subscriber that never FINs makes close return
`Error::Timeout`; a final range with a skipped and a reset group still settles
and FINs. [Track tail interop](/quest/m1/track-tail-interop.md) is the
cross-language proof over a real relay.

Public API: none. Wire: on lite-07 a subscriber FINs its Subscribe Stream
after reading the track's end, and a publisher closing gracefully waits for it.

## Closes

- [#4508](https://github.com/moq-dev/moq/issues/4508) - `close()` returns `Ok` while the subscriber gets a cut track

## Related

- [IETF drain before close](/quest/m1/ietf-drain-before-close.md) - the same drain for moq-transport sessions
- [SUBSCRIBE_DROP](/quest/m1/subscribe-drop.md) - replaces the lite-07 tail accounting the FIN rule waits on
