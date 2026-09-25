# [M] Session death error

## Goal

When a session dies, every track it was receiving ends with the session's own
error, in Rust and JS, over moq-lite and IETF. A reader never sees a clean end
for a stream that was cut off, and never a generic `Dropped` or `Cancel`
standing in for the real cause. A publisher's reset of a subscription reaches
the reader as that reset's error.

## Plan

Today the cause is lost in several places. Rust:

- The lite subscriber's serve loop gives back `Error::Dropped` when the
  session dies, and `SubscriptionCleanup` aborts the remaining tracks with
  `Error::Cancel` (`rs/moq-net/src/lite/subscriber.rs`). `Dropped` here is a
  bug.
- The IETF subscriber's `State::drop` aborts every subscription with
  `Error::Cancel` (`rs/moq-net/src/ietf/subscriber.rs`).
- Worse, the reader can see a clean end. Once SUBSCRIBE_END has declared
  the boundary, a session that dies with a group below it still in flight
  leaves `recv_group()` returning `Ok(None)`, skipping that group (#4061).
  The abort does reach the track, but the clean-end check wins whenever the
  reader polls after the close lands, so the outcome depends on who reads
  first. A declared end is only clean once every group below it is
  accounted for; an abort before then wins.

JS:

- The lite `Subscriber.close()` runs from `Connection.close()` even after a
  fatal error and closes every track cleanly (`js/net/src/lite/subscriber.ts`,
  `js/net/src/lite/connection.ts`).
- On lite-05 and later, `#drainResponses` swallows a reset of the subscribe
  stream and reports it as a clean end. On lite-01 to -04 the error does reach
  the track.
- On IETF drafts 14-16, where one control stream carries every request, the
  adapter closes every per-request stream cleanly when the session or control
  stream dies (`js/net/src/ietf/adapter.ts`), so the tracks end cleanly. Drafts
  17 and later reject with the transport error.

Thread the session's close error to wherever tracks are torn down, and abort
with it. A deliberate local close of the session is still a close, not an
error.

Test by killing a session mid-track in both languages and asserting the reader
sees the session's error, and by resetting a subscribe stream on lite-05 and
later. For #4061, pin the deterministic mock repro and the real-QUIC one from
`kidq330/bug/subscription_ends_clean_with_missing_group`
(`subscription_end_integrity` in `moq-net` and `moq-tokio`), with their
controls: a finished track still ends clean while its session lives.

## Closes

- [#4061](https://github.com/moq-dev/moq/issues/4061) - close this issue when the quest finishes
