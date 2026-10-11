# Broadcast epochs

## Goal

A path and the epoch on its route are the only content identity, and no
first-party publisher reuses a pair for different content. Only routes with
the same epoch resume a subscription from the first frame it lacks; a route
without one keeps its subscriptions until it goes. So epochs are what make
failover seamless. Routes without an epoch never splice (maintainer,
2026-10-08): a `Restart` (or an end and start on older versions) tells every
downstream subscriber to stop using its copy of the old source and
resubscribe fresh; a downstream relay retires its copy for new requests and
forwards the `Restart`. A restart is a new epoch at the same
path: the newest epoch wins new requests and announce consumers see that
`Restart`, so viewers re-request rather than stall on a replaced
broadcast. Subscriptions already on the old one stay until the
application drops them or its route goes. Without an epoch, a restarted
publisher on the same hop chain as its lingering old session wins at once
(the newest announcement breaks the tie), but one on a different chain of
the same length and cost that loses the routing hash is reached only once
the old session closes and its route is withdrawn.

The epoch rides moq-lite 07 announcements and requests as metadata, so the
path never changes and every older version and moq-transport keeps working:
their routes carry no epoch, and see a restart as an end and start at the
same path.

Decided 2026-10-10: keep epochless clients supported on updated relays, with
unchanged paths and End+Start on replacement. The relay retains the explicit
identity internally; fresh requests cannot join its old cached instance.
This does not promise cache invalidation in arbitrary older IETF relays or
clients that retain objects across broadcasts. The negotiated
[IETF extension](/quest/m1/ietf-epochs.md) follows in m1, independently of
this release gate; seamless JS handover stays in m1 too.

Non-goals: pooling, which needs nothing here; a redundant pair shares an
explicit epoch through `moq --epoch`.
Also out of scope: trusting the publisher's clock (a far-future epoch wins
until its route goes away).

## Plan

Decided:

- This line gates the next release (decided 2026-10-03: without epochs every
  restarting first-party publisher stalls its viewers).
- The epoch is route metadata, not a path segment (decided 2026-10-06,
  replacing the `@<uuidv7>` segment: a path suffix changes the name old
  clients subscribe to). It is a lite-07 field on ANNOUNCE_START, TRACK,
  SUBSCRIBE, and FETCH, with no negotiation and nothing on published versions.
- The route's epoch is taken as given: nothing mints one by default (decided
  2026-10-06). Each first-party publisher mints one per run and announces it;
  a replica announces a shared one. A route without one, such as a
  transcoder's prefix claim, is never stitched to another worker's output.
  A better route without an epoch wins new requests and is announced as a
  `Restart`, replacing "stays on the worker that first served a
  subscription", which let dead routes linger (decided 2026-10-07).
