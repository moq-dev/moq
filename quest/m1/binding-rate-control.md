# [L] Bindings follow the connection's bandwidth share

## Goal

A non-Rust publisher follows the connection's send estimate the way Rust does.
The bindings mirror `bandwidth::Allocator` and `bandwidth::Reservation`: a
session mints the allocator, the built-in video encoder in moq-ffi and libmoq
reserves its configured bitrate against it and follows the grant, the built-in
audio encoder reserves its bitrate and follows once
[#2848](/quest/m2/2848-follow-the-bandwidth-grant-in-moq-audio-instead-of.md)
lands, and an application that owns its encoder reserves a share for its own
track and reads the current grant. A Python, Swift, Kotlin, Go or C video
publisher stops holding its configured bitrate through congestion.

Boundaries: `rs/moq-net/src/model/bandwidth.rs` is the spec, as it is for
[#2709](/quest/m1/2709-per-broadcast-bandwidth-estimates-and-reservation.md);
the bindings add no policy. OBS adopting the surface is
[OBS rate control](/quest/m2/obs-moq-video/rate-control.md).

## Plan

`MoqBroadcastProducer` holds only the broadcast, and one origin can serve
several sessions, so a "follow this connection" flag on the publish call has no
connection to follow; the handle comes from the session.

What the tree has today. `Allocator::new(estimate)` / `unlimited()`,
`reserve(&track::Demand, max) -> Reservation`, and
`Reservation::{peek, consumer, update}` (`bandwidth.rs:184-343`). moq-ffi's
`encode_video` (`rs/moq-ffi/src/video.rs:327-363`) builds a
`moq_video::encode::Config`, opens a `Sink`, and wraps it in
`Producer::with_track` / `Producer::new`; it never touches
`moq_video::encode::Options`, whose `bandwidth: Allocator` field
(`rs/moq-video/src/encode/producer.rs:246`) is capture-gated
(`rs/moq-video/src/encode/mod.rs:40-41`) and drives the capture loop only.
The audio side does build `moq_audio::encode::Producer` from an `Options`
carrying an allocator (`rs/moq-ffi/src/audio.rs:254`,
`rs/moq-audio/src/encode/producer.rs:62`) but leaves it unlimited. The
estimate reaches a binding only as the `send_rate_bps` snapshot on
`MoqConnectionStats` (`rs/moq-ffi/src/session.rs:539-543`).

- moq-ffi: `MoqSession::bandwidth() -> MoqBandwidth` returns a handle to the
  one allocator the session owns, so every handle shares one reservation
  registry. `MoqSession::Inner` is a client `moq_tokio::Connection` or an
  accepted `moq_net::Session` (`session.rs:605-609`), and their estimates
  differ in type: `Connection::send_bandwidth()` is an infallible,
  reconnect-surviving consumer (`rs/moq-tokio/src/connection.rs:998`), while
  `Session::send_bandwidth()` is an `Option`
  (`rs/moq-net/src/session.rs:99`), so the accepted side mints
  `.map(Allocator::new).unwrap_or_else(Allocator::unlimited)`. On the client
  side the consumer reports `None` while disconnected and resumes on the next
  connection, and reservations survive the gap.
- Two ways in, because a reservation needs the track's demand and the
  built-in encoders create their track inside the publish call: an app-owned
  encoder calls `MoqBandwidth::reserve(track, max_bps) -> MoqReservation` on a
  track producer it already holds, while the video and audio publish options
  take the `MoqBandwidth` handle and the publish call reserves at the
  configured bitrate itself, exposing the result as `producer.reservation()`.
  `MoqReservation::grant() -> Option<u64>` is `Reservation::peek`: `None`
  means no estimate or no demand, hold the current rate, and `Some(0)` is a
  real zero grant. `update(max_bps)` moves the ceiling and dropping the
  reservation releases the share.
- The built-in video encoder's follow loop runs in moq-ffi: a task reads the
  reservation's `consumer()`, feeds it through `moq_mux::rate::Control` (the
  policy moq-video uses, moved there by #2848), and applies each target with
  `MoqVideoProducer::set_bitrate` (`video.rs:298-307`), retiring on
  `BitrateUnsupported` exactly as moq-video does
  (`rs/moq-video/src/encode/producer.rs:383-386`). The audio publish call
  passes the handle's allocator into `Options::bandwidth`, so it reserves and
  follows whenever the Rust Producer does. `set_bitrate` stays as the manual
  ceiling.
- libmoq: `moq_session_bandwidth`, `moq_bandwidth_reserve`,
  `moq_reservation_grant`, `moq_reservation_update`, `moq_reservation_close`,
  a bandwidth-handle parameter on the raw video and audio publish calls, and a
  reservation accessor on their producers. Regenerate `moq.h`.
- Wrappers `py/moq-rs`, `swift`, `kt`, `go/wrapper/*.go` and
  `doc/lib/{py,swift,kt,go,c}` per the Cross-Package Sync table; dart after dev
  merges. Run `just test smoke-full`.
- Tests: two video producers on one session reserving 4 and 2 Mbps against a
  3 Mbps estimate get grants summing to at most 3 Mbps; two `bandwidth()`
  handles see each other's reservations; a dropped reservation frees its share;
  a reservation reports `None` across a reconnect and a grant again after it;
  the built-in encoder's applied bitrate follows a shrinking grant.

Branch from dev.

## Closes

- [#2857](https://github.com/moq-dev/moq/issues/2857) - close this issue when the quest finishes

## Related

- [#2709](/quest/m1/2709-per-broadcast-bandwidth-estimates-and-reservation.md) - the same allocator mirrored in js/net
- [#2848](/quest/m2/2848-follow-the-bandwidth-grant-in-moq-audio-instead-of.md) - audio following its grant in Rust, and the policy's move to moq-mux
- [Ladder](/quest/m2/ladder/README.md) - the transcode consumer of the same estimate
