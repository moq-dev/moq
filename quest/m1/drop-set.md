# [L] Drop set

## Goal

A hostile upstream fragmenting drops costs what the cache's byte pool would
hold, never more: each cursor's disposition record is billed to the pool at a
fixed cost per run, in Rust and `js/net`, as the track's runs already are, and
a cursor's CPU per drop tracks the runs it touches, never their span or the
set. Eviction keeps every promise a reader was made: a final run folds into
the cursor's floor, and losing any other ends the subscription with
`TooFarBehind`.

## Plan

The track's drop set is billed and bounded, and every reader folds it in
incrementally ([moq-dev/moq#8294](https://github.com/moq-dev/moq/pull/8294)).
The per-cursor storage is not billed to anything: `DropCursor`'s `pending`
and its `reported`, `reported_stale`, `reported_received`, and
`reported_skip` sets, `Skipped`, `Delivered`, `TrackRun`'s `served`,
`dropped`, `dropped_stale`, and `expired`, the lite subscriber's record of
the group streams that finished since its last update (`Tail::finished`, JS
`Tail#finished`), and the JS wire cursor's `#pending`, `#skipped`,
`#reported`, `#reportedStale`, `#reportedReceived`, and `#reportedSkip`.
This quest bounds them, keeping the behavior
[moq-dev/moq#7670](https://github.com/moq-dev/moq/pull/7670) and
moq-dev/moq#8294 landed and the decisions recorded there.

Decided 2026-10-27 (maintainer): this runs before
[Rust groups by start](/quest/m2/time-addressing/model.md), which requires it
because runs keyed by group sequence convert mechanically to starts.

- **One disposition record per cursor,** a run-length map whose runs are
  delivered, skipped as stale, or reported dropped or skipped, with the
  reason and, for a stale run, the budget it was judged under. It replaces
  `Skipped` (`rs/moq-net/src/model/skipped.rs`), `Delivered`
  (`rs/moq-net/src/model/delivered.rs`, whose floor folds stale gaps in with
  delivered groups), `DropCursor`'s `reported*` sets, `TrackRun::served` in
  `rs/moq-net/src/lite/publisher.rs`, and their JS mirrors. Delivery is
  recorded, not inferred from the cursor position, since an arrival-order or
  descending cursor delivers out of sequence order.
- **Billed to the pool, never guessed.** Each cursor run is billed through a
  `Charge` at a fixed per-run cost (in JS, through the pool in
  `js/net/src/cache.ts`), as the track's runs are. A relay's own eviction is
  never a run.
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
- **CPU per drop.** Every report inserts into the per-cursor `RangeSet`s and
  sorted arrays, so `track_subscriber_churn_drops` in
  `rs/moq-net/benches/track.rs` slows once many readers each hold many
  reported runs (256 readers holding 65,536 each: about 1 to 4 ms per drop
  against 0.16 ms with one each). JS `#askFills` still walks every run above
  the edge on each probe while a fill handler serves the track; mirror Rust's
  `Misses::unknown` memo there.

Mirror each case in `js/net`.
In `js/net`, a group a sink buffers keeps its source wanted only while
`Subscriber.#reading` says its reader will still read it
([moq-dev/moq#7948](https://github.com/moq-dev/moq/pull/7948)), so a change to
how a cursor judges a missing or stale group keeps that verdict in step.

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
- A sequence whose shared run was shed or forgotten is judged locally or
  filled by range, through a relay (the flood bound itself is covered by
  `a_drop_flood_stays_within_the_pool` and its JS mirror).
- A single run spanning billions of sequences is settled by each cursor in
  one step, whether dropped, skipped by a wider reader, or evicted and judged
  again.

Public API: none. Wire: none.

## Related

- [Subscribe ranges](/quest/m2/subscribe-ranges/README.md) - fills the cache miss an evicted run leaves
