# [L] moq-net names say one thing

## Goal

moq-net and `@moq/net` names don't collide with other items, don't describe
the wrong shape, and mirror each other. Each rename reaches the bindings and
docs that expose it.

## Plan

Decided 2026-10-23 (API audit follow-up, maintainer):

- **Demand says `wanted`.** `group::Demand` drops
  `is_used`/`used`/`unused`/`poll_used`/`poll_unused` for `track::Demand`'s
  `wanted()` snapshot plus `wanted_changed()` (and its poll).
  `broadcast::Demand` is already deleted.
- **Allocator.** `Session::bandwidth()` returns the send allocator beside
  `send_bandwidth()`/`recv_bandwidth()` estimates: rename it `allocator()`, and
  `Client::with_bandwidth` `with_allocator`.
- **Stats and transport collisions.** `stats::Control` (vs `track::Control`)
  becomes `stats::Message`; `stats::Bytes` (vs `bytes::Bytes`) becomes
  `stats::Totals`; the trait `transport::Stats` (vs `session::Stats` and the
  `stats` module) becomes `transport::Metrics`.
- **Wire owns bytes.** `transport::Metrics` drops `bytes_sent` and
  `bytes_received`: `transport::Wire` already counts them, and
  `session::Stats` reads them from it. `StatsUnavailable` goes; the trait
  method gets a default.
- **Credential.** `auth::Auth` (JS `Auth.Auth`) is one presented credential:
  `auth::Credential` in Rust, JS, and the FFI.
- **Info.** `track::Consumer::query() -> Pending<Querying>` becomes
  `info() -> Pending<Resolving>`, mirroring JS `Track.Consumer.info()`.
  `Pending` stays generic over the task so poll callers and `track()` keep
  working; awaiting it still yields `Result<Info>`.
- **Reservation.** `bandwidth::Reservation::consumer()` becomes `consume()`;
  JS `Reservation.grant` (vs `Auth.Grant`) becomes `rate`, as the bindings
  name it.
- **JS groups.** `Track.Producer.writeGroup` becomes `createGroup`, Rust's name.

Kept, so the next audit skips them: `largest()` stays a snapshot on
`track::Producer` and a stream on readers, and `subscription()` stays the
aggregate on producers and requests: the receiver type tells them apart.
`server::Activity` and `stats::RefusalKind` stay in moq-net.

Public API: moq-net, `@moq/net`, moq-tokio, moq-ffi and its wrappers renamed.
Wire: none.
