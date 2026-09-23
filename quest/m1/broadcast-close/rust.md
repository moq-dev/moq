# [M] Rust close

## Goal

`moq_net::broadcast::Producer::close()` is the one way to end a broadcast, and
no Rust or JS code in the repository calls a deprecated end API.

## Plan

In `rs/moq-net/src/model/broadcast.rs`:

- Add `close(&self)`: set `closing`, answer unserved names with `NotFound`,
  close the liveness token, and retire the announcer, as `finish` does today
  without setting `finished`. Borrow, like `finish`: any clone ends it.
- Mark `finish`, `abort`, and `Consumer::is_finished` `#[deprecated]` and
  `#[doc(hidden)]` per `rs/CLAUDE.md`. `finish` forwards to `close`. Leave
  `abort`'s behavior alone until the `dev` removal.
- Drop the "dropped without finish()" warning in `Drop for Alive`.
- `SourceGuard` (the lite and IETF subscribers' source handle) closes on both
  a graceful end and a drop, since the abort it records on drop reaches no one
  past this hop. Check the IETF `Detach::Abrupt` path still ends the source.

Callers to move (production): `model/origin.rs` (front end),
`lite/subscriber.rs` (`Route::finish`), `ietf/subscriber.rs`, moq-srt
`ts.rs`, moq-rtmp `server.rs` and `dial.rs`, moq-stats `produce.rs`, moq-gst
`sink/imp.rs`, moq-rtc `server/mod.rs`, moq-transcode `lib.rs`, moq-relay
`cluster.rs` (gossip registration), moq-boy `main.rs`, and moq-tokio's
`clock` and `chat` examples. Tests and benches across moq-net, moq-tokio,
moq-hls, moq-bench, and moq-gst move too.

moq-srt and moq-rtmp `Publisher::abort` drop the broadcast instead of ending
it; closing it explicitly makes that path deterministic.

In `js/net/src/broadcast.ts`, `Producer.close()` and `Consumer.close()` stop
taking `abort`: deprecate the parameter in the JSDoc and move the two origin
teardown callers in `origin.ts`. JS `close()` already blocks re-announcing.

Update `doc/lib/rs/moq-net.md`, which says the route retracts on `finish()`.
