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

Decided by the maintainer on 2026-10-19, splitting this from
[A widened max age re-offers cached groups](/quest/m1/resolved-demand.md),
which keeps drops final for now.

- **One run-length set per track.** The track state keeps the dropped and
  expired group runs within the subscription range. Each run carries its
  reason (stale, cut, never produced, evicted) and, for a stale run, the
  budget it was judged under. A group that arrives removes its sequence; a
  drop never removes a group that already arrived.
- **Each cursor judges for itself, a run at a time.** A cursor walks the
  set's runs in its range, never their sequences, and settles each whole run
  in O(1):
  - A run whose reason does not depend on the budget (a cut, a group never
    produced) is dropped for every reader as one range, as today.
  - A stale run is dropped promptly as one range for a reader whose budget
    is no wider than the one it was judged under, and for any reader under
    whose own budget `TrackState::drifted` convicts it. `drifted` judges a
    missing sequence by its successor here, and every sequence in a missing
    run shares one successor, so one call settles the run.
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
- **A widening re-offers the dropped runs now inside its budget,** through
  the re-offer path the resolved-demand quest adds, in the subscription's
  order. Upstream, the relay's own widening asks for them again.
- **A local stale run that loses its edge is invalidated** as one span, the
  same as a widening invalidates it, matching resolved-demand's stale-edge
  trigger. A run this hop judged stale records the newer group here that
  convicted it. When that group is aborted or evicted, the run's sequences
  become unknown: a cached group is re-offered, and a missing one is judged
  again against the current edge (`TrackState::drifted`) or waited for.
- **A received stale run records no edge.** Its groups are not held here,
  so a cold cursor at this relay would fill them from upstream by range, and
  the upstream, still holding its edge, would drop them again. A local abort
  or eviction therefore never makes a received verdict obsolete; only the
  upstream changing its mind does, and it says so: on lite-07 it re-offers
  the group when its own edge is lost, and a widening re-asks. A received
  run is invalidated only by an upstream re-offer or a widening. Cuts and
  never-produced runs carry no edge and are unaffected. No wire field.
- **A stale drop names its budget.** A SUBSCRIBE_UPDATE has no
  acknowledgement, so after a widening from B1 to B2, an in-flight B1 drop
  and the B2 re-drop look identical, and no guess settles a B2 reader. On
  lite-07 SUBSCRIBE_DROP carries `Max Age`: the budget a staleness verdict
  was judged under, encoded like `Publisher Max Age` (milliseconds plus one),
  with `0` for a drop that does not depend on the budget (a cut, a group
  never produced). The relay records each run with that exact budget. A B1
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
  rest, and that a later group wins over its drop. Whichever of this and
  [SUBSCRIBE_DROP accounts for every group](/quest/m1/subscribe-drop.md)
  lands second writes it, including the `Max Age` field above, which is
  free to add because lite-07 is unpublished.

Code it replaces, in Rust and `js/net` alike:

- `TrackState::drop_groups`, the `drops` range set and `DropCursor` in
  `rs/moq-net/src/model/track.rs`, which record a whole range without its
  reason or budget, even over groups already cached.
- `TrackRun::served` in `rs/moq-net/src/lite/publisher.rs`, which holds
  served and dropped sequences in one set and refuses any group in it.
- In `js/net/src/track.ts`: `#drop`, `#undrop`, the `drops` signal, and the
  wire cursor's `#pending`, `#dropsSeen`, and `#reported`, whose
  `#tryRecvGroup` closes any group already reported.

Untrusted input: a hostile upstream can fragment the set by alternating
drops and arrivals, and an open live range is no bound, since its groups are
evicted while their runs would stay. Bill each run to the cache's existing
byte accounting like a group: the Rust `Pool` through a `Charge` at a fixed
per-run cost, and the same eviction bound in `js/net`. Under pressure the
pool evicts the oldest runs as it evicts groups, and a run older than the
cache's retention window is forgotten, so the set never outgrows what the
pool would hold, with no new constant. An evicted run's sequences become
unknown again. A cursor judges an unknown missing sequence as it would a
group never received: locally against its own budget (`TrackState::drifted`),
and otherwise as a cache miss, which the relay fills upstream by range
([Subscribe ranges](/quest/m1/subscribe-ranges/README.md)), so no reader
waits indefinitely on forgotten state. Extend `track_subscriber_churn` in
`rs/moq-net/benches/track.rs` with a fragmented set up to pool capacity,
swept over subscriber count, run count, and run span, so enumeration cost
tracks the touched runs, never the set or a run's span.

Test on mock time through a relay:
- On a live track, a reader no wider than the upstream budget gets a prompt
  drop for an upstream OLD naming a group the relay never cached.
- On lite-07, a widened reader gets that group re-offered and never the
  drop, whether the drop was flushed before or after the widening
  SUBSCRIBE_UPDATE. Check what each reader receives, not only tail
  accounting.
- On lite-07, after a widening from B1 to B2 for a group the upstream no
  longer holds, an in-flight drop tagged B1 settles only the B1 reader, and
  the re-drop tagged B2 settles the B2 reader.
- A group arriving after its drop is served, and a drop arriving after its
  group leaves it served.
- At the same budget B, for a group G judged stale because of a newer
  edge E: when the relay judged G itself and E is aborted or evicted there,
  a B reader gets G re-offered from the cache. When G was reported stale by
  the upstream and E is lost upstream, the upstream re-offers G, which wins
  over the stored drop. When E is evicted only at the relay, the received
  drop stands, as a cold relay filling G from upstream would also be told.
- Through a lite-06 hop, a reported group stays dropped for every reader,
  with no session error.
- A shrink sends no drops.
- A flood of fragmented runs stays within the pool, evicting the oldest
  runs first, and a sequence whose run was evicted is judged locally or
  filled by range, never waited on indefinitely.
- A single run spanning billions of sequences is settled by each cursor in
  one step, whether dropped, skipped by a wider reader, or evicted and
  judged again.

Mirror each case in `js/net`.

Public API: none. Wire: lite-07 SUBSCRIBE_DROP (unpublished) gains `Max Age`
and the widening and ordering rules above; published versions are
unchanged.

## Required

- [A widened max age re-offers cached groups](/quest/m1/resolved-demand.md) - the re-offer path this extends to reported groups
- [SUBSCRIBE_DROP accounts for every group](/quest/m1/subscribe-drop.md) - restores the lite-07 drop whose wire and behavior this extends

## Related

- [Subscribe ranges](/quest/m1/subscribe-ranges/README.md) - fills the cache miss an evicted run leaves
