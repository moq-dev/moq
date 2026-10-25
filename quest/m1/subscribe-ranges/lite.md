# [L] Lite-07 ranges

## Goal

FETCH is gone from lite-07, and a relay fills a lite-07 upstream by
subscribing to the missing ranges. Older wires and moq-transport carry the
anchors they can, and refuse only the rest.

## Plan

Before implementing the remainder, settle its scope with the
[JS ranges](/quest/m1/subscribe-ranges/js.md) quest. JS still serves and requests
FETCH on lite-07: `js/net/src/lite/subscriber.ts` `fetchGroup`/`#fetchSetup`
emits `StreamId.Fetch`, `js/net/src/lite/connection.ts` dispatches it, and
`js/net/src/lite/fetch.ts` accepts it. A Rust-only removal would therefore
remove a current cross-language capability on the explicitly offered WIP
version. JS ranges currently Requires this quest, so the plans cannot leave
that migration to an unchanged child while calling this wire change complete.

Options for the maintainer:

- Recommended: land the Rust range-fill and completion implementation first,
  then move final FETCH removal in both implementations to JS ranges. This
  keeps the two implementation rollouts independently landable; it changes this
  quest's Goal and the JS quest's boundary only after approval.
- Keep FETCH removal here and include the required JS request/response
  migration in this PR. The remaining public JS range API and dynamic producer
  work can stay with JS ranges, but this PR must identify that boundary and
  preserve every currently supported lite-07 fetch consumer.

Also confirm the bounded-subscription contract before changing it. This plan
expects a lite-07 FIN without SUBSCRIBE_END when ranges are covered, while
[Fetch as a range](/quest/m1/subscribe-ranges/fetch-range.md) says wire
publishers keep parking so an update can widen the range. Current
`lite/publisher.rs` `TrackRun::update` treats the end as a serving cap and
`model/track.rs` `TrackState::poll_next_in_range` explicitly parks at it;
`lite/subscriber.rs` `ServeLoop::poll` refuses a FIN without SUBSCRIBE_END on
lite-05 and later. The draft's Subscribe section also requires SUBSCRIBE_END
before a publisher FIN, apart from subscriber cancellation. Recommendation:
make bounded completion an explicit lite-07 exception and preserve parking on
published lite versions and moq-transport. Keeping all wire subscriptions
parked is the alternative and needs a distinct completion mechanism for a
bounded fill. A subscription completing must not declare its cached track
finished or reset a group stream still being read.

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

The intended bounded-completion path settles the subscription tail and keeps
the copy once every owed range is covered; the contract above must first be
aligned across the quest plans, the draft, and both implementations. The Rust
copy currently sends a relative or time end as open, since nothing names a
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
