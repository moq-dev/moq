# [XL] A job's exact route continues its claim's readers

## Goal

A worker advertises a prefix route to accept lazy work. When a request
starts a job, the job's exact route carries the prefix route's identity, so
a reader first served through the claim continues on the exact route with
no cold-start reset, across relays, on lite-07. A different worker, or a
restarted job, never continues those readers: each is another identity.
Readers holding `follow` or `request` simply continue, and see a new
broadcast only when the identity changes. Rust and JS players keep playing
through a claim.

The ANNOUNCE_TAKEOVER work that landed in
[moq-dev/moq#6994](https://github.com/moq-dev/moq/pull/6994) is removed: the
lite-07 ANNOUNCE_TAKEOVER message (0x5), the origin, front and relay
handling of predecessors, the `Takeover { announce, predecessor }` event, the
predecessor fields in moq-ffi and the C ABI, and the takeover text in the
lite draft and the concept docs. No wire message or public event replaces it.

Non-goals: moq-transport, which keeps the cold-start reset
([moq-transport carries no epoch](/quest/m1/ietf-epoch-cut.md), and an
edge mints a new identity for each announcement,
[Identity and epoch](/quest/m1/api-audit/route/identity.md)).

## Plan

Re-planned 2026-10-30 (maintainer, the [route epic](/quest/m1/api-audit/route/README.md)):
continuity is the identity promise, not an `ANNOUNCE_TAKEOVER` predecessor
ID. An identity is one continuous sequence space at a path, so the exact
route and the claim's answer are the same consumer route,
origin → route(path, identity) → broadcast. The 2026-10-10 plan (TAKEOVER as
START plus a predecessor Announce ID, with per-stream ID translation in
relays) is dropped. Its safety requirements carry over, as follows.

Decided 2026-10-31 (maintainer, merging main into
[moq-dev/moq#7420](https://github.com/moq-dev/moq/pull/7420)): identity
wins over the predecessor design that landed in
[moq-dev/moq#6994](https://github.com/moq-dev/moq/pull/6994) meanwhile.
Keeping both would give two continuity rules, one by identity and one by
predecessor prefix, that every origin, relay and binding must agree on. The
identity rule already covers the claim case and needs no wire message, so
this quest removes the predecessor machinery and grows to [XL].

- **Removing ANNOUNCE_TAKEOVER.** Delete the 0x5 message and its codec in
  `rs/moq-net/src/lite/announce.rs`, `compress.rs`, `publisher.rs`,
  `subscriber.rs` and `replay.rs`; the predecessor tracking in
  `rs/moq-net/src/model/origin.rs` and `front.rs` (the `seen` stamps in
  `OriginConsumerState` and `PendingFront::via`); relay forwarding in
  `rs/moq-relay`; and `rs/moq-net/tests/integration/announce_takeover.rs`.
  Remove the `Takeover` event from Rust (unless
  [Listing and follow](/quest/m1/api-audit/route/events.md) has already
  removed the event kinds), `TAKEOVER` and the predecessor fields from
  `MoqAnnounceEvent` in `rs/moq-ffi/src/origin.rs` and from
  `moq_announce_event` in `rs/moq-c/src/api.rs`, and their wrapper and
  binding-doc mentions. In `js/net`, drop the ANNOUNCE_TAKEOVER decoding in
  `lite/announce.ts` and `lite/subscriber.ts`. Remove the takeover sections
  from `drafts/draft-lcurley-moq-lite.md`, `doc/concept/identity.md` and
  `doc/concept/moq-lite.md`.
- **Inheriting the identity.** `request.route(options)` from
  [One Route handle](/quest/m1/api-audit/route/handle.md) creates the
  path's exact route under the prefix route's identity by default, and the
  answer it gives the waiting request is that route's broadcast. A job
  that serves a held broadcast through [serve](/quest/m1/api-audit/route/serve.md)
  never announces an exact route, so it needs none of this.
- **One stream per path and identity.** A claim may answer a path under
  its identity only while that stream is live. Once the job's route ends,
  the next request for that path gets a fresh identity, because its
  sequences restart. A request pinned to the old identity is refused as
  ended, never answered with new bytes. This is the old rule "a worker
  restarting a job under an unchanged claim cannot relabel the previous
  job's cached content", now enforced in the origin. Prove it in the
  model, including retained generations and pending answers, before
  relying on it.
- **Relays.** An aggregated prefix can be served by several workers, but
  each worker has its own identity. A relay can therefore only continue a
  reader on routes carrying the identity it resolved, and readers of A
  never move to B. A relay that cannot keep the identity, such as an
  older-lite hop, mints a new one, so its readers see a new broadcast
  rather than a splice.
- **What a continued reader keeps.** Handles, group positions, metadata,
  object and negative caches, catalog pins, and outstanding
  TRACK/SUBSCRIBE/FETCH requests. Unrelated sibling paths are unchanged. A
  continued stream does not renew a composed catalog.
- **JS players.** `js/net`'s lite subscriber continues a consumer resolved
  through the claim on the exact route with the same identity instead of
  invalidating it, in `BroadcastCache` and `connection/forward.ts`, so a
  `@moq/watch` reader through a claim keeps playing.
- **Epoch-bound requests through a continued route.** A front or selection
  that continues on the exact route keeps the upstream SUBSCRIBE it already
  sent under the claim's route; a later request naming the exact route's
  epoch joins it without restamping. This is accepted while the answering
  worker is the same identity by construction. Decide whether that join
  needs the upstream's answer to confirm the epoch.
- **Docs.** Update the lazy-job lifecycle in `doc/concept/identity.md`:
  the cold-start step no longer resets on lite-07. Also update the lite
  draft's announce section where it mentions claims.

Tests with controlled time cover:
- the first reader before and after the exact route lands, with no reader
  at all, and with two workers at one prefix;
- a job restarting on one worker, and a request pinned to an ended stream;
- pending TRACK/SUBSCRIBE/FETCH at the front between its resolve and the
  exact route landing, and late groups across the transition;
- conflicting epochs within one identity;
- independently ending prefix and exact routes (a claim ending before or
  after its child), and late snapshots;
- multi-hop and diamond relays, and mixed versions with an older-lite hop;
- held catalog and media pins through a real player rather than route-table
  checks, and the composer.

Remove the cold-start warm-up reads (`claimed` in
`rs/moq-net-tests/tests/integration/restart.rs` and the
`rs/moq-net/tests/integration/drained_claim.rs` helper) for lite-07 and
assert that the first subscription succeeds. Keep them for moq-transport.
Run `just check`, `just test interop --all` and `just drafts check`.

Public API: breaking; removes `Takeover` and the predecessor fields in every
language. Wire: removes lite-07's ANNOUNCE_TAKEOVER (0x5), adding nothing
beyond [Identity and epoch](/quest/m1/api-audit/route/identity.md)'s fields.

## Required

- [Identity and epoch](/quest/m1/api-audit/route/identity.md) - the identity a job's exact route inherits
- [One Route handle](/quest/m1/api-audit/route/handle.md) - `request.route` creates the exact route under the claim's identity

## Related

- [Listing and follow](/quest/m1/api-audit/route/events.md) - continuity shows up inside `follow`, not as an event
- [Claim answer epochs](https://github.com/moq-dev/moq/pull/5678) - why a restarted job must never be stitched to its predecessor
- [Retired requests](https://github.com/moq-dev/moq/pull/5294) - replacements never substitute content into held handles
- [Routes and announces](/quest/m1/cluster-routing/routes.md) - the route layer whose ANNOUNCE_START layout carries the identity
