# [L] Public answering

## Goal

A custom relay or session does what the built-in ones do with an origin,
through public moq-net and @moq/net calls only: answer broadcast and track
requests on demand and close them once unused without racing a returning
reader.

## Plan

Rules, carve-outs, and the inventory method are in the
[epic](/quest/m1/public-model/README.md). The draft implementation binds new
track requests to their answer source's selection period, exposes atomic
unused operations and empty origins, moves source ownership into sessions,
and adds JS broadcast answering and a fallback request stream. Finish the
public contract and inventory before this quest is complete.

- The fallback must yield a public forwarded-request role that can accept
  an answer. The current generic JS `Request.accept` is `@internal`, so it
  is erased from published declarations. Making it public would recreate
  the application-claim misuse that the required quest below removes. That
  quest owns the type split; do not duplicate it here or substitute runtime
  checks for the public role boundary.
- Settle the proposed names and ownership shape in the draft PR. The
  current Rust proposal is `Producer::answer_source(&Request)`; prefer a
  source factory tied to the eventual forwarded-request role if it removes
  mismatched origins and application claims by construction. JS proposes
  `Producer.fallback()`, with `unrouted()` as an alternative. The fallback
  returns only the request stream and close operation, since it owns no
  advertised route to update.
- Finish validity for already handed-out but unaccepted track requests.
  Source retirement currently stops new lookups and queue handouts, while
  ordinary broadcast close leaves a claimed request to its handler. A late
  answer must not serve a retired instance. Preserve writes on tracks
  accepted before retirement, and add coverage for the unfinished handout.
- Finish migrating test-only users of the JS request-slot wire view onto
  the public answerer, then remove that view and the exported `RequestSlot`.
  Delete the registered-but-unread `OriginConsumer.broadcasts` view.
- Remove the private Rust `Producer::stats` and `Consumer::stats` accessors
  by retaining the supplied session stats at their callers. Fold surviving
  hidden-discovery helpers into the public consumer shape after lazy
  discovery lands, and remove the private `Consumer::request` wrapper by
  using `request_broadcast` from the lite publisher.
- Rerun the inventory after the dependency lands. Keep route ingestion and
  advertising with [Public routes](/quest/m1/public-model/routes.md).
  Rust interests and snapshots currently have scaffolding, but production
  sessions still discover all allowed prefixes; expose only what the lazy
  discovery implementation needs, wherever that lands second.
- Keep per-operation validity for uncached FETCH and TRACK-info private
  under [the deferred quest](/quest/m1/public-model/deferred.md). Closing a
  source to new track requests cannot stop an already accepted epochless
  track from fetching a replacement publisher. Do not weaken those checks
  while moving the new-track boundary into the model.

The fallback still must serve only unrouted, authorized requests, never
advertise, accept the first answer, and error resolved requests when its
handle dies or an announced route replaces the blind answer. Unresolved and
new requests may use a standby; resolved ones never stitch across instances.
Each public call needs mock-time coverage, and the built-in sessions must
use the public answering calls. Public API and wire impact stay explicit in
the PR; these model changes do not change the wire format.

## Required

- [Claim request type](/quest/m1/public-model/claim-request-type.md) - split forwarded requests from application claims before publishing a fallback that can accept answers

## Related

- [Public routes](/quest/m1/public-model/routes.md) - route ingest and advertising, split out of this quest to follow cluster routing
- [Public model after ranges, handover, and the JS tail](/quest/m1/public-model/deferred.md) - the track info and fetch answers, split out of this quest
