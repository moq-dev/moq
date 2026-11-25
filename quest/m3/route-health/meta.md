# [M] Metadata-only subscriptions

## Goal

Rust and JS subscribe to a track's group and frame progress without its
payload, as a subscription of its own beside payload subscriptions, on the
lite-08 wire [wire.md](/quest/m3/route-health/wire.md) defines and every
lite-08 publisher and relay of ours already serves. A reader
that wants the payload opens a payload subscription from its first missing
frame, and nothing changes mode in place.

## Plan

Decided 2026-11-16 (maintainer, on
[moq-dev/moq#8882](https://github.com/moq-dev/moq/pull/8882), which built
the shape below it and is closed with its branch kept):

- ✅ No promotion in place: "rethinking drop in promotion makes sense. Keep
  SUBSCRIBE and SUBSCRIBE_METADATA logically separate." A metadata
  subscription and a payload subscription are two subscriptions, each with
  a mode fixed for its life. A reader switches by opening a payload
  subscription from its first missing frame, as a move between routes
  already does (`rs/moq-net/src/model/resume.rs`), and then closing the
  metadata one. The overlap carries only headers (rejected: promotion by a
  SUBSCRIBE_UPDATE on the same subscription, decided 2026-10-13 and built in
  that PR, where most of 19 review findings came from it: the reader's walk,
  the cache, and the publisher's accounting each needed rules for a group
  held in both modes).
- ✅ Deferred: "punt the new API/message to m3 for now; the goal was just to
  make sure the routing layer supports it; we don't have an immediate
  user." It waits on the route layer it rides on.
- With a fixed mode every group stream of a subscription has that mode, so
  the mode marker in each group stream header goes, and SUBSCRIBE_UPDATE
  gains no field. This reverses 2026-10-13's marker, which existed for
  streams opened before a mode update.

Decided 2026-11-25 (maintainer, /quest-run): ✅ "actually move
SUBSCRIBE_META entirely to m3 and out of lite-07". The wire and the serving
side are [wire.md](/quest/m3/route-health/wire.md), in m3 on lite-08-wip: a
publisher serving the mode, a relay forwarding metadata demand upstream, both
codecs, and the TLA+ check of two modes on one track. This quest keeps the
subscriber side: the API that asks for metadata, the bindings, and what
[failover](/quest/m3/route-health/failover.md) reads, so it stays M.
Rejected, both reversed by it: the wire in lite-07 before the cut with this
quest the implementation riding on it (decided 2026-11-23), and the serving
side in m2 on lite-07 (decided earlier on 2026-11-25). wire.md's Plan
records what this quest's code must keep conforming to.

- Recommended, to confirm when this starts: in the API, an entry and a
  consumer type of their own, so a payload read of a header-only frame cannot
  be written: the C binding in that PR needed an `Unsupported` error for it.

Standing from 2026-10-13 and 2026-11-02 (maintainer), carried from the quest
deleted in [moq-dev/moq#8423](https://github.com/moq-dev/moq/pull/8423)
(`git show cd560bde5^:quest/m5/route-health/meta.md`):

- ✅ It is a live stream: `largest()` on a payload-less standby is only the
  SUBSCRIBE_OK snapshot, and a one-shot status races the active route.
- ✅ A metadata FRAME keeps its timestamp and declared size and carries no
  bytes, so a real empty frame stays distinguishable. Metadata never
  satisfies a media reader, fills a cache as a complete frame, or advances a
  resume cursor.
- ✅ A relay pulls payload upstream only for payload demand (except over an
  older version, below), and holds at most one upstream subscription per
  mode for a track. Metadata demand is
  served from a payload copy while the relay holds one, and otherwise
  travels upstream as a metadata subscription. When the last payload reader
  leaves and metadata demand remains, the relay opens the metadata
  subscription and closes the payload one.
- Superseded 2026-11-25 (maintainer, /quest-run, on
  [wire.md](/quest/m3/route-health/wire.md)): ✅ nothing refuses the
  mode. A relay whose upstream is lite-07 or earlier subscribes for payload
  and drops it for its metadata readers, and moq-transport gets an
  [extension](/quest/m3/route-health/ietf.md) for the mode (rejected: this
  rule's own "an older version never emulates it by fetching and discarding
  media: it reports the mode unsupported", which the newer decision
  replaces, with its NOT_SUPPORTED refusal and "no moq-transport counterpart
  is needed").

What the parked PR learned, on branch `quest/m3/route-health/meta` at
`c00addff48`:

- Carries over here: a header read that returns the declared size without
  waiting for or inflating a compressed frame (`read_info`, `readInfo`); the
  bindings' header read; and the interop lane's subscriber half. The codecs,
  the relay's per-mode upstream, and the TLA+ check go to
  [wire.md](/quest/m3/route-health/wire.md), and its refusal on older
  versions and moq-transport is dropped.
- Recommended, to confirm when this starts: an endpoint whose own session is
  an older version asks for metadata the way a relay does over one, with a
  payload subscription whose payload it drops, so the API never refuses.
- A history fill is payload demand of its own. There it switched the relay's
  one shared upstream subscription to payload, so live groups arrived with
  payload until the fill was answered. A subscription per mode removes the
  case; pin it from the public API.
- Adding a field to the C `moq_subscription` struct changes its ABI.

Mirror the API across Rust, JS, and every binding that exposes
`Subscription`.

Tests, from the public API: metadata-only traffic across several hops
carries no payload; mixed demand; a payload subscription opened mid-group
from a position seen as metadata; a fill while only metadata demand stands;
empty frames; cancellation; an older-version session or hop, which carries
payload only where it must. Sweep tracks, readers, and relay depth
for bytes, wakes, and memory. Add the subscriber to
`just test interop --all`.

Public API: a metadata subscription in Rust, JS, and the bindings. Wire: none
beyond [wire.md](/quest/m3/route-health/wire.md).

## Required

- [Metadata mode on the lite-08 wire, served](/quest/m3/route-health/wire.md) - the SUBSCRIBE mode, payload-less FRAME, and the publishers and relays that serve it
