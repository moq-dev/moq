# [M] Bindings follow the connection's bandwidth share

## Goal

A non-Rust publisher follows the connection's send estimate the way Rust does.
The bindings mirror `bandwidth::Allocator` and `bandwidth::Reservation`: a
session mints the allocator, a publisher reserves a share for a track at its
configured ceiling, the built-in video and audio encoders in moq-ffi and libmoq
follow the grant when handed a reservation, and an application that owns its
encoder reads the reservation's current grant. A Python, Swift, Kotlin, Go or C
publisher stops holding its configured bitrate through congestion.

Boundaries: the allocation rules are Rust's (strict priority tiers, max-min fair
within a tier, ceilings never observed rates) and the bindings add no policy.
OBS adopting the surface is [OBS rate control](/quest/m2/obs-moq-video/rate-control.md).

## Plan

`MoqBroadcastProducer` holds only the broadcast, and one origin can serve
several sessions, so a "follow this connection" flag on the publish call has no
connection to follow; the handle comes from the session. On dev
`rs/moq-net/src/model/bandwidth.rs` provides `Allocator::new(estimate)`,
`reserve(&track::Demand, max) -> Reservation`, and
`Reservation::{peek, consumer, update}`, and `Session::send_bandwidth()` is the
estimate. Today `rs/moq-ffi/src/video.rs` and `audio.rs` never set
`Options::bandwidth`, and the estimate reaches a binding only as the
`send_rate_bps` snapshot on the connection stats.

- moq-ffi: `MoqSession::bandwidth() -> MoqBandwidth` returns a handle to the
  one allocator the session owns, so every handle shares one reservation
  registry. The allocator consumes the live `bandwidth::Consumer`, and because a
  moq-ffi session reconnects on its own, that consumer is the reconnecting
  one: it reports no estimate while disconnected and resumes on the next
  connection, and reservations survive the gap. `MoqBandwidth::reserve(track,
  max_bps) -> MoqReservation` is keyed on the track producer's demand.
  `MoqReservation::grant() -> Option<u64>` is the current share as a snapshot
  (an encoder that asks before each frame needs nothing more); `None` means the
  allocator has no estimate or the track is not demanded, hold the current
  rate, and `Some(0)` is a real zero grant, exactly the distinction
  `Reservation::peek` draws. `update(max_bps)` moves the ceiling and dropping
  the reservation releases the share. Video and audio publish options accept an
  optional reservation; the built-in encoders feed its consumer to
  `Options::bandwidth` unchanged, so the same `None` versus zero semantics
  reach `rate::Control`. `set_bitrate` stays as the manual ceiling.
- libmoq: `moq_session_bandwidth`, `moq_bandwidth_reserve`,
  `moq_reservation_grant`, `moq_reservation_update`, `moq_reservation_close`,
  and a reservation parameter on the raw video and audio publish calls.
  Regenerate `moq.h`.
- Wrappers `py/moq-rs`, `swift`, `kt`, `go/wrapper/moq` and
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
- [#2848](/quest/m1/2848-follow-the-bandwidth-grant-in-moq-audio-instead-of.md) - audio following its grant in Rust
- [Ladder](/quest/m1/ladder/README.md) - the transcode consumer of the same estimate
