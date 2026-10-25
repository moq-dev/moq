# [L] Lite-07 ranges

## Goal

A relay fills a lite-07 upstream by subscribing to the missing ranges. Older
wires and moq-transport carry the anchors they can, and refuse only the rest.
FETCH stays on lite-07 until [JS ranges](/quest/m1/subscribe-ranges/js.md)
removes it from both implementations.

## Plan

Decided 2026-10-25 (maintainer, on
[moq-dev/moq#6807](https://github.com/moq-dev/moq/pull/6807)):

- **FETCH removal moves to JS ranges.** JS still requests and serves wire
  FETCH on lite-07 (`track.Consumer.fetchGroup` emits `StreamId.Fetch`, and
  `lite/connection.ts` dispatches it), so a Rust-only removal would drop a
  current cross-language capability. This quest lands the Rust range fill and
  bounded completion; the coordinated Rust and JS removal, with its draft
  change, lands in JS ranges. Keeping the removal here with the JS
  request/response migration was rejected.
- **A covered bounded subscription FINs on lite-07.** Once every range has a
  bounded end and every group in them is delivered or named by
  SUBSCRIBE_DROP, the lite-07 publisher FINs without SUBSCRIBE_END. The FIN
  ends the subscription, never the track: the subscriber settles its tail,
  keeps the cached copy, and resets no group stream still being read.
  Published lite versions and moq-transport keep parking, since their
  subscribers read a FIN as the track's end. Parking on every wire with a
  separate bounded-fill completion was rejected.

Bounded completion is in place: the lite-07 publisher FINs once
`track::Subscriber::covered` holds, and the Rust copy settles the tail on the
covered ranges, then subscribes again only for demand they leave out. A hole
on a live track counts as dropped only once the feed gives up on it, past the
arrivals grace; [Fetch as a range](/quest/m1/subscribe-ranges/fetch-range.md)
owns judging such a miss sooner.

Lite-07 SUBSCRIBE and SUBSCRIBE_UPDATE already carry the range list
(`lite::Range`, typed anchors), and the publisher serves it from the model.
Older wires map at the codec, and a merged request goes upstream as one span,
from group 0 when its starts mix (`wire_span`).

Removing FETCH from lite-07 needs the fill path to work through the
subscription first. An attempt that routed lite-07 fills through
`FeedFetches`, adding each held fill's groups to the upstream ranges, broke
fetches of an open or previously cut group (`catalog_handover` fetch tests,
`restart::retired_fetch_lite07`, `fetch_cancel`, `untimed`, `rejoin`):

- A held fill is judged by the track's arrivals ledger, which still holds the
  cut group from the subscription that left. It settles `NotFound` before the
  new subscription delivers the group. Judge it against the subscription that
  asked for it.
- Dropping a fill's range once it is answered shrinks or closes the upstream
  subscription while its group stream is still arriving, which resets it.
  Keep the range until that stream finishes.
- A relay serving the fill from its own cut copy has the same problem one hop
  up.

The Rust copy sends a relative or time end as open, since nothing names a
resolved end and it resolves the end itself (`wire_ranges`), so only absolute
bounded ranges complete upstream.

moq-transport's relative offsets differ by one: draft-20 `Relative(0)` is
the next group and `Relative(1)` the current one, so an open-ended
`Relative(n)..` maps to draft-20 `Relative(n + 1)`, saturating. Older drafts
only encode a relative 0, so there it is SUBSCRIBE plus a relative joining
FETCH n groups back, as the live join already does. A range with a bounded
end resolves both bounds against the Largest Object and goes upstream as an
absolute range. Live keeps today's join (`ietf::subscriber::subscribe_join`).
Each mapping that makes a wire carry an anchor lifts that wire's
`Unsupported` refusal for it, and extends the per-wire table in
`rs/moq-net/tests/integration/range_anchors.rs`.

Update `drafts/draft-lcurley-moq-lite.md` with each wire change here. Run
`just drafts check` and `just test interop --all`.

Decided 2026-10-22 (maintainer): SUBSCRIBE_OK carries the resolved start for
a relative range start, so a reader's ordered cursor takes its floor from it
([Container reads drop backfill](/quest/m1/read-backfill.md)).

## Related

- [Container reads drop backfill](/quest/m1/read-backfill.md) - floors its ordered cursor on the resolved start
- [JS ranges](/quest/m1/subscribe-ranges/js.md) - removes FETCH from lite-07 in both implementations once this lands
