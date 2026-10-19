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
- **Each cursor judges for itself.** A cursor enumerates the missing
  sequences in its range from the set:
  - A run whose reason does not depend on the budget (a cut, a group never
    produced) is dropped for every reader, as today.
  - A stale run is dropped promptly for a reader whose budget is no wider
    than the one it was judged under, and for any reader under whose own
    budget `TrackState::drifted` convicts it.
  - A wider reader waits for the group, a re-offer, or its own verdict.

  This fixes the gap Codex found on
  [moq-dev/moq#5986](https://github.com/moq-dev/moq/pull/5986): today
  `TrackRun::poll_forward_drop` emits only recorded drops, cuts, or stale
  cached groups. A sequence the relay never cached is not judged until the
  track finishes.
- **A widening re-offers the dropped runs now inside its budget,** through
  the re-offer path the resolved-demand quest adds, in the subscription's
  order. Upstream, the relay's own widening asks for them again.
- **A received drop's budget.** A SUBSCRIBE_UPDATE has no acknowledgement,
  so a drop that arrives after a widening may have been judged under either
  budget. Record the narrowest budget sent upstream since the last widening.
  A guess that is too narrow only makes a wide reader wait for its own
  verdict, never drop early.
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
  lands second writes it. No new wire field.

Code it replaces, in Rust and `js/net` alike:

- `TrackState::drop_groups`, the `drops` range set and `DropCursor` in
  `rs/moq-net/src/model/track.rs`, which record a whole range without its
  reason or budget, even over groups already cached.
- `TrackRun::served` in `rs/moq-net/src/lite/publisher.rs`, which holds
  served and dropped sequences in one set and refuses any group in it.
- In `js/net/src/track.ts`: `#drop`, `#undrop`, the `drops` signal, and the
  wire cursor's `#pending`, `#dropsSeen`, and `#reported`, whose
  `#tryRecvGroup` closes any group already reported.

Untrusted input: a hostile peer can fragment the set by alternating drops
and arrivals. The set never holds more runs than the groups in the
subscription range, the same order as the cache index, so it needs no limit
constant. Extend `track_subscriber_churn` in `rs/moq-net/benches/track.rs`
with a fragmented set, swept over subscriber count and runs, so enumeration
cost tracks the touched runs, not the set.

Test on mock time through a relay:
- On a live track, a reader no wider than the upstream budget gets a prompt
  drop for an upstream OLD naming a group the relay never cached.
- On lite-07, a widened reader gets that group re-offered and never the
  drop, whether the drop was flushed before or after the widening
  SUBSCRIBE_UPDATE. Check what each reader receives, not only tail
  accounting.
- A group arriving after its drop is served, and a drop arriving after its
  group leaves it served.
- Through a lite-06 hop, a reported group stays dropped for every reader,
  with no session error.
- A shrink sends no drops.
- Alternating drops and arrivals never grow the set past the range.

Mirror each case in `js/net`.

Public API: none. Wire: no encoding change; lite-07 SUBSCRIBE_DROP gains the
widening and ordering rules above.

## Required

- [A widened max age re-offers cached groups](/quest/m1/resolved-demand.md) - the re-offer path this extends to reported groups

## Related

- [SUBSCRIBE_DROP accounts for every group](/quest/m1/subscribe-drop.md) - restores the lite-07 drop these rules apply to
