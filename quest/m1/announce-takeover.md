# [L] A job's exact route continues its claim's readers

## Goal

A worker advertises a prefix route to accept lazy work. When a request
starts a job, the job's exact route carries the prefix route's identity, so
a reader first served through the claim continues on the exact route with
no cold-start reset, across relays, on lite-07. A different worker, or a
restarted job, never continues those readers: each is another identity. No
new wire message and no public event are added. Readers holding `follow` or
`request` simply continue, and see a new broadcast only when the identity
changes.

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
- **Docs.** Update the lazy-job lifecycle in `doc/concept/identity.md`:
  the cold-start step no longer resets on lite-07. Also update the lite
  draft's announce section where it mentions claims.

Tests with controlled time cover:
- the first reader before and after the exact route lands, with no reader
  at all, and with two workers at one prefix;
- a job restarting on one worker, and a request pinned to an ended stream;
- pending TRACK/SUBSCRIBE/FETCH and late groups across the transition;
- independently ending prefix and exact routes, and late snapshots;
- multi-hop and diamond relays, and mixed versions with an older-lite hop;
- held catalog and media pins, the actual players, and the composer.

Remove the cold-start warm-up reads (`claimed` in
`rs/moq-net-tests/tests/integration/restart.rs` and the
`rs/moq-net/tests/integration/drained_claim.rs` helper) for lite-07 and
assert that the first subscription succeeds. Keep them for moq-transport.
Run `just check`, `just test interop --all` and `just drafts check`.

Public API: none beyond the route handle. Wire: none beyond
[Identity and epoch](/quest/m1/api-audit/route/identity.md)'s fields.

## Required

- [Identity and epoch](/quest/m1/api-audit/route/identity.md) - the identity a job's exact route inherits
- [One Route handle](/quest/m1/api-audit/route/handle.md) - `request.route` creates the exact route under the claim's identity

## Related

- [Listing and follow](/quest/m1/api-audit/route/events.md) - continuity shows up inside `follow`, not as an event
- [Claim answer epochs](https://github.com/moq-dev/moq/pull/5678) - why a restarted job must never be stitched to its predecessor
- [Retired requests](https://github.com/moq-dev/moq/pull/5294) - replacements never substitute content into held handles
- [Routes and announces](/quest/m1/cluster-routing/routes.md) - the route layer whose ANNOUNCE_START layout carries the identity
