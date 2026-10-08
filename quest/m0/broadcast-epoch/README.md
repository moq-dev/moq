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
  explicit `Restart` announce event tells players to follow, through
  [Restart](/quest/m0/broadcast-epoch/restart.md). When the newest goes and
  an older one is still live, the older one wins again as a new broadcast.
- [Claim-served epochs](/quest/m1/claim-epochs.md), where a
  lite-07 claim's answer carries the served broadcast's own epoch, no longer
  gates this line (decided 2026-10-08): it is a lite-07 opt-in, so it moved
  to m1.
- A catalog `broadcast` reference by name follows the newest epoch, since a
  path cannot name one.
- Every first-party publisher that can restart mints its own: the apps and
  moq-boy through [Apps](/quest/m0/broadcast-epoch/apps.md), the
  ingest gateways, moqsink, and the bindings below. moq-stats mints one per
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

When the release cut carries stats epochs, drop
[#4810](https://github.com/moq-dev/moq/pull/4810)'s wall-clock group seed and
its `doc/concept/stats.md` sentence from `release`; release-to-main
back-merges keep `main`'s `rs/moq-stats` and `doc/concept/stats.md` until
then. MoQ Pro's VOD `storage.json` moves from the same seed to its own epoch.

## Required

- [Restart](/quest/m0/broadcast-epoch/restart.md) - a replaced broadcast reaches announce consumers as an explicit Restart, subscriptions stay sticky, and new requests never join a replaced route's front
- [Apps](/quest/m0/broadcast-epoch/apps.md) - moq-cli, the browser publish and watch components, and demo/web restart into a new epoch and reset on the switch
- [Publish catalog restart](/quest/m0/broadcast-epoch/publish-catalog-restart.md) - `@moq/publish` never reuses catalog group numbers under one name and epoch after a re-announce
- [TS restart](/quest/m0/broadcast-epoch/ts-restart.md) - a signalled backward TS discontinuity finishes the broadcast and continues the same input under a fresh epoch
- [Bindings](/quest/m0/broadcast-epoch/bindings.md) - moq-ffi and every wrapper expose the epoch and let a publisher announce one
- [moqsrc](/quest/m0/broadcast-epoch/moqsrc.md) - moqsrc switches to the new broadcast on a `Restart`, keeping its pads by rendition name
- [Stats totals and prefix tracks](/quest/m0/broadcast-epoch/stats-split.md) - the same release retires the per-path stats maps for totals and on-demand prefix tracks (decided 2026-10-05)
- [Dynamic epoch update](/quest/m0/broadcast-epoch/dynamic-epoch.md) - an epoch change through `Dynamic::update` announces a `Restart` downstream, so subscribers drop the old copy and resubscribe, while the origin keeps serving