- The newest epoch wins a prefix ahead of cost (decided 2026-10-06). The
  hard switch that ended subscriptions in flight with `Unroutable` (decided
  2026-10-06 for epochs, and 2026-10-07 in #5013 for routes without one) is
  reversed (2026-10-07): subscriptions stay sticky on their route and an
  explicit `Restart` announce event tells players to follow. When the newest goes and
  an older one is still live, the older one wins again as a new broadcast.
- [Announcement takeover](/quest/m1/announce-takeover.md) stays m1 and gates
  the lite-07 cut, not this release. It replaces the response-only claim-epoch
  proposal: an independent route explicitly continues the same served
  instance, rather than inferring continuity from an epochless claim.
- Decided in the 2026-10-10 resume audit: a catalog reference pins its first
  resolved sibling for the catalog instance's lifetime in players and
  exporters. Optional reference epochs constrain the first resolution on
  epoch-aware transports; on epochless transports they remain best effort.
  This supersedes following the newest sibling by name. The
  [catalog references quest](/quest/m0/broadcast-epoch/catalog-references.md)
  owns the schemas, readers, and compatibility documentation.
- Every first-party publisher that can restart mints its own: the apps,
  moq-boy, the ingest gateways, moqsink, and the bindings below. Players
  (`moq play`, `@moq/watch`, demo/web) follow the announce `Restart`. moq-stats mints one per
  [group announcement](/doc/concept/stats.md#broadcasts), which also gated
  the release (decided 2026-10-04).
- The m1 quests gating this line moved under it in the 2026-10-05 audit, and
  the OBS half moved to m1 as [OBS publishes under epochs](/quest/m1/obs-epoch.md).

- Until lite-07 is offered by default, default sessions (lite-06) carry no
  epoch, so a cluster GOAWAY redial or a standby takeover ends subscriptions
  instead of resuming them (accepted 2026-10-06 over negotiating the field on
  lite-06). A release that needs seamless failover promotes lite-07 first.

Decided 2026-10-06: older versions and moq-transport lose cross-route resume,
since their routes have no epoch, but their mid-group start handling stays.
The IETF joining FETCH is the normal live join for every IETF subscription, and
the IETF resume point and lite-05/06 `widen_frame_bounds` still serve any
mid-group start: a public `Subscription::with_start` or a downstream Frame
Start a relay forwards upstream.

This README owns an end-to-end relay test: republish a name under a new
epoch while the old publisher's session stays open. A lite-07 viewer and a lite-06 or IETF viewer
that follow the announce `Restart` (or END then START) both reach the new
epoch within one RTT-scale bound rather than the idle timeout, and killing the
newest epoch falls back to a still-live older one.
The same test drives the real players (decided 2026-10-09, from #5154):
`moq play` and a browser `@moq/watch`, the latter through the `just test
media` lane, both show the new run within that bound, without a manual
republish against a live relay.

When the release cut carries stats epochs, drop
[#4810](https://github.com/moq-dev/moq/pull/4810)'s wall-clock group seed and
its `doc/concept/stats.md` sentence from `release`; until then, the
maintainer's pre-release merge of `release` into `main` keeps `main`'s
`rs/moq-stats` and `doc/concept/stats.md`. MoQ Pro's VOD `storage.json` moves from the same seed to its own epoch.

The 2026-10-10 resume audit adds release gates for newly issued requests
through retired handles and for late duplicate delivery after same-epoch
handover. Keep existing reads sticky; stopping new unpinned wire work is a
separate boundary. The catalog-reference work applies the same pinning
policy to players and exporters. Wall-clock expiry, live DROP dispositions,
FETCH budgets, seamless JS handover, and the IETF extension remain m1.

Extend the line's relay/media tests to hold catalog and media from A while B
replaces it, exercise later metadata/subscription/FETCH operations, and prove
that following the replacement starts a fresh catalog with fresh pins. Check
exact and covering routes, matching/different/absent epochs, and legacy
End+Start. The child quests own their focused regressions.

## Required

- [Retired requests](/quest/m0/broadcast-epoch/retired-requests.md) - a held broadcast or track cannot open unpinned wire work against its replacement
- [Resume duplicates](/quest/m0/broadcast-epoch/resume-duplicates.md) - a delayed group is delivered once even beyond the old 1024-entry history
- [JS restart keeps the request](/quest/m0/broadcast-epoch/js-restart-keeps-request.md) - a resolved request remains on its old instance, matching Rust's sticky subscriptions
- [Source pin](/quest/m0/broadcast-epoch/source-pin.md) - `Source` is built from the resolved catalog broadcast and every later request stays on it, so an epochless replacement never splices into the old program
- [Catalog references](/quest/m0/broadcast-epoch/catalog-references.md) - players and exporters pin siblings consistently, with optional reference epochs and documented legacy best effort
- [Publish catalog restart](/quest/m0/broadcast-epoch/publish-catalog-restart.md) - `@moq/publish` never reuses catalog group numbers under one name and epoch after a re-announce
- [Bindings](/quest/m0/broadcast-epoch/bindings.md) - moq-ffi and every wrapper expose the epoch and let a publisher announce one
- [Stats totals and prefix tracks](/quest/m0/broadcast-epoch/stats-split.md) - the same release retires the per-path stats maps for totals and on-demand prefix tracks (decided 2026-10-05)

## Related

- [JS consume identity](https://github.com/moq-dev/moq/pull/5254) - requests carry the serving prefix's epoch and new winners cannot reuse an old cached broadcast
