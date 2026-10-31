# [L] Lite-07 ranges

## Goal

A relay fills a lite-07 upstream by subscribing to the missing ranges. Older
wires and moq-transport carry the anchors they can, and refuse only the rest. FETCH stays on lite-07 until
[Remove FETCH from lite-07](/quest/m1/subscribe-ranges/lite07-fetch.md)
removes it from both implementations.

## Plan

Decided 2026-10-25 (maintainer, on
[moq-dev/moq#6807](https://github.com/moq-dev/moq/pull/6807)): FETCH removal
moves out of this quest (to its own quest since 2026-10-27), since JS still requests and serves wire FETCH on lite-07
(`track.Consumer.fetchGroup`), so a Rust-only removal would drop a current
cross-language capability. This quest keeps FETCH on lite-07 and makes the
range fill work without it.

### Range fill through the subscription

Lite-07 SUBSCRIBE and SUBSCRIBE_UPDATE carry the range list (`lite::Range`,
typed anchors), and the publisher serves it from the model. Older wires map at
the codec, and a merged request goes upstream as one span, from group 0 when
its starts mix (`wire_span`). A lite-07 publisher FINs a covered bounded
subscription without SUBSCRIBE_END; the Rust copy settles its tail, keeps the
cached copy, and subscribes again only for demand the covered ranges leave
out.

An attempt that routed lite-07 fills through `FeedFetches`, adding each held
fill's groups to the upstream ranges, broke fetches of an open or previously
cut group (`catalog_handover` fetch tests, `restart::retired_fetch_lite07`,
`fetch_cancel`, `untimed`, `rejoin`):

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
bounded ranges complete upstream. A hole on a live track counts as dropped
only once the feed gives up on it, past the arrivals grace;
[Fetch as a range](/quest/m1/subscribe-ranges/fetch-range.md) owns judging
such a miss sooner. A range served only by datagrams never completes: they
resolve no SUBSCRIBE_START, so a FIN overtaking them leaves the receiver no
owed range to wait on. Datagram fills need the receiver told what is owed
first.

### moq-transport anchors

Decided 2026-10-27 (maintainer): a bridge never sends a relative anchor to
moq-transport. It resolves `Relative(n)` to an absolute Group ID, from its
cache or the live edge, before sending, replacing the planned mapping of
`Relative(n)..` onto draft-22's `Filter::Relative(n + 1)` and draft 18's
relative joining FETCH. Reason: both count back by Group ID arithmetic, which
breaks once [time addressing](/quest/m1/time-addressing/model.md) makes legacy
Group IDs sparse start ticks. A range with a bounded end
resolves both bounds against the Largest Object and goes upstream as an
absolute range. Live keeps today's join (`ietf::subscriber::subscribe_join`). A copy already
resolves what its cache settles (`ResolvedRanges`, from
[moq-dev/moq#6815](https://github.com/moq-dev/moq/pull/6815)) and refuses the
rest (`anchored_cached`); what remains is a cold copy resolving `Relative(n)`
from the Largest Object of a live join instead of refusing it.
Each mapping that makes a wire carry an anchor lifts that wire's
`Unsupported` refusal for it, and extends the per-wire table in
`rs/moq-net/tests/integration/range_anchors.rs`.

Update `drafts/draft-lcurley-moq-lite.md` with each wire change here. Run
`just drafts check` and `just test interop --all`.

## Related

- [Remove FETCH from lite-07](/quest/m1/subscribe-ranges/lite07-fetch.md) - removes FETCH from lite-07 in both implementations once this and JS ranges land
