# [L] Drop set

## Goal

Every cursor judges a missing group against its own budget. Today a dropped
group is final for every reader of a track: a narrow reader can wait
indefinitely for an upstream OLD naming a group the relay never cached,
and a wider reader never gets a group an upstream already reported stale.
The track state instead keeps the dropped and expired runs with their
reason and budget, so a narrow reader gets a prompt drop, a wide reader
waits, and a widening re-offers what its new budget covers. On lite-07 this
restores "growing means I want it": a group reported dropped under a
narrower budget is served after a widening, as a cold relay would.

## Plan

Decided 2026-10-27 (maintainer): this runs after
[Linear fill path](/quest/m1/subscribe-ranges/fill-linear.md), since both edit
`given_up` in `rs/moq-net/src/model/track/miss.rs`, and before
[Rust groups by start](/quest/m1/time-addressing/model.md), which requires it
because runs keyed by group sequence convert mechanically to starts.

Decided by the maintainer on 2026-10-19, splitting this from the widened
max age re-offer
([moq-dev/moq#6717](https://github.com/moq-dev/moq/pull/6717)), which keeps
drops final for now. That PR re-offers cached groups a cursor skipped as
stale, in the subscription's order (newest first unless ascending; `js/net`
lowest first, since its buffer is ascending), through the
[moq-dev/moq#5929](https://github.com/moq-dev/moq/pull/5929) owed path. Its
skipped record (`Skipped` in `rs/moq-net/src/model/skipped.rs`, `#skipped`
in `js/net/src/track.ts`) keeps a run only while the cache holds one of its
groups. On 2026-10-25 the maintainer deferred the full pool-billed
disposition record to this quest.

- **One run-length set per track.** The track state keeps the dropped and
  expired group runs within the subscription range. Each run carries its
  reason (stale, cut, never produced, or evicted or expired where no copy
  remains: received as such from the upstream, or at an origin with no
  upstream) and, for a stale run, the
  budget it was judged under. A group that arrives removes its sequence; a
  drop never removes a group that already arrived. A relay's own eviction
  is never a run: the group is unknown again, and any cursor that wants it,
  new, widened, or already owed it, fills it from upstream by range, the
  existing miss fill. That needs no new state, since the cursor's record
  already holds the span as unknown, a run at a time.
- **Each cursor judges for itself, a run at a time.** A cursor walks the
  set's runs in its range, never their sequences, and settles each whole run
  in O(1):
  - A run whose reason does not depend on the budget (a cut, a group never
    produced, an upstream or origin eviction or expiry) is dropped for every reader
    as one range, as today.
  - A received stale run is dropped promptly as one range for a reader
    whose budget is no wider than the one it was judged under and whose
    range covers this relay's aggregate range for the track, which the
    upstream judged under. A verdict depends on the range as well as the
    budget, since `drifted` caps the edge to the reader's range
    (`Edge::cap`), so a narrower reader treats the run as unknown and fills
    it by range, where the upstream judges it under that narrower range.
    The cost is an extra range request, only for narrower readers, when a
    stale drop arrives.
  - A missing run this relay judges itself is dropped for a reader under
    whose own budget and range `TrackState::drifted` convicts it. `drifted`
    judges a missing sequence by its successor here, and every sequence in
    a missing run shares one successor, so one call settles the run.
  - A wider reader skips the run and waits for a group, a re-offer, or its
    own verdict, each of which splits the run at most once.
  - An unknown span (a gap between runs, or an evicted run) is judged the
    same way, as one unit bounded by its neighbours: by one `drifted` call
    against its shared successor, or by one range miss fill upstream.

  A hostile upstream's huge run costs one Charge and O(1) per cursor, and
  splitting it costs per-run Charges the pool bounds, so CPU tracks pool
  bytes, never span.

  This fixes the gap Codex found on
  [moq-dev/moq#5986](https://github.com/moq-dev/moq/pull/5986): today
  `TrackRun::poll_forward_drop` emits only recorded drops, cuts, or stale
  cached groups. A sequence the relay never cached is not judged until the
  track finishes.
- **Each cursor keeps a disposition record,** next to the per-track set: a
  group that arrives leaves the shared set, so only the cursor's record
  remembers that it reported cached G stale. The record is one run-length
  map per cursor, each run marked delivered, skipped as stale, or reported
  in SUBSCRIBE_DROP, with its reason and, for a stale run, the budget and
  range it was judged under. It replaces the skipped record, `Delivered`
  (`rs/moq-net/src/model/delivered.rs`), whose floor folds stale gaps in
  with delivered groups, `TrackRun::served` in
  `rs/moq-net/src/lite/publisher.rs`, and the JS wire cursor's `#reported`.
  Delivery is recorded, not inferred from the cursor position, since an
  arrival-order or descending cursor delivers out of sequence order. On
  lite-07 a reported stale run is no longer final: like a skipped one, it
  is re-offered when a widening covers it.
- **The record is bounded by the pool, never by guessing.** Each run is
  billed to the cache's `Pool` through a `Charge` at a fixed per-run cost
  and is settled a run at a time, so a cursor's state and CPU track its
  runs, never their span. The floor only rises over a final prefix: the
  runs from the floor up to the lowest run that is not final. An unknown
  hole is never final: a descending cursor can deliver H while a lower G
  is still unknown. When the pool evicts a run inside the final prefix, it
  folds into the floor; any other run cannot go without guessing, so the
  subscription ends with `TooFarBehind` and the subscriber re-subscribes.
- **A widening re-offers the dropped runs now inside its budget,** through
  the same re-offer path as skipped runs, in the subscription's order.
  Upstream, the relay's own widening asks for them again.
- **Only a widening reverses a stale verdict,** local or received alike: no
  run tracks the group that convicted it, and an abort or eviction of that
  group leaves the run standing. So warm/cold equivalence after an edge
  loss is not promised: when the newer group that made G stale is aborted
  or evicted, a cold cursor may serve G while a warm one stays past it.
  Watching for it would need both groups a verdict depends on (`drifted`
  uses G's successor and a possibly later presentation edge), and a run
  skipped at the max age bound would no longer be final. A received run is
  also reversed by the upstream re-offering its group after a widening,
  since a later group wins over its drop. Cuts and never-produced runs are
  unaffected.
- **A stale drop names its budget.** A SUBSCRIBE_UPDATE has no
  acknowledgement, so after a widening from B1 to B2, an in-flight B1 drop
  and the B2 re-drop look identical, and no guess settles a B2 reader. On
  lite-07 SUBSCRIBE_DROP carries `Max Age`: the budget a staleness verdict
  was judged under, encoded like `Publisher Max Age` (milliseconds plus one),
  with `0` for a drop that does not depend on the budget: a cut, a group
  never produced, or a group the sender evicted or expired with no copy it
  could fetch (an origin, or a relay forwarding its upstream's eviction
  drop), none of which depends on the reader's budget. A relay never sends
  an eviction drop for its own eviction; it fills the group by range
  instead. A widening never reverses a `0` drop. The relay records each run with that
  exact budget. A B1
  drop then settles only readers no wider than B1, and the B2 re-drop
  settles a B2 reader. The cost is one varint per drop, a few bytes on an
  event that is already per group, which is worth an unambiguous term.
  Rejected: an implicit generation counted by both sides per
  SUBSCRIBE_UPDATE. It needs a generation-to-budget history at the relay
  and agreement on every update counted, while the budget itself is
  self-describing, order-free, and unaffected by updates that change
  nothing.
- **lite-07 only for re-offering reported groups.** Released lite-05 and
  lite-06 peers treat a flushed SUBSCRIBE_DROP as final, so a drop received
  over those versions is final for every reader, and a lite-07 hop never
  sends a group onto a lite-05 or lite-06 hop after reporting it. On
  lite-07, a widening obliges the publisher to serve the newly covered groups
  it holds and never delivered on this subscription, including ones it
  reported dropped, and to drop again, under the new terms, the ones it
  cannot serve. SUBSCRIBE_UPDATE stays incremental and never resets, since a
  reset races in-flight, out-of-order data. A shrink sends no drops.
- **Draft wording.** `drafts/draft-lcurley-moq-lite.md` states, for lite-07's
  restored SUBSCRIBE_DROP, that a drop holds only under the terms in effect
  when it was sent, that a widening re-offers held groups and re-drops the
  rest, and that a later group, when sent, wins over its drop. This quest
  writes it, including the `Max Age` field above, which is free to add
  because lite-07 is unpublished.

Code it replaces, in Rust and `js/net` alike:

- `TrackState::drop_groups`, the `drops` range set and `DropCursor` in
  `rs/moq-net/src/model/track.rs`, which record a whole range without its
  reason or budget, even over groups already cached.
- In `js/net/src/track.ts`: `#drop`, `#undrop`, the `drops` signal, and
  the wire cursor's `#pending` and `#dropsSeen`, whose `#tryRecvGroup`
  closes any group already reported.

Untrusted input: a hostile upstream can fragment the set by alternating
drops and arrivals, and an open live range is no bound, since its groups are
evicted while their runs would stay. Bill each run to the cache's existing
byte accounting like a group: the Rust `Pool` through a `Charge` at a fixed
per-run cost. Under pressure the pool evicts the oldest runs as it evicts
groups, and a run older than the cache's retention window is forgotten, so
the set never outgrows what the pool would hold, with no new constant. An
evicted run's sequences become unknown again. `js/net` has no byte pool and
gets no new cap: its shared and per-cursor runs expire with the same idle
window as the groups they sit between, and expiry applies the same
fold-or-`TooFarBehind` rule as a Rust eviction. A run sits between cached
sequences, so there are at most about one per cached sequence (alternating
drop and arrival), and JS run state grows no faster than the JS cache
itself. The JS byte bound for both is
[JS unbounded groups](/quest/m1/unbounded-groups/js.md).

Only a final run folds into a cursor's floor: a delivered run, a run
skipped at the track's max age bound (no widening reaches past it), or a
budget-independent drop (a cut, a group never produced, an upstream or
origin eviction or expiry, or any drop reported over lite-05 or lite-06). A stale run reported on lite-07 within reach of the
max age bound is budget-qualified, so it is treated like an unknown hole:
the floor never rises over it, and if the pool must evict it or a run above
it, the cursor fails with `TooFarBehind`. Folding it would forgo the
re-offer this plan promises after a widening. A later group wins over its
drop only while the cursor's record still holds that drop: a drop folded
into the floor under pool pressure is final for that cursor, so its group
arriving late is not delivered there.

A cursor judges an unknown missing sequence (never a stale run its record
still holds as reported, which only a widening settles) as it would a
group never received: locally against its own budget (`TrackState::drifted`),
and otherwise as a cache miss, which the relay fills upstream by range
([Subscribe ranges](/quest/m1/subscribe-ranges/README.md)), so no reader
waits indefinitely on forgotten state. Extend `track_subscriber_churn` in
`rs/moq-net/benches/track.rs` with a fragmented set up to pool capacity,
swept over subscriber count, shared runs, runs per cursor record, and run
span, so enumeration cost tracks the touched runs, never the set, the
record, or a run's span.

Test on mock time through a relay:
- On a live track, a reader no wider than the upstream budget gets a prompt
  drop for an upstream OLD naming a group the relay never cached.
- On lite-07, a widened reader gets that group re-offered. A drop already
  flushed before the widening SUBSCRIBE_UPDATE was received, so the reader
  then gets the re-offer and no further final drop; a drop still in flight
  does not settle the widened reader. Check what each reader receives, not
  only tail accounting.
- On lite-07, after a widening from B1 to B2 for a group the upstream no
  longer holds, an in-flight drop tagged B1 settles only the B1 reader, and
  the re-drop tagged B2 settles the B2 reader.
- A group arriving after its drop, while the cursor's record still holds
  the drop, is served, and a drop arriving after its group leaves it
  served.
- Through a lite-06 hop, a reported group stays dropped for every reader,
  with no session error.
- A shrink sends no drops.
- Rust and `js/net` each encode and decode an origin's eviction drop and
  retention-expiry drop with `Max Age` `0`, across languages; a relay
  forwards them unchanged, and a widening re-offers neither.
- A group a relay evicted while the upstream still holds it is never
  reported dropped: a new reader, a widened reader, and a reader already
  owed it each fill it by range and receive it.
- A reader whose range is narrower than the relay's aggregate range is not
  settled by a received stale drop: it fills G by range and gets G or a
  drop judged under its own range, while a reader covering the aggregate
  range is settled at once.
- On lite-07, a cursor that delivered F, reported its neighbour G stale
  while G stayed cached, and delivered H, gets only G after a widening that
  covers all three: G is re-offered and F and H are never resent.
- Under pool pressure, a cursor's delivered runs and budget-independent
  drops fold into the floor, while evicting a lite-07 stale-reported run
  within reach, or a run above it, ends the cursor with `TooFarBehind`
  instead of losing the re-offer. Through a lite-06 hop the reported run
  folds.
- A flood of fragmented runs stays within the pool in Rust, evicting the
  oldest shared runs first, and a sequence whose shared run was evicted is
  judged locally or filled by range, never waited on indefinitely. In
  `js/net` the same flood holds at most one run more than its cached
  sequences, and the runs expire with the idle window.
- A single run spanning billions of sequences is settled by each cursor in
  one step, whether dropped, skipped by a wider reader, or evicted and
  judged again.
- Per-reader misses are bounded the same way. [Dynamic range
  requests](https://github.com/moq-dev/moq/pull/6171) adds a third unbounded
  record: each reader's given-up misses, one per gap, which a reader whose
  floor never moves cannot forget. Fold them into the cursor's runs, billed
  to the pool (from
  [moq-dev/moq#6171](https://github.com/moq-dev/moq/pull/6171), decided
  2026-10-23).

- A group the publisher cut while a fetched copy still serves its end is
  delivered and never also reported dropped. Today `DropCursor::cut`
  (`rs/moq-net/src/model/track.rs`) reports any visible slot's
  `cut_error()`, so the arrival walk can hand out the fetched copy and report
  the group dropped too; `Slot::cut()` already returns nothing while a copy
  serves the end. Found by
  [moq-dev/moq#7053](https://github.com/moq-dev/moq/pull/7053) (added
  2026-10-28).

Mirror each case in `js/net`.

Public API: none. Wire: lite-07 SUBSCRIBE_DROP (unpublished) gains `Max Age`
and the widening and ordering rules above; published versions are
unchanged.
- A local producer's declared end must settle over dropped groups, as the
  doc on `TrackState::drop_groups` already claims (added 2026-10-25): let
  `reaches(fin)` count a live edge whose gap `edge..fin` is wholly dropped,
  then delete moq-archive's fake empty group at `fin - 1`
  (`rs/moq-archive/src/consumer/track.rs`).

### CPU per drop (2026-10-24 pre-cut audit)

Besides memory, run fragmentation costs CPU today, new on main: every
SUBSCRIBE_DROP bumps `drops_revision`, and each reader's `DropCursor::next`
rescans the overlapping runs (`rs/moq-net/src/model/track.rs` about lines
5802-5870), so N fragmented runs and R downstream readers cost O(R*N) per
6-byte DROP. JS `insertDrop` (`js/net/src/internal.ts:136`) copies the whole
run array once for the track and once per sink (`track.ts:914`). The
replacement keeps the per-cursor walk incremental in both languages, with a
benchmark swept over runs and readers.

### Idle START unreport test

[moq-dev/moq#6717](https://github.com/moq-dev/moq/pull/6717) made the JS lite
publisher unreport stale skips below an idle SUBSCRIBE_START, so a later
widening can re-offer them as Rust does, but landed it without a test: the
case needs an update to arrive between START and the first group. Cover it
deterministically, through the track hooks rather than wire timing, when the
drop runs are reworked here.

### A covered cut is still reported

`DropCursor::cut` reports a group as cut even when a fetched copy covers it,
noted in [moq-dev/moq#7053](https://github.com/moq-dev/moq/pull/7053) and again
by [moq-dev/moq#7149](https://github.com/moq-dev/moq/pull/7149), which made a
fetched copy settle such a group. Decided 2026-10-29 (maintainer): settle it
here, where a cut run gets a reason and a covering copy can clear it.

### Cancelled fetches hold the end

Found 2026-10-28 by [moq-dev/moq#7149](https://github.com/moq-dev/moq/pull/7149):
a cancelled fetch below a track's end still holds the end open until the cache
expiry scan removes it, so how fast a subscription ends depends on expiry
timing. Settle it here, where cancelled and expired runs get a reason.

## Related

- [Subscribe ranges](/quest/m1/subscribe-ranges/README.md) - fills the cache miss an evicted run leaves
- [JS unbounded groups](/quest/m1/unbounded-groups/js.md) - the JS byte bound for groups and the runs between them; not required, since JS runs never outgrow the JS cache
