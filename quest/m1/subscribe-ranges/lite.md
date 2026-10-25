# [L] Lite-07 ranges

## Goal

FETCH is gone from lite-07, and a relay fills a lite-07 upstream by
subscribing to the missing ranges. Older wires and moq-transport carry the
anchors they can, and refuse only the rest.

## Plan

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

The lite-07 publisher FINs a subscription without SUBSCRIBE_END once the
model covers its ranges (a relative or time end, or bounded sparse ranges).
The subscriber treats that as a protocol violation today. It should settle
the tail and keep the copy, ending with no grace once the owed range is
covered (see "Delete the old start bookkeeping" in the epic README). The Rust
copy sends a relative or time end as open for now, since nothing names a
resolved end and it resolves the end itself (`wire_ranges`).

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

Update `drafts/draft-lcurley-moq-lite.md` with the FETCH removal and its
lite-07 changelog bullets (FETCH max age and epoch). Published versions
(lite-03..06) keep FETCH. Run `just drafts check` and `just test interop --all`.

Decided 2026-10-22 (maintainer): SUBSCRIBE_OK carries the resolved start for
a relative range start, so a reader's ordered cursor takes its floor from it
([Container reads drop backfill](/quest/m1/read-backfill.md)).

## Related

- [Container reads drop backfill](/quest/m1/read-backfill.md) - floors its ordered cursor on the resolved start
