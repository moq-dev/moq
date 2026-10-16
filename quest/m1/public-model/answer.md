# [L] Public routes and answering

## Goal

A custom relay or session does what the built-in ones do with an origin,
through public moq-net and @moq/net calls only: ingest a peer's routes under
the peer's identity, answer broadcast and track requests on demand and close
them once unused without racing a returning reader, and advertise routes to
a peer without echoing the peer's own routes back.

## Plan

Rules, carve-outs, and the inventory method are in the
[epic](/quest/m1/public-model/README.md). Rerun the inventory first; the
recommendations below are the planning quest's, and the ones marked
**decision** were settled by the maintainer on 2026-10-16 (#5717).

Rust items (`rs/moq-net/src/model/`), as they stood at b4eab0db1:

- `origin::Hop::from_wire` (IETF cluster): public as the decode-side
  constructor that accepts 0 (`Hop::UNKNOWN`), beside `Hop::new`.
- `origin::Cost::charged` (both subscribers, IETF cluster): public.
  `Cost::UNKNOWN` goes; it equals `Cost::default()`.
- **Announcing identity** (`origin::Route::via`, `Route::source`,
  `Route::with_via`; both subscribers, server). **Decision**:
  - ✅ Replace. The identity moves onto the origin handle a session
    announces through, as `origin::Producer::via(hop)` beside the public
    `Producer::peer` (the name `via` stays), so every route it ingests
    carries it and a `Route` can never forward it. `Route.via` and
    `Route.source` stay private fields the handle sets.
  - A public `Route::with_via`, which a caller could forward by mistake.
- `origin::Consumer::excluding` (both publishers, client): public;
  split-horizon toward a peer, the counterpart of the identity above.
- `origin::Producer::create_source` (both subscribers): public under a role
  name (for example `answer_source`). It mints a broadcast with the origin's
  cache policy and stats that answers a `Dynamic` request without entering
  the route table, which `broadcast::Producer::new` cannot do.
- `broadcast::SourceGuard` (both subscribers): not model. Move it to the
  sessions; a third party drops or closes its own source.
- **Request validity** (`broadcast::RequestValidity`, `origin::Selection`;
  both subscribers). **Decision**:
  - ✅ Remove the need. The source minted for a `Dynamic` request ends
    itself with `Unroutable` once its route is replaced or its selection
    period retires, so no session can serve a stale instance by forgetting
    a check. The answering session sees it through its writes failing.
  - A public validity handle every answerer must poll.
- **Unused races** (`broadcast::Producer::poll_unheld`, `close_unheld`,
  `group::Producer::poll_unused`, `abort_unused`,
  `track::Request::reject_unused`; both subscribers). **Decision**:
  - ✅ Public, as one family matching the public
    `track::Producer::abort_unused(self, err) -> Result<(), Self>`: each
    answerer has `demand()` for the wait and an atomic `*_unused` that
    refuses when a reader returned in the gap. Rename `close_unheld` and
    `poll_unheld` to the `unused` vocabulary where their semantics match
    `broadcast::Demand`, or document the difference.
  - Keep them private.
- `origin::Dynamic::withdrawn` (lite subscriber): public, as the explicit
  peer withdrawal that differs from a session simply dropping its handle.
- `origin::interest_prefixes` (both subscribers): remove the need through
  [Rust lazy discovery](/quest/m1/lazy-discovery/rust.md), which gives Rust
  the origin's watched interests the way JS has them; make that public in
  both languages there or here, whichever lands second.
- `origin::Consumer::discovery`, `includes_hidden` (IETF publisher): fold
  into the public `with_hidden` plus a getter, unless Lazy discovery deletes
  them first. `beyond` (topping up a feed): re-inventory after Lazy
  discovery; public only if it survives.
- `origin::Consumer::request` (lite publisher): remove the need; it is the
  body of the public `request_broadcast`.
- `origin::Producer::empty`, `origin::Consumer::empty` (sessions): public.
  A scope with no patterns is not an equivalent: `scope` refuses an empty
  intersection with `Unauthorized`, which stays, since failing loud on a
  scope that names nothing is the right default. The drivers fill an omitted
  session half with an empty producer, and answer an out-of-scope announce
  interest with an empty consumer, so a custom session needs the same
  handles. Restructuring the drivers to run without a half is the
  alternative, but it is a bigger change for no public gain.
- `origin::Producer::stats`, `origin::Consumer::stats` (sessions): remove
  the need; the caller keeps the `stats::Session` it passed to the public
  `with_stats`.
- `bandwidth::Producer::weak`, `bandwidth::Weak`, `Weak::consume` (session
  handle): public. A session hands out estimate consumers without the
  handle itself counting as demand, which
  [Rust PROBE on demand](/quest/m1/probe-on-demand-rs.md) extends to the
  receive side.
- Test-only: `origin::Producer::announce`, `AnnounceProducer`,
  `ProduceTest`, `origin::Consumer::get_broadcast`.

JS items (`js/net/src/`):

- `wire.ts` `OriginProducer.receive` (`connection/forward.ts`): mirror the
  identity decision. A received route is a public `Origin.Producer.dynamic`
  on a handle marked with the peer's identity, so `receive` goes and
  `Origin.Request.accept` stops special-casing session-delivered broadcasts
  (`Broadcast.fromSession`).
- `wire.ts` `OriginProducer.requests`, `changed`, `blind`, `answer`,
  `routes`, `origin.ts` `RequestSlot`, `internal.ts` `coveringPrefixes`
  (the blind-answer loop in `connection/forward.ts`). **Decision**:
  - ✅ Replace with the Rust shape. A session answers through the public
    `Origin.Dynamic` requests and mints a source per request, as Rust
    sessions do, so the slot loop and its view go. Caveat: JS "blind" mode
    (answering requests after a peer refuses or resets announce requests)
    has no counterpart in the Rust sessions. The public shape must answer
    blind requests too, or Lazy discovery must remove blind mode first;
    decide which before deleting the loop.
  - Make the `wire.ts` views public.
- `wire.ts` `OriginProducer.expect` (`connection/reload.ts`, keeps requests
  pending while a reconnect is in flight): public if Rust has the same need
  across a reconnect, else private to the connection.
- `wire.ts` `OriginProducer.interests`, `attach`, `accepts`: `interests`
  is public with Lazy discovery; `attach(discovery)` goes with the
  `discovery` option Lazy discovery removes; `accepts` is a check on the
  public `allowed()` patterns.
- `wire.ts` `OriginConsumer.advertised`, `local`, `demand`, with
  `Advertised`, `Advertisements`, `sameInstance`, and `internal.ts`
  `presented`, `hiddenBelow`, `scopeHead`, `scopeCaptures`, `scopeOverlaps`
  (both publishers and subscribers): replace with the Rust shape. A
  publisher advertises from the public `announced()` stream of an
  `excluding(peer)` consumer and resolves with the public request taking an
  epoch, as Rust does. The helpers stay private to `origin.ts`. Check that
  the public announce event carries what `sameInstance` and a scoped
  route's claim need; if it does not, that is track metadata and goes public.
- `wire.ts` `Broadcast.requested` (with its `served` flag) and
  `overrideBroadcastWire` (both subscribers). Part of the JS answering
  decision above: ✅ public, mirroring Rust's
  `broadcast::Producer::dynamic()`: a broadcast producer hands out track
  requests on demand, and a track's info and fetches are answered through
  its `Track.Request` and `Track.Dynamic` (ranges once Model ranges lands).
  A subclass of `Broadcast.Consumer` cannot do either today.
- `wire.ts` `OriginConsumer.broadcasts`: registered and never read; delete.
- `hop.ts` `HopSchema`, `randomHop`, `UNKNOWN_HOP`, `MAX_HOPS`,
  `Cost.zero`, `Route.default`, `Route.normalize`, `routesEqual`: public
  values under `Origin`, mirroring Rust's `Hop::new`, `Hop::UNKNOWN`,
  `Route::default` and `PartialEq`. `Hop` is branded, so a third party
  cannot build `Route.hops` without a cast today.
- `announced.ts` `Announce.Producer`: remove the need; a session's route
  ingestion goes through the origin.
- `wireOf`, `registerWire`, `trackOf`, `registerTrackConsumer`: stay
  package-private plumbing, shrinking as the views above empty out. Delete
  `wire.ts` if nothing is left.

Tests: each public call gets a mock-time test in its language, and the
built-in sessions move onto the public calls, so their existing suites
cover the behavior.

## Required

- [Rust lazy discovery](/quest/m1/lazy-discovery/rust.md) - settles the interest and discovery items before they become public

## Related

- [Rust PROBE on demand](/quest/m1/probe-on-demand-rs.md) - the receive side of the bandwidth handle
- [Model ranges](/quest/m1/subscribe-ranges/model.md) - the track info and fetch answers
