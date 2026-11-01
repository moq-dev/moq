# [L] Lite-07 ranges

## Goal

A relay fills a lite-07 upstream by subscribing to the missing ranges. FETCH stays on lite-07 until
[Remove FETCH from lite-07](/quest/m1/subscribe-ranges/lite07-fetch.md)
removes it from both implementations.

## Plan

Decided 2026-10-25 (maintainer, on
[moq-dev/moq#6807](https://github.com/moq-dev/moq/pull/6807)): FETCH removal
moves out of this quest (to its own quest since 2026-10-27), since JS still requests and serves wire FETCH on lite-07
(`track.Consumer.fetchGroup`), so a Rust-only removal would drop a current
cross-language capability. This quest keeps FETCH on lite-07 and makes the
range fill work without it.

Decided 2026-11-01 (maintainer, on
[moq-dev/moq#7487](https://github.com/moq-dev/moq/pull/7487)): this quest
waits for [Fetch as a range](/quest/m1/subscribe-ranges/fetch-range.md)'s miss
judging (a miss with no feed frontier, and holes below where a live feed
started) and its cached backfill rule. Without them a range fill is a
functional regression from FETCH, which answers a miss at once.

### Range fill through the subscription

Lite-07 SUBSCRIBE and SUBSCRIBE_UPDATE carry the range list (`lite::Range`,
typed anchors), and the publisher serves it from the model. Older wires map at
the codec, and a merged request goes upstream as one span, from group 0 when
its starts mix (`wire_span`). A lite-07 publisher FINs a covered bounded
subscription without SUBSCRIBE_END; the Rust copy settles its tail, keeps the
cached copy, and subscribes again only for demand the covered ranges leave
out. The maintainer has since directed an explicit end signal (SUBSCRIBE_END
or similar) in place of that FIN, since a FIN prevents later updates (on
[moq-dev/moq#7545](https://github.com/moq-dev/moq/pull/7545)).
[Fetch as a range](/quest/m1/subscribe-ranges/fetch-range.md) owns that
design; build the fill on whatever it settles, not on the FIN.

The first attempt
([moq-dev/moq#7487](https://github.com/moq-dev/moq/pull/7487), WIP commit
[b7f573d](https://github.com/moq-dev/moq/commit/b7f573d755ae53e6f2bdb94cc480ae47fd6ba254)) built this in
`rs/moq-net/src/lite/subscriber.rs` and was moved back out until the
requirement lands. Reuse its design:

- A `RangeFills` set holds each fill and asks for its next owed run
  (`Fill::next_run`), added as an absolute bounded range after the demand's
  ranges on the upstream SUBSCRIBE or SUBSCRIBE_UPDATE. With no demand open,
  a bounded SUBSCRIBE asks for the fills alone. A fill answers itself from the
  cache as its groups arrive.
- A fill is judged only against the subscription that asked for it, not the
  track's arrivals ledger, which still holds a group an earlier subscription
  cut. A SUBSCRIBE_DROP over its groups, a SUBSCRIBE_START past its range's
  start, or a covered end refuses it, so a cut group is filled whole.
- A run stays on the wire while a fill owes part of it, then until each of its
  streams still being read finishes, so narrowing never resets a group in
  flight.
- Only the demand's ranges set the floor and the live state. A subscription
  carrying only fills sets neither, and canceling it does not idle the copy.
  If the demand leaves while fills are out, the subscription keeps the
  demand's ranges until they are answered.
- A NotFound drop or a covered end names the fill's groups dropped
  (`Fill::drop_groups`), as a FETCH miss does; Old and NOT_FETCHABLE reject
  them, and any other error refuses only that request.

Its integration tests (`range_fill.rs`: a miss subscribes and sends no FETCH,
a miss rides the live subscription then retires its range, a group the
publisher never had becomes a miss after the grace, a cut group is filled
whole) passed. These failed, and must pass once the requirement lands:

- On-demand `requests()` tracks with no live feed, such as moq-archive's, never
  ask a fill and park (`untimed::a_fetched_untimed_frame_arrives_untimed`,
  `fetch_cancel::a_refetch_within_the_linger_stays_on_the_relay_lite07`,
  `subscription_end_integrity::a_session_close_cancels_a_pending_fetch_from_either_side`).
  Fetch as a range's miss with no feed frontier covers them.
- A missing or expired group fetched with no budget parks where FETCH answered
  NotFound at once (`rejoin::{rejoined,taken_over}_quiet_catalog_survives_cache_expiry`).
  Fetch as a range's holes below where a live feed started cover them.
- A relay re-asks upstream for groups it cached, since its covered memory
  leaves with the downstream demand
  (`fetch_fanout::sequential_viewers_share_the_relays_fetched_groups`, two
  upstream SUBSCRIBEs where FETCH needed one). Fetch as a range's cached
  backfill rule covers it: a relay leaves cached groups out of a bounded
  upstream range. On lite-07 the test counts SUBSCRIBE streams, not FETCH.

Open questions the attempt raised, each with the attempt's recommendation:

- **Datagram sequences.** A range fill of a sequence served only as a datagram
  never completes: datagrams resolve no SUBSCRIBE_START, so a FIN or end
  overtaking them leaves the receiver no owed range to wait on
  (`datagram::a_fetched_datagram_is_refused_per_version`). Recommended: a
  lite-07 publisher sends SUBSCRIBE_START for a bounded past range, then
  SUBSCRIBE_DROP with NOT_FETCHABLE for each sequence it holds only as a
  datagram; the subscriber already maps that drop onto the fill. This is a
  wire change. The alternative keeps FETCH for a fill the first answer shows is
  a datagram, at a round trip and a second path.
- **Fills from partway through a group.** Route recovery (`resume::Recover`)
  fills from frame N, which a whole-group range cannot say. Recommended: keep
  FETCH on lite-07 for a fill whose first group starts past frame 0, matching
  Fetch as a range's crate-private fetch for that case. The alternative asks
  for the whole group and drops the frames the reader already has.
- **One budget caps every range.** Merging the fills into the upstream
  Subscriber Max Age takes the most tolerant budget, which loosens the live
  feed's cap while a fill is out
  (`route_change::resume_delivers_the_outage_tail_within_the_budget` delivers
  a stale group 5). Recommended: settle the partial-group question first,
  since the failing fill is a recovery fill, and rerun the test. The
  alternatives fill under the demand's budget, refusing older groups as Old,
  or add a per-range budget to the wire.
- `auth::lite_07_narrowing_resets_a_fetch_in_flight` read `Stream(Cancel)`
  instead of `Unauthorized`/`Unroutable` when the grant narrowed mid-group;
  the refusal must carry the grant's error.

The Rust copy sends a relative or time end as open, since nothing names a
resolved end and it resolves the end itself (`wire_ranges`), so only absolute
bounded ranges complete upstream. A hole on a live track counts as dropped
only once the feed gives up on it, past the arrivals grace, until Fetch as a
range judges such a miss sooner.

Update `drafts/draft-lcurley-moq-lite.md` with each wire change here,
including that a subscriber fills missing groups through its ranges. Run
`just drafts check` and `just test interop --all`, and the `moq-net-tests`
`restart::retired_fetch_lite07` and `moq-relay` suites.

## Required

- [Fetch as a range](/quest/m1/subscribe-ranges/fetch-range.md) - its miss judging and cached backfill rule, which a range fill needs to answer a miss as FETCH does

## Related

- [Remove FETCH from lite-07](/quest/m1/subscribe-ranges/lite07-fetch.md) - removes FETCH from lite-07 in both implementations once this and JS ranges land
