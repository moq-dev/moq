# Subscribe ranges

## Goal

A moq-lite-07 SUBSCRIBE replaces FETCH. One subscription asks for any number
of group ranges, past or live, in ascending or descending order, and the
publisher serves their union from one cursor without sending a group twice.
Every sequence in range either arrives or is named by a SUBSCRIBE_DROP, so
nothing blocks on a cache miss. Relays fill misses upstream with ranges, not
one request per group, including over moq-transport where group numbers may be
sparse. Lite FETCH is removed from lite-07.

## Plan

Decided in planning (09-29), after the fetch-span agent (#4558) found that a
relay bounds a sparse FETCH only if the model can request ranges:

- **Wire (lite-07, unpublished).** SUBSCRIBE carries a list of group ranges
  (an open end means live) in place of Group Start/End, plus
  `order: asc | desc` (desc, newest first, is today's rule and the default).
  Max Age stays as a cap alongside the ranges, since a group's timestamp isn't
  always known. SUBSCRIBE_UPDATE replaces the list. Lite FETCH is removed from
  lite-07; published versions keep it, answered through the same model.
- **Holes.** SUBSCRIBE_DROP (restored in lite-07 by
  [SUBSCRIBE_DROP](/quest/m1/subscribe-drop.md)) names every sequence in the
  requested ranges the publisher won't deliver: never existed, expired, or
  refused upstream. Datagram groups are always dropped in a past range (the datagrams-are-live-only quest, #4551). The
  publisher delivers what it has and fills the rest as upstream answers.
- **Model.** `Subscription` gains `ranges` and `order`, mirroring the wire;
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
- Ranges are frame-precise (`Position`), not whole groups. The IETF joining
  FETCH for a mid-group SUBSCRIBE's uncached prefix stays, because today's
  bridge relies on it (maintainer, 09-29).
- The relay half of fetch-span (#4558, from the finished Moxygen line) moves
  here; #4558 lands only the no-handler skip.

This line owns the end-to-end test: a relay with a sparse cache answers a
wide past range plus live in both orders, over lite-07 and moq-transport, with
every sequence delivered or dropped.

## Required

- [Model ranges](/quest/m1/subscribe-ranges/model.md) - `Subscription` carries ranges and order, and `Dynamic` fills misses by range
- [SUBSCRIBE_DROP](/quest/m1/subscribe-drop.md) - publishers name every group they won't deliver
- [Lite-07 ranges](/quest/m1/subscribe-ranges/lite.md) - the lite-07 wire, Rust publisher and subscriber, and the draft
- [moq-transport ranges](/quest/m1/subscribe-ranges/ietf.md) - range FETCH upstream per missing run, and non-blocking FETCH served downstream
- [JS ranges](/quest/m1/subscribe-ranges/js.md) - `@moq/net` model and lite-07 parity

## Related

- [Track priority scope](/quest/m1/track-priority-scope.md) - group order within a track now follows the subscription
