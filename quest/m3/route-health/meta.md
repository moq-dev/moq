# [L] Metadata-only subscriptions

## Goal

Rust and JS subscribe to a track's group and frame progress without its
payload, as a subscription of its own beside payload subscriptions, on the
lite-07 wire [lite07-metadata](/quest/m2/lite07-metadata.md) defines; a
lite-07 peer of ours stops refusing the mode. A chain of relays forwards
and aggregates that demand without pulling the media behind it. A reader
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

Decided 2026-11-23 (maintainer, /quest-run): ✅ the wire goes into lite-07
before the cut as [lite07-metadata](/quest/m2/lite07-metadata.md), and this
quest is the implementation that rides on it (rejected: the whole quest in
lite-08), since the [epic](/quest/m3/route-health/README.md) now has a
user. That quest settles the wire this
Plan recommended: a `Meta` byte on SUBSCRIBE, set when it opens, the
payload-less FRAME, and a refusal code. Its Plan records what this quest's
code must keep conforming to.

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
- ✅ A relay pulls payload upstream only for payload demand, and holds at
  most one upstream subscription per mode for a track. Metadata demand is
  served from a payload copy while the relay holds one, and otherwise
  travels upstream as a metadata subscription. When the last payload reader
  leaves and metadata demand remains, the relay opens the metadata
  subscription and closes the payload one.
- ✅ An older version never emulates it by fetching and discarding media: it
  reports the mode unsupported. No moq-transport counterpart is needed. A
  lite-07 peer that refuses the mode is reported the same way, and a relay
  whose upstream refuses refuses its own readers.

What the parked PR learned, on branch `quest/m3/route-health/meta` at
`c00addff48`:

- Carries over: the payload-less FRAME in both codecs, whose bytes
  [lite07-metadata](/quest/m2/lite07-metadata.md) fixes; a header read that
  returns the declared size without waiting for or inflating a compressed
  frame (`read_info`, `readInfo`); the bindings' header read; the refusal on
  lite-06 and moq-transport; and the interop lane.
- A history fill is payload demand of its own. There it switched the relay's
  one shared upstream subscription to payload, so live groups arrived with
  payload until the fill was answered. A subscription per mode removes the
  case; pin it.
- The TLA+ subscription model was extended for promotion and found six bugs
  there. With fixed modes it needs no promotion rule; check that two
  subscriptions to one track in different modes keep the existing accounting.
- Adding a field to the C `moq_subscription` struct changes its ABI.

Mirror the API across Rust, JS, and every binding that exposes
`Subscription`.

Tests: metadata-only traffic across several hops carries no payload; a relay
whose last payload reader leaves keeps forwarding headers to its metadata
readers; mixed demand; a payload subscription opened mid-group from a
position seen as metadata; a fill while only metadata demand stands; empty
frames; cancellation; refusal on an unsupported version and by a lite-07
peer without the mode. Sweep tracks, readers, and relay depth for bytes,
wakes, and memory. Add the mode to `just test interop --all`.

Public API: a metadata subscription in Rust, JS, and the bindings. Wire: none
beyond [lite07-metadata](/quest/m2/lite07-metadata.md); a lite-07 publisher
of ours serves the mode instead of refusing it.

## Required

- [Metadata mode on the lite-07 wire](/quest/m2/lite07-metadata.md) - the SUBSCRIBE mode, payload-less FRAME, and refusal this serves
