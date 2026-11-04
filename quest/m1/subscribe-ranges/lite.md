# [L] Lite-07 ranges

## Goal

Lite-07 ranges carry implicit Range IDs, so a range a SUBSCRIBE_UPDATE adds
learns where it started. A relay fills a lite-07 upstream by subscribing to the
missing ranges, and recovers a dropped group with a one-group SUBSCRIBE. Lite-07
FETCH stays, for one group or a slice within one group
([Lite-07 FETCH is single-group only](/quest/m1/subscribe-ranges/lite07-fetch.md)).

## Plan

Decided 2026-10-25 (maintainer, on
[moq-dev/moq#6807](https://github.com/moq-dev/moq/pull/6807)): FETCH removal
moves out of this quest (to its own quest since 2026-10-27), since JS still requests and serves wire FETCH on lite-07
(`track.Consumer.fetchGroup`), so a Rust-only removal would drop a current
cross-language capability. Since 2026-11-03 FETCH stays on lite-07 for one
group or a slice within one group, and this quest fills multi-group ranges
through the subscription.

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

The explicit end landed in
[moq-dev/moq#7675](https://github.com/moq-dev/moq/pull/7675): SUBSCRIBE_END is
renamed SUBSCRIBE_FINISH, and on lite-07 neither it nor a covered bounded
subscription closes the Subscribe Stream. The publisher keeps reading updates
and FINs only in reply to the subscriber's FIN. Once no group can resolve a
held start (every group of a bounded subscription given up, or the track
finished), it sends SUBSCRIBE_OK and then the drops it held. The Rust copy
starts its tail at SUBSCRIBE_FINISH, waits for SUBSCRIBE_OK before judging it,
and FINs once it settles. That fixed back-to-back one-range reads
(`range_read::one_range_reads_back_to_back_each_complete`), and the Rust
copy's covered-FIN path (`SubStream::covered`, `ServeLoop::covered`) is gone.
Datagrams take no ranges, since they become their own track kind (maintainer,
2026-11-01; the datagram-tracks plan,
[moq-dev/moq#7575](https://github.com/moq-dev/moq/pull/7575)). The same PR
brought the JS lite-07 publisher and subscriber to the explicit end, so the two
implementations share the wire; JS still names it SUBSCRIBE_END in code.

Decided 2026-11-03 (maintainer, on
[moq-dev/moq#7675](https://github.com/moq-dev/moq/pull/7675)), answering the
first attempt's open questions:

1. **Land part 1:** ✅ land it now. Options: ✅ land now; keep a draft.
2. **Fill budget:** ✅ the live demand's budget. Groups that budget drops
   count as officially dropped, so the one-group SUBSCRIBE recovery (no max
   delay) brings them back, and the live cap never loosens. Options: ✅ live
   demand's budget; loosest budget; per-range wire budget.
3. **Partial-group fills:** "we can keep FETCH then but for a single group (or
   slice within a single group). SUBSCRIBE is for multiple groups and does not
   support slicing within a group." A fill starting past frame 0, such as route
   recovery's (`resume::Recover`), keeps its FETCH. See
   [Lite-07 FETCH is single-group only](/quest/m1/subscribe-ranges/lite07-fetch.md).
4. **Datagram-only groups:** ✅ wait for datagram tracks
   ([Datagram tracks](/quest/m1/datagram-tracks/README.md), planned in
   [moq-dev/moq#7575](https://github.com/moq-dev/moq/pull/7575)); after them
   ranges never carry datagrams. Options: ✅ wait for datagram tracks;
   NOT_FETCHABLE drops now; keep FETCH for these.
5. **Range IDs:** ✅ implicit range IDs now, using
   [moq-dev/moq#7207](https://github.com/moq-dev/moq/pull/7207)'s encoding, so
   a range added by SUBSCRIBE_UPDATE learns where it started. Options:
   absolute-only fills; ✅ implicit range IDs now.

Range IDs are this quest's next step. They did not fit #7675 as a checkpoint
addition: the encoding changes SUBSCRIBE_UPDATE in the Rust and JS codecs,
publishers, and subscribers together (#7207 was 1,420 lines across both and
now conflicts with main), and it needs a start report #7207 never defined.
What remains is Range IDs, then the range fill and the one-group recovery.

### Range IDs

[moq-dev/moq#7207](https://github.com/moq-dev/moq/pull/7207) is the reference:
each Range implicitly takes the next Range ID on the stream, from 0, and
SUBSCRIBE_UPDATE edits the live Ranges rather than restating them, with
`Remove Count` Range IDs to retire and then the Ranges to add. A Range ID never
assigned or already retired is a PROTOCOL_VIOLATION, and the live Ranges must
stay non-empty and fit in one SUBSCRIBE. SUBSCRIBE_OK's Range Starts answer the
Ranges live when it is sent, in Range ID order. A Range an update adds after
SUBSCRIBE_OK has its start reported by SUBSCRIBE_STARTED, below. Rust and JS
change together, with draft text and codec, publisher, and subscriber tests.

Decided 2026-11-03 (maintainer, on
[moq-dev/moq#7675](https://github.com/moq-dev/moq/pull/7675)): ✅ a new
**SUBSCRIBE_STARTED** message (type 0x3) carrying `Range ID` and
`Range Start`, sent when an added Range's start resolves. Options: ✅ new
SUBSCRIBE_STARTED; repeat SUBSCRIBE_OK. `Range Start` is encoded as in
SUBSCRIBE_OK (the Group plus one, 0 for none), and the subscriber owes the
added Range's groups from it as it does a SUBSCRIBE_OK Range Start.

### Range fill through the subscription

Lite-07 SUBSCRIBE and SUBSCRIBE_UPDATE carry the range list (`lite::Range`,
typed anchors), and the publisher serves it from the model. Older wires map at
the codec, and a merged request goes upstream as one span, from group 0 when
its starts mix (`wire_span`). A covered bounded subscription stays open, so a
fill joins the live subscription's ranges with a SUBSCRIBE_UPDATE.

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
  cut. A SUBSCRIBE_DROP over its groups, or a SUBSCRIBE_OK Range Start past
  its range's start, refuses it, so a cut group is filled whole. The WIP's
  covered-FIN refusal has no counterpart now: the publisher drops what it
  gives up instead.
- A run stays on the wire while a fill owes part of it, then until each of its
  streams still being read finishes, so narrowing never resets a group in
  flight.
- Only the demand's ranges set the floor and the live state. A subscription
  carrying only fills sets neither, and canceling it does not idle the copy.
  If the demand leaves while fills are out, the subscription keeps the
  demand's ranges until they are answered.
- A NotFound drop names the fill's groups dropped
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

Open questions the attempt raised, now decided (2026-11-03, above), kept for
their detail:

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
  no ranges (maintainer, 2026-11-01). Today a lite-07 publisher that served a
  bounded range's only group as a datagram never answers it with SUBSCRIBE_OK
  (`TrackRun::unservable`), so such a fill parks until its readers leave.
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
  or add a per-range budget to the wire. Since the one-group recovery below
  was decided (2026-11-02), filling under the demand's budget composes with
  it: a group the live budget drops as OLD is officially dropped, so a
  one-group SUBSCRIBE with no max delay recovers it, and the live cap never
  loosens. Decided that way on 2026-11-03.
- **A range added after SUBSCRIBE_OK has no reported start.** SUBSCRIBE_OK's
  Range Starts are positional and sent once, yet the subscriber judges a
  bounded range complete from them, on a stream that stays open for later
  updates. So a Relative or Time range a SUBSCRIBE_UPDATE adds never learns
  where it started. Raised by the closed Range ID prototype
  ([moq-dev/moq#7207](https://github.com/moq-dev/moq/pull/7207)), whose
  recommendation was to decide here whether ranges get implicit IDs so a later
  message can report a start. SUBSCRIBE_UPDATE keeps restating the whole list
  either way: each range is a few bytes, and restating stays stateless. IDs
  were adopted on 2026-11-03, with #7207's encoding, which edits rather than
  restates.
- `auth::lite_07_narrowing_resets_a_fetch_in_flight` read `Stream(Cancel)`
  instead of `Unauthorized`/`Unroutable` when the grant narrowed mid-group;
  the refusal must carry the grant's error.

The Rust copy sends a relative or time end as open, since nothing names a
resolved end and it resolves the end itself (`wire_ranges`), so only absolute
bounded ranges complete upstream. A hole on a live track counts as dropped
once the feed gives up on it, past the arrival grace (#7545's bounded wait,
`TrackState::miss_grace`).

### Recovering a dropped group

Decided 2026-11-02 (maintainer, on
[moq-dev/moq#7625](https://github.com/moq-dev/moq/pull/7625)): "lite-07 can
issue a one group SUBSCRIBE as a FETCH replacement (no max-delay) if an
existing SUBSCRIBE had it dropped", "and only when the group is officially
marked DROP so we don't receive two copies of it". A group a reader still
wants, once an existing subscription names it in SUBSCRIBE_DROP, is recovered
by a new SUBSCRIBE covering only that group with no max delay, never on a
guess, a timeout, or while the group may still arrive. This is lite-07's
replacement for a FETCH of a dropped group wider than one group's slice, and
with the fill budget decided 2026-11-03 it also recovers what the live
budget drops. Tests: a
group still in flight on the existing subscription sends no second SUBSCRIBE,
and a dropped group is recovered exactly once.

### JavaScript names the explicit end

Moved here from JS ranges on 2026-11-03 (maintainer, on
[moq-dev/moq#7696](https://github.com/moq-dev/moq/pull/7696)), which finished
that quest: the SUBSCRIBE_FINISH JS work belongs with
[moq-dev/moq#7675](https://github.com/moq-dev/moq/pull/7675), which brought the
JS lite-07 publisher and subscriber to the explicit end. What it left: rename
SUBSCRIBE_END to SUBSCRIBE_FINISH in `js/net`, as Rust and the draft did, and
delete the JS subscriber's covered-FIN path (`#parkCovered`, `#demandCovered`,
the `short` reopen), which no lite-07 publisher triggers any more.

### Draft and checks

Update `drafts/draft-lcurley-moq-lite.md` with each wire change here,
including that a subscriber fills missing groups through its ranges, and
recovers a dropped group with a one-group SUBSCRIBE. Run
`just drafts check` and `just test interop --all`, and the `moq-net-tests`
`restart::retired_fetch_lite07` and `moq-relay` suites.

## Related

- [Lite-07 FETCH is single-group only](/quest/m1/subscribe-ranges/lite07-fetch.md) - lite-07 FETCH reads one group or a slice of one, once this and JS ranges land
