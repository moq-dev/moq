# [L] Drop set

## Goal

A hostile upstream fragmenting drops costs what the cache's byte pool would
hold, never more: the drop set's per-track runs and each cursor's
disposition record are billed to the pool at a fixed cost per run, in Rust
and `js/net`, and a cursor's CPU per drop tracks the runs it touches, never
their span or the set. Eviction keeps every promise a reader was made: a
final run folds into the cursor's floor, and losing any other ends the
subscription with `TooFarBehind`.

## Plan

[moq-dev/moq#7670](https://github.com/moq-dev/moq/pull/7670) landed the
behavior: lite-07 SUBSCRIBE_SKIP and SUBSCRIBE_DROP with `Update`, a per-run
reason (`Hole` in `rs/moq-net/src/model/track/hole.rs`,
`Hole` in `js/net/src/internal.ts`), relays settling a received stale drop
only for readers it covers (`Drift::settles`, `settles` in
`js/net/src/track.ts`) and forgetting stale runs when they send new terms
(`supersede_stale`), and every lite-07 update re-offering what was reported
stale (`reoffer_stale`). It kept today's storage: `TrackState::drops`,
`DropCursor` with its `reported` and `reported_stale` sets, `Skipped`,
`Delivered`, `TrackRun::served`, and the JS `drops` signal, `#pending`,
`#reported`, and `#reportedStale`, none billed to anything. This quest is
what remains: bounding them.

Decided 2026-11-03 (maintainer, /quest-unblock): `js/net` bounds run memory
by porting Rust's per-broadcast `cache::Pool` to JS. One shared byte account
per broadcast bills frames and drop-set runs (a fixed cost per run) under
Rust's default budget, with eviction and the fold-into-floor or
`TooFarBehind` behavior below, so Rust and JS mirror each other with no new
setting. Reason: the 2026-11-02 audit found the existing 32 MiB limit in
`js/net/src/group.ts` bounds only one group's frame tail, and a mock-time
probe retained 64, 256, and 1,024 disjoint OLD runs with zero cached groups;
a shared pool bounds drop-only tracks the same way in both languages. Not
chosen: a per-track run cap; upstream backpressure. The pool port itself is
[JS cache pool](/quest/m1/js-cache-pool.md), split out on 2026-11-03 since it
lands on its own; this quest bills the runs to it.

Decided 2026-11-03 (maintainer, /quest-unblock), renamed and narrowed by the
`/quest-complete` decisions below: range terms carry an
explicit update id. Each lite-07 SUBSCRIBE and SUBSCRIBE_UPDATE carries an
increasing range-terms id, and a stale drop echoes the id it was judged
under. A relay treats a drop whose id has been superseded as unknown and
refills it by range, keeping no term history. Reason: a drop sent under an
old broad range can arrive after SUBSCRIBE_UPDATE narrows the aggregate at
the same budget (mock-time cursors over groups at 0, 1, and 2 seconds serve
group 2 under the open range and group 0 under a range capped before group
1), and `Max Age` alone cannot tell the two verdicts apart. Not chosen: a
conservative refill with bounded remembered ranges; treating every range
update as invalidating in-flight drops.

Decided 2026-11-03 (maintainer, /quest-unblock), with `Max Age` dropped,
`Terms` renamed `Update`, and SKIP narrowed to declared sequences by the
`/quest-complete` decisions below: lite-07
splits its hole message by whether the group ever existed. `SUBSCRIBE_SKIP {Group Start,
Group End}` names sequences the publisher never produced, such as skipped
sequence numbers or ones past a cut: it has no error code, ends no reader,
and is final. `SUBSCRIBE_DROP {Group Start, Group End, Error Code, Max Age,
Terms}` names groups that existed but will not be delivered: an aborted or
reset group, eviction or expiry with no copy left, and stale (OLD). It ends
any held reader with its code; a stale drop carries the `Max Age` and the
range-terms id it was judged under, so a widening can re-offer it. lite-06
and older keep today's SUBSCRIBE_DROP on the wire. The split runs through
moq-net's Rust and JS model and API, whose drop/skip naming follows it, and
through the draft. Reason: a hole that never existed has no reader to end
and no budget to qualify, so one message conflated a final, code-free
verdict with a reversible, reader-ending one. Not chosen: stale in SKIP; a
single message with a reason enum; renaming only.

Decided 2026-11-03 (maintainer, /quest-complete), changing the design
[moq-dev/moq#7670](https://github.com/moq-dev/moq/pull/7670) first shipped:

- **`Terms` is renamed `Update`** on lite-07 SUBSCRIBE, SUBSCRIBE_UPDATE and
  SUBSCRIBE_DROP: the number of the SUBSCRIBE or SUBSCRIBE_UPDATE in effect,
  in the Rust and JS code too.
- **SUBSCRIBE_DROP drops `Max Age`:** it is {Group Start, Group End, Error
  Code, Update}. The budget a stale (OLD) drop was judged under comes from
  the subscriber's own record of what it sent with that Update, and a drop
  whose Update is superseded is no verdict. Any SUBSCRIBE_UPDATE reopens every
  outstanding OLD drop in the subscriber's tail accounting; the publisher
  re-offers what the new terms cover and drops again, under the new Update,
  what is still stale. This replaces reopening only when the max delay widens
  past the drop's. A non-OLD drop is final, whatever its Update.
- **`track::Producer::drop_groups` is crate-private.** `skip_groups` stays
  public (see the next decision).

Decided 2026-11-03 (maintainer, /quest-complete), replacing the "eager SKIP"
decision above and the PR's `## Waiting` item on out-of-order creation:

- ✅ **SKIP comes only from an explicit, public `track::Producer::skip_groups`**,
  and from `append_group` (below). Reason: producers legitimately create
  groups out of order (`claim_sequence` re-creating an aborted sequence,
  moq-mux and moq-c `seek`, `finish()`'s documented "older groups can still
  arrive"), so a gap tells moq-net nothing; only the producer knows a
  sequence will never exist. moq-archive's replay is the in-tree consumer of
  `skip_groups`, for the sequences up to a recorded end it holds nothing
  for; a producer that numbers sparsely would call it too. moq-mux's live
  encoders use `append_group`. The relay records a received SKIP with
  `skip_groups` (below).
- ✅ **Both inferences are removed:** the eager skip of the gap in
  `create_group`, and the automatic SKIP at finish for never-arrived
  sequences below the final one (`TrackState::missed` and the publisher's
  `drop_unserved`). An undeclared gap is judged stale against the reader's
  budget, or named with DROP NOT_FOUND, as before this PR.
- ✅ **SKIP is final:** creating a group (or appending a group or datagram)
  at a skipped sequence is refused with `Error::Skipped` (JS `Skipped`).
  Re-creating a dropped or aborted sequence still works.
- ✅ **SKIP replaces every drop, final or stale** (maintainer, 2026-11-03,
  `/quest-complete`, replacing the 2026-11-04 `/quest-iterate` rule that a
  SKIP replaced only a stale OLD drop). "Never produced" is the most
  authoritative fact about a sequence, so a received SKIP, `skip_groups`, or
  `append_group`'s implicit skip overwrites an earlier DROP of any code (OLD,
  aborted, evicted, NOT_FOUND), and a later DROP leaves a skip in place.
  Readers that drop already ended stay ended, and nothing is re-delivered; the
  run's reason becomes Skip, so every later reader, relay and downstream
  report sees SKIP, and a relay forwards the SKIP even over a DROP it already
  forwarded (`DropCursor::reported_skip`, `TrackRun::dropped`, and their JS
  mirrors). A cached group is never overwritten: a received SKIP naming one
  leaves the group, logs a protocol error, and keeps the session, since the
  draft requires no more. A received SKIP or final DROP also takes its groups
  out of the subscriber's stale record, so the next update doesn't owe them
  again. Not chosen: SKIP replacing only a stale OLD drop.
- ✅ **A group or final drop replaces a lite-07 stale drop already sent**
  (agent decision from review, 2026-11-04, `/quest-iterate`; the maintainer
  chose to keep it and retire the stale report at the 2026-11-03 round-4
  checkpoint, `/quest-complete`). A front that switches routes can hand out
  the new route's group, or its final verdict, for a sequence the old route
  reported stale, before the update that reopens it, so the publisher serves
  the group, or sends the final drop, over the OLD it sent. Its own expired
  streams' OLD drops count the same. Whatever replaces a stale report
  retires it: a group handed out, a SKIP, or a final drop takes the sequence
  out of the reader's stale record (`DropCursor::reported_stale`, the
  resumed `Reader::reported_stale`, JS `#reportedStale`) and the publisher's
  (`TrackRun::dropped_stale`, `expired`), so the next SUBSCRIBE_UPDATE
  neither sends OLD for it again nor names it at the end. On the receiving
  side a Group Stream wins over a stale drop of its sequence in either order:
  an OLD that lands after the stream leaves the group, still arriving or
  not, and an update owes again only the stale runs the track still holds.
  The end sweep's NOT_FOUND never replaces a stale drop, so an update can
  still reach an ended track's history. Not chosen: reverting the
  replacement; merging with the stale report left in place.
- ✅ **`append_group` is an implicit skip** (refinement, same session): it
  means "the next group", so the producer declares it won't go back, and it
  skips every sequence below it that never arrived and wasn't already given
  up, as if `skip_groups` were called on them; those are then final. Today
  `append_group` leaves no gap of its own, so this only fires on holes earlier
  `create_group` calls left below the head; under time addressing it names
  the span between consecutive starts. `create_group` stays non-committal,
  so out-of-order creation (`claim_sequence` retries, `seek`) keeps working.
  There is still no inference at finish.
- ✅ **`skip_groups` stays public** and is documented as the explicit source
  of SKIP; `drop_groups` stays crate-private. Rust and JS mirror each other
  (`skipGroups`, `appendGroup`).
- ✅ **Draft:** SKIP names only sequences the publisher declared it will never
  produce, and it is final.
- ✅ **Run-length tracking** (refinement, same session): skipped and dropped
  groups are kept as runs, never one entry per sequence, in Rust and JS: the
  track's hole set, the implicit `append_group` skip, `skip_groups`, and what
  the publisher reports. A huge `skip_groups` or append gap costs one run and
  one SUBSCRIBE_SKIP (`a_huge_skip_costs_one_run`,
  `lite07_skips_a_huge_run_at_once`, and their JS mirrors).
- ✅ **`create_group` refuses a skipped sequence and allows a dropped one**
  (refinement, same session): a group stream wins over its drop, which fills
  answering a miss and `claim_sequence` retries of an aborted sequence need.
- ✅ **No `create_group` gap error, for now** (maintainer, same session):
  gaps stay legal, and `append_group` or `skip_groups` declares them. No
  principled bound exists in the model: the cache and the budget bound bytes
  and time, not sequence counts, a gap costs one run whatever its length,
  and under time addressing every group leaves a gap between starts. Not
  chosen: a debug log on a gap; a configurable size limit (an arbitrary
  knob).
- Not chosen: eager skip on `create_group` with a late group winning over
  it (the shipped design, which inferred SKIP from a gap that out-of-order
  creation makes meaningless); refusing every `create_group` below the last
  group (broke `claim_sequence`, `seek` and 85 moq-net tests); keeping the
  finish-time inference for a local producer (a gap at finish is no more a
  declaration than one mid-stream).

Decided 2026-10-27 (maintainer): this runs after the range count cost work,
which finished in [moq-dev/moq#7542](https://github.com/moq-dev/moq/pull/7542),
and before [Rust groups by start](/quest/m1/time-addressing/model.md), which
requires it because runs keyed by group sequence convert mechanically to
starts.

- **One disposition record per cursor,** a run-length map whose runs are
  delivered, skipped as stale, or reported dropped or skipped, with the
  reason and, for a stale run, the budget it was judged under. It replaces
  `Skipped` (`rs/moq-net/src/model/skipped.rs`), `Delivered`
  (`rs/moq-net/src/model/delivered.rs`, whose floor folds stale gaps in with
  delivered groups), `DropCursor`'s `reported` and `reported_stale`,
  `TrackRun::served` in `rs/moq-net/src/lite/publisher.rs`, and the JS
  wire cursor's `#reported` and `#reportedStale`, `#skipped`, and the
  publisher's `served`. Delivery is recorded, not inferred from the cursor
  position, since an arrival-order or descending cursor delivers out of
  sequence order.
- **Billed to the pool, never guessed.** Each track run and each cursor run
  is billed through a `Charge` at a fixed per-run cost (in JS, through the
  [JS cache pool](/quest/m1/js-cache-pool.md)). Under pressure the pool
  evicts the oldest track runs as it evicts groups, and a run older than the
  cache's retention window is forgotten. An evicted track run's sequences are
  unknown again: a reader judges them by `drifted` or fills them by range.
  A relay's own eviction is never a run.
- **The floor rises only over a final prefix:** the runs from the floor up to
  the lowest run that is not final. A final run is a delivered one, one
  skipped at the track's max age bound, a skip, a budget-independent drop,
  or any drop reported over lite-05 or lite-06. A lite-07 stale report within
  reach of the max age bound, or an unknown hole, is not: when the pool must
  evict it or a run above it, the cursor fails with `TooFarBehind` rather
  than forgo the re-offer. A drop folded into the floor is final for that
  cursor, so its group arriving late is not delivered there.
- **Per-reader misses** fold into the cursor's runs too: the given-up misses
  `rs/moq-net/src/model/track/miss.rs` keeps per gap (from
  [moq-dev/moq#6171](https://github.com/moq-dev/moq/pull/6171), decided
  2026-10-23).
- **CPU per drop.** Every SUBSCRIBE_DROP bumps `drops_revision`, and each
  reader's `DropCursor::next` rescans the overlapping runs, so N fragmented
  runs and R readers cost O(R*N) per 6-byte drop; JS `insertDrop`
  (`js/net/src/internal.ts`) copies the whole run array once for the track
  and once per sink. Keep the per-cursor walk incremental in both languages.
  Extend `track_subscriber_churn` in `rs/moq-net/benches/track.rs` with a
  fragmented set up to pool capacity, swept over subscriber count, shared
  runs, runs per cursor record, and run span.

Still to cover, beyond the memory work:

- **Idle START unreport test.** [moq-dev/moq#6717](https://github.com/moq-dev/moq/pull/6717)
  made the JS lite publisher unreport stale skips below an idle
  SUBSCRIBE_START, without a test: the case needs an update between START
  and the first group. Cover it deterministically through the track hooks.
- **Cancelled fetches hold the end.** Found 2026-10-28 by
  [moq-dev/moq#7149](https://github.com/moq-dev/moq/pull/7149): a cancelled
  fetch below a track's end holds the end open until the cache expiry scan
  removes it. Settle it with a run reason.
- **Untimed fills below a resolved start.** Noted 2026-10-31 from
  [moq-dev/moq#7235](https://github.com/moq-dev/moq/pull/7235): a lite-07
  copy with no media time asks a fill below the start even for a reader whose
  budget the publisher already exceeded. If the extra FETCH shows up in relay
  traffic, bound it with a run below the start carrying its budget.
- **A fill's stale verdict is final for everyone.** `TrackState::drop_unfed`
  (`rs/moq-net/src/model/track/fill.rs`) records what a fill's source gave up
  as a budget-independent drop, including OLD judged under that fill's own
  terms. Record it as a stale run, so it settles only the readers it covers.

Tests, on mock time through a relay, in Rust and mirrored in `js/net`:

- After a widening from B1 to B2 for a group the upstream no longer holds, an
  in-flight drop under the B1 update settles no reader once the B2 update is
  sent, and the re-drop under the B2 update settles the B2 reader, over the
  wire.
- A group a relay evicted while the upstream still holds it is never reported
  dropped: a new, a widened, and an owed reader each fill it by range.
- Rust and `js/net` encode and decode an origin's eviction and retention
  expiry drop (EVICTED, not OLD) across languages; a relay forwards them
  unchanged, and no update re-offers either.
- Through a lite-06 hop, a reported group stays dropped for every reader,
  with no session error; a shrink sends no drops.
- Under pool pressure, delivered runs and budget-independent drops fold into
  the floor, while evicting a lite-07 stale report within reach, or a run
  above it, ends the cursor with `TooFarBehind`.
- A flood of fragmented runs, including a JS drop-only flood with no cached
  groups, stays within the pool, evicting the oldest shared runs first, and
  a sequence whose run was evicted is judged locally or filled by range.
- A single run spanning billions of sequences is settled by each cursor in
  one step, whether dropped, skipped by a wider reader, or evicted and judged
  again.

Public API: none. Wire: none.

## Required

- [JS cache pool](/quest/m1/js-cache-pool.md) - the byte account the JS runs bill

## Related

- [Subscribe ranges](/quest/m1/subscribe-ranges/README.md) - fills the cache miss an evicted run leaves
