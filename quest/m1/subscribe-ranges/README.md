# Subscribe ranges

## Goal

A moq-lite-07 SUBSCRIBE replaces FETCH. One subscription asks for any number
of group ranges, past or live, bounded by group or by time, in ascending or
descending order, and the
publisher serves their union from one cursor without sending a group twice.
Every sequence in range either arrives or is named by a SUBSCRIBE_DROP, so
nothing blocks on a cache miss. Relays fill misses upstream with ranges, not
one request per group, including over moq-transport where group numbers may be
sparse. Lite FETCH is removed from lite-07.

## Plan

Decided in planning (09-29), after the fetch-span agent (#4558) found that a
relay bounds a sparse FETCH only if the model can request ranges:

- **Wire (lite-07, unpublished).** SUBSCRIBE carries a list of group ranges
  (an open end means live; ends may be relative, below) in place of Group Start/End, plus
  `order: asc | desc` (desc, newest first, is the default). Order landed in
  #5385; over moq-transport the upstream order is fixed at SUBSCRIBE, since no
  draft lets an update change it.
  Subscriber Max Age (the model's `max_delay`) stays as a cap alongside the
  ranges, since an untimed track's groups have no timestamp to bound. Decided 2026-10-10:
  historical requests also obey that cap and publisher max-age. An omitted
  FETCH budget means publisher max-age alone; translation to a range must
  not accidentally give it the live SUBSCRIBE zero-delay default. SUBSCRIBE_UPDATE
  replaces the list. Lite FETCH is removed from
  lite-07; published versions keep it, answered through the same model.
- **Holes.** SUBSCRIBE_DROP (restored in lite-07 by
  [SUBSCRIBE_DROP](/quest/m1/subscribe-drop.md)) names every sequence in the
  requested ranges the publisher won't deliver: never existed, expired, or
  refused upstream. Datagram groups are always dropped in a past range (#4982). The
  publisher delivers what it has and fills the rest as upstream answers.
- **Model.** `Subscription` gains `ranges` beside its `order`, mirroring the wire;
  `request_groups` as a separate API was rejected. A publisher's
  `track::Dynamic` receives range requests for misses, replacing per-group
  `requested_group`, and `fetch_group` becomes a one-range subscription or is
  removed. This breaks published moq-net APIs.
- **moq-transport.** Some deployments use non-contiguous group numbers, so a
  relay never probes upstream one group at a time. Upstream, each run of
  locally missing groups becomes one standalone range FETCH, capped below the
  upstream's live group (which arrives via SUBSCRIBE), so it never blocks.
  moq-transport caps a FETCH at the Largest Object, so it can return the live
  group's existing prefix but never wait for its future objects. Downstream,
  an IETF FETCH is served from the model's ranges, capped at the Largest
  Object.
- **Relative ends** (maintainer, 2026-10-11, superseding the 10-07 `Live`
  flag and closing #5085). Each range end is `Absolute(group, frame)` or
  `Relative(n)`, naming the group n before the latest; either end may be
  relative, and an omitted end is live. An offset past the track's start
  clamps to group 0. Before any group exists, relative ends resolve when the
  first one arrives, so `Relative(0)..` starts at it. The cross-language and
  bridge tests cover both edges. Ends are inclusive of whole groups:
  a relative start resolves to frame 0 of that group, and a relative end to
  just after its last frame (the model's exclusive `Position::after_group`),
  so `Absolute(0)..Relative(0)` includes the latest group. Live is `Relative(0)..`, so no separate `Live` field exists.
  Merging subscribers is the union of their ranges, so a range above the
  live edge never hides the latest group from a merged live subscriber,
  which is the starvation the flag existed to prevent. Subscriber Max Age
  still caps every range, so `Absolute(0)..` means everything not stale.
  A relay that cannot resolve a relative end locally forwards it upstream as
  relative.
- **Time ends** (maintainer, 2026-10-17, for DVR seek in the
  [archive line](/quest/m1/archive/README.md)). A range end may also be
  `Time(pts)`, in the track's timescale. Whoever holds the track resolves it:
  the cache from its groups' timestamps, or a `track::Dynamic` handler such as
  an archive consumer. A start resolves to the group containing pts, and an
  end to just after that group. A lite-07 relay that cannot resolve one
  forwards it upstream unchanged. An untimed track refuses a time end, and a
  moq-transport bridge resolves it locally or refuses it, since moq-transport
  has no time filter. It is part of lite-07 and must land before
  [lite-07 finalizes](/quest/m1/lite07-finalize.md), which this line gates. This amends the 09-29
  premise that ranges are group-only because a group's timestamp isn't always
  known: timed tracks know it, and untimed ones refuse.
- Ranges are frame-precise (`Position`), not whole groups. The IETF joining
  FETCH for a mid-group SUBSCRIBE's uncached prefix stays, because today's
  bridge relies on it (maintainer, 09-29).
- The relay half of fetch-span moves here; #4558 landed only the no-handler
  skip.

This line owns the end-to-end test: a relay with a sparse cache answers a
wide past range plus live in both orders, over lite-07 and moq-transport, with
every sequence delivered or dropped.

## Required

- [Dynamic range requests](/quest/m1/subscribe-ranges/dynamic.md) - `Dynamic` fills misses by range, a subscriber reads only its own ranges, and a range end may be a time
- [SUBSCRIBE_DROP](/quest/m1/subscribe-drop.md) - publishers name every group they won't deliver
- [Lite-07 ranges](/quest/m1/subscribe-ranges/lite.md) - the lite-07 wire, Rust publisher and subscriber, and the draft
- [moq-transport ranges](/quest/m1/subscribe-ranges/ietf.md) - range FETCH upstream per missing run, and non-blocking FETCH served downstream
- [JS ranges](/quest/m1/subscribe-ranges/js.md) - `@moq/net` model and lite-07 parity

## Related

- [Track priority scope](/quest/m1/track-priority-scope.md) - group order within a track now follows the subscription
- [IETF fill timeout](/quest/m1/ietf-fill-timeout.md) - bounds upstream waiting independently of content age
