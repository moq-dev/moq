# [L] Public answering

## Goal

A custom relay or session does what the built-in ones do with an origin,
through public moq-net and @moq/net calls only: answer broadcast and track
requests on demand and close them once unused without racing a returning
reader.

## Plan

Rules, carve-outs, and the inventory method are in the
[epic](/quest/m1/public-model/README.md). Rerun the inventory first; the
recommendations below are the planning quest's, and the ones marked
**decision** were settled by the maintainer on 2026-10-16 (#5717).

Rust items (`rs/moq-net/src/model/`), as they stood at b4eab0db1:

- `origin::Producer::create_source` (both subscribers): public under a role
  name (for example `answer_source`). It mints a broadcast with the origin's
  cache policy and stats that answers a `Dynamic` request without entering
  the route table, which `broadcast::Producer::new` cannot do.
- `broadcast::SourceGuard` (both subscribers): not model. Move it to the
  sessions; a third party drops or closes its own source.
- **Request validity** (`broadcast::RequestValidity`, `origin::Selection`;
  both subscribers). **Decision**:
  - ✅ Remove the need. The source minted for a `Dynamic` request enforces
    validity itself, so no session can serve a stale instance by forgetting
    a check. Revised in review (#5717): once its route is replaced or its
    selection period retires, the source refuses new track requests with
    `Unroutable`, but tracks it already accepted keep running and finish.
    `origin::Dynamic::update` and the subscriber drain paths rely on that,
    so producer writes must not start failing. The answering session learns
    it separately: the source's request stream (`poll_requested_track`)
    ends, so it stops answering and drops the source once its accepted
    tracks are done.
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
  Rust lazy discovery ([moq-dev/moq#6165](https://github.com/moq-dev/moq/pull/6165)), which gives Rust
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
- Test-only: `origin::Producer::announce`, `AnnounceProducer`,
  `ProduceTest`, `origin::Consumer::get_broadcast`.

JS items (`js/net/src/`):

- `wire.ts` `OriginProducer.requests`, `changed`, `blind`, `answer`,
  `routes`, `origin.ts` `RequestSlot`, `internal.ts` `coveringPrefixes`
  (the blind-answer loop in `connection/forward.ts`). **Decision**:
  - ✅ Replace with the Rust shape. A session answers through the public
    `Origin.Dynamic` requests and mints a source per request, as Rust
    sessions do, so the slot loop and its view go.
  - Make the `wire.ts` views public.
- **Blind answers** (the same loop, raised in review of #5717). Every JS
  session answers each request no route covers by subscribing on its peer:
  always, and as the only path after a peer refuses or resets discovery.
  Lazy discovery keeps a plain JS
  `request(path)` blind, so blind answering stays in JS and needs a public
  shape before the loop goes. Rust has no blind answers (decided 2026-10-20,
  maintainer), so no Rust session needs this shape.
  **Decision** (settled 2026-10-16, narrowed to JS 2026-10-20):
  - ✅ A fallback answerer, `Origin.Producer.fallback()` in JS. The name is still open: propose
    alternatives in this quest's PR before shipping it. It returns the same
    `Dynamic` request stream, scoped to the handle's allowed paths. It
    receives only requests that no announced or served route covers. Once
    the table routes a path, the new route takes only unresolved and new
    requests for it. A request the fallback already resolved ends with an
    error, since a blind answer has no epoch to prove the route serves the
    same instance (never stitch). It is never
    advertised, since it claims nothing. The first fallback to answer a
    request wins. Dropping the handle ends the requests it resolved with an
    error and never moves them to another fallback, which may be another
    publisher instance (never stitch). A standby answers only requests
    still unresolved, and new ones. This is what `serveRequests` in
    `connection/forward.ts` does today.
  - A `dynamic(prefix, route)` flagged blind and ranked below every
    announced route. This reuses one call but puts an unadvertised,
    claim-free entry in the route table, where selection and split-horizon
    must special-case it.
  Delete the JS loop only once the fallback answers the same requests,
  including after a downgrade.
- `wire.ts` `OriginProducer.expect` (`connection/reload.ts`, keeps requests
  pending while a reconnect is in flight): public if Rust has the same need
  across a reconnect, else private to the connection.
- `wire.ts` `OriginProducer.interests`, `attach`, `accepts`: `interests`
  is public with Lazy discovery; `attach(discovery)` goes with the
  `discovery` option Lazy discovery removes; `accepts` is a check on the
  public `allowed()` patterns.
- `wire.ts` `Broadcast.requested` (with its `served` flag) and
  `overrideBroadcastWire` (both subscribers). Part of the JS answering
  decision above: ✅ public, mirroring Rust's
  `broadcast::Producer::dynamic()`: a broadcast producer hands out track
  requests on demand, and a track's info and fetches are answered through
  its `Track.Request` and `Track.Dynamic`. A subclass of
  `Broadcast.Consumer` cannot do either today. The track-request handout
  lands here. Fetch and info answering through ranges (`overrideBroadcastWire`)
  moved to [the deferred quest](/quest/m1/public-model/deferred.md), which
  waits on Model ranges.
- `wire.ts` `OriginConsumer.broadcasts`: registered and never read; delete.
- `announced.ts` `Announce.Producer`: remove the need; a session's route
  ingestion goes through the origin.
- `wireOf`, `registerWire`, `trackOf`, `registerTrackConsumer`: stay
  package-private plumbing, shrinking as the views above empty out. Delete
  `wire.ts` if nothing is left.

Tests: each public call gets a mock-time test in its language, and the
built-in sessions move onto the public calls, so their existing suites
cover the behavior.

## Related

- [Public routes](/quest/m1/public-model/routes.md) - route ingest and advertising, split out of this quest to follow cluster routing
- [Public model after ranges, handover, and the JS tail](/quest/m1/public-model/deferred.md) - the track info and fetch answers, split out of this quest
