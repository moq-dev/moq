# [XL] Lite-07 ranges

## Goal

A relay fills a lite-07 upstream by subscribing to the missing ranges, and
recovers a dropped group with a one-group SUBSCRIBE. SUBSCRIBE_FINISH signals
a track's end and the Subscribe Stream stays open, so one-range reads back to
back on one lite-07 session complete. FETCH stays on lite-07 until
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
waited for Fetch as a range's miss judging (a miss with no feed frontier, and
holes below where a live feed started) and its cached backfill rule. Without
them a range fill is a functional regression from FETCH, which answers a miss
at once. Both landed in
[moq-dev/moq#7545](https://github.com/moq-dev/moq/pull/7545) on 2026-11-02.
Fetch as a range finished in
[moq-dev/moq#7625](https://github.com/moq-dev/moq/pull/7625), which kept
`track::Consumer::fetch_group` public as the single-group fetch and moved its
remaining lite-07 work here (maintainer, 2026-11-02): the explicit end, the
back-to-back hang, and the one-group recovery below.

### Range fill through the subscription

Lite-07 SUBSCRIBE and SUBSCRIBE_UPDATE carry the range list (`lite::Range`,
typed anchors), and the publisher serves it from the model. Older wires map at
the codec, and a merged request goes upstream as one span, from group 0 when
its starts mix (`wire_span`). Today a lite-07 publisher FINs a covered bounded
subscription without SUBSCRIBE_END, and the Rust copy settles its tail, keeps
the cached copy, and subscribes again only for demand the covered ranges leave
out. The explicit end below replaces that FIN; build the fill on it, not on
the FIN.

The first attempt
([moq-dev/moq#7487](https://github.com/moq-dev/moq/pull/7487), WIP commit
[b7f573d](https://github.com/moq-dev/moq/commit/b7f573d755ae53e6f2bdb94cc480ae47fd6ba254)) built this in
`rs/moq-net/src/lite/subscriber.rs` and was moved back out until the
requirement landed. Reuse its design:

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
whole) passed. These failed, and must pass on #7545's miss judging:

- On-demand `requests()` tracks with no live feed, such as moq-archive's, never
  ask a fill and park (`untimed::a_fetched_untimed_frame_arrives_untimed`,
  `fetch_cancel::a_refetch_within_the_linger_stays_on_the_relay_lite07`,
  `subscription_end_integrity::a_session_close_cancels_a_pending_fetch_from_either_side`).
  #7545's miss with no feed frontier covers them.
- A missing or expired group fetched with no budget parks where FETCH answered
  NotFound at once (`rejoin::{rejoined,taken_over}_quiet_catalog_survives_cache_expiry`).
  #7545's holes below where a live feed started cover them.
- A relay re-asks upstream for groups it cached, since its covered memory
  leaves with the downstream demand
  (`fetch_fanout::sequential_viewers_share_the_relays_fetched_groups`, two
  upstream SUBSCRIBEs where FETCH needed one). #7545's cached
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
  Since settled in part: datagrams become their own track kind, which takes
  no ranges (maintainer, 2026-11-01; see "Ends are signaled" below).
- **Fills from partway through a group.** Route recovery (`resume::Recover`)
  fills from frame N, which a whole-group range cannot say. Recommended: keep
  FETCH on lite-07 for a fill whose first group starts past frame 0, as
  `resume::Recover` does today (`group::Fetch::frame_start`). The alternative asks
  for the whole group and drops the frames the reader already has.
- **One budget caps every range.** Merging the fills into the upstream
  Subscriber Max Age takes the most tolerant budget, which loosens the live
  feed's cap while a fill is out
  (`route_change::resume_delivers_the_outage_tail_within_the_budget` delivers
  a stale group 5). Recommended: settle the partial-group question first,
  since the failing fill is a recovery fill, and rerun the test. The
  alternatives fill under the demand's budget, refusing older groups as Old,
  or add a per-range budget to the wire.
- **A range added after SUBSCRIBE_OK has no reported start.** SUBSCRIBE_OK's
  Range Starts are positional and sent once, yet the subscriber judges a
  bounded range complete from them, on a stream that stays open for later
  updates. So a Relative or Time range a SUBSCRIBE_UPDATE adds never learns
  where it started. Raised by the closed Range ID prototype
  ([moq-dev/moq#7207](https://github.com/moq-dev/moq/pull/7207)), whose
  recommendation was to decide here whether ranges get implicit IDs so a later
  message can report a start. SUBSCRIBE_UPDATE keeps restating the whole list
  either way: each range is a few bytes, and restating stays stateless. If IDs
  are adopted, #7207 is the reference encoding.
- `auth::lite_07_narrowing_resets_a_fetch_in_flight` read `Stream(Cancel)`
  instead of `Unauthorized`/`Unroutable` when the grant narrowed mid-group;
  the refusal must carry the grant's error.

The Rust copy sends a relative or time end as open, since nothing names a
resolved end and it resolves the end itself (`wire_ranges`), so only absolute
bounded ranges complete upstream. A hole on a live track counts as dropped
once the feed gives up on it, past the arrival grace (#7545's bounded wait,
`TrackState::miss_grace`).

### Ends are signaled, not inferred

The maintainer, 2026-11-01, on
[moq-dev/moq#7545](https://github.com/moq-dev/moq/pull/7545): "we should have
a SUBSCRIBE_END message or something to signal the last group of a track? we
were using QUIC FIN to signal that but that prevents updates in the future, so
we should have a signal instead". Settled the same day:

- No SUBSCRIBE_COVERED message: a subscriber judges a bounded range complete
  from the groups it received, SUBSCRIBE_DROP, the Range Starts in
  SUBSCRIBE_OK, and SUBSCRIBE_FINISH. Drops and groups name absolute
  sequences, so the subscriber judges against its latest ranges and an update
  in flight changes nothing it has counted; the FIN's race with an unread
  update goes away.
- "Keep stream open": SUBSCRIBE_FINISH names the track's final group and no
  longer FINs the Subscribe Stream, so an update can still reach an ended
  track's history. A covered bounded subscription is not FINed either.
- ✅ Rename SUBSCRIBE_END to SUBSCRIBE_FINISH. It matches TRACKS_FINISH and
  `track::Producer::finish`, and "END" would read as closing the stream, which
  it no longer does.
- Before SUBSCRIBE_OK a lite-07 publisher holds back what it gave up
  (`TrackRun::skipped`) and answers only once a group stream resolves the
  start, so a bounded subscription none of whose groups it can serve gets
  neither, and only the covered FIN ends it today. Once such a subscription is
  covered, the publisher sends SUBSCRIBE_OK and then the drops it held.
- The subscriber FINs the stream once it wants nothing more, and the publisher
  FINs in reply.
- Datagrams take no ranges, since they become their own track kind
  (maintainer, 2026-11-01). The datagram-tracks plan
  ([moq-dev/moq#7575](https://github.com/moq-dev/moq/pull/7575)) touches this
  quest too, and should take this design and the rename as given.

The draft's exception for a start past the publisher's largest position should
also say that a relay knows that position only from its upstream's answer,
never from its cache (maintainer, 2026-11-01: "NEVER use the cache as the live
edge").

### Back-to-back one-range reads

Acceptance test (maintainer, 2026-11-02, on
[moq-dev/moq#7625](https://github.com/moq-dev/moq/pull/7625)): on lite-07,
one-range reads of the same track through one session, groups 1 then 2, and
2, 1, 0, each complete, direct and through a relay. Today each hangs on the
later read (on 2, and on 0). Lite-05/06 and moq-transport pass. It also hangs
hls's `a_live_fmp4_import_serves_segments_over_lite07` once hls reads a
segment's groups that way. Reproduce with sequential `one_range` reads in
`range_read.rs`'s fixture.

The cause is the publisher's end-of-subscription FIN on a covered bounded
subscription, traced with tracing on:

- When the copy widens `1..2` to `1..3` with SUBSCRIBE_UPDATE while the
  publisher FINs the covered `1..2` without reading it, the copy's tail owes
  `1..3` (`SubStream::covered`, `updated` set) and waits its grace, which is
  `Duration::MAX` with no max delay (`Subscription::budget`). Group 2 never
  comes and nothing re-subscribes.
- When the FIN lands first (as on main before
  [moq-dev/moq#7625](https://github.com/moq-dev/moq/pull/7625)), the tail
  settles, `TrackServe::begin` finds the lingering demand equal to the covered
  ranges and keeps `Sub::None`, and the second read's demand never wakes the
  serve loop again, so the copy is left with no upstream subscription.

No SUBSCRIBE_DROP is involved, so the one-group recovery below does not reach
it, and a fresh SUBSCRIBE while group 2 may still be in flight could deliver
it twice. Fix it with the explicit end above: the Subscribe Stream stays open
and the covered FIN goes, so the publisher reads the update and serves or
drops the group.

### Recovering a dropped group

Decided 2026-11-02 (maintainer, on
[moq-dev/moq#7625](https://github.com/moq-dev/moq/pull/7625)): "lite-07 can
issue a one group SUBSCRIBE as a FETCH replacement (no max-delay) if an
existing SUBSCRIBE had it dropped", "and only when the group is officially
marked DROP so we don't receive two copies of it". A group a reader still
wants, once an existing subscription names it in SUBSCRIBE_DROP, is recovered
by a new SUBSCRIBE covering only that group with no max delay, never on a
guess, a timeout, or while the group may still arrive. This is lite-07's
FETCH replacement for a dropped group, so that
[Remove FETCH from lite-07](/quest/m1/subscribe-ranges/lite07-fetch.md) can
remove FETCH from Rust and JS together, as decided on 2026-10-25. Tests: a
group still in flight on the existing subscription sends no second SUBSCRIBE,
and a dropped group is recovered exactly once.

### Draft and checks

Update `drafts/draft-lcurley-moq-lite.md` with each wire change here,
including that a subscriber fills missing groups through its ranges. Run
`just drafts check` and `just test interop --all`, and the `moq-net-tests`
`restart::retired_fetch_lite07` and `moq-relay` suites.

## Related

- [Remove FETCH from lite-07](/quest/m1/subscribe-ranges/lite07-fetch.md) - removes FETCH from lite-07 in both implementations once this and JS ranges land
