# [S] A relayed reader reports a refused fill as a gap a direct reader never sees

## Goal

An ordered reader of a relay copy reports the same items as the same reader at the publisher.
Asking from a time before every group, a direct reader reads the first group with no gap, while
the relayed one reports `Err(10..21)` with `NotFound` first, once its ordered reader checks for
gaps before every read.

## Plan

Kept by the maintainer on 2026-11-24 ([pick round](https://github.com/moq-dev/moq/pull/9529#issuecomment-7140096782)).

Found while building [moq-dev/moq#9529](https://github.com/moq-dev/moq/pull/9529), with the fix
[A front's ordered reader reports a drop that lands with the group above it](/quest/m1/front-drop-with-next-group.md)
plans applied: `waiting_time_start::a_range_before_every_group_reads_past_it` in
`rs/moq-net/tests/integration/` fails there, the relayed reader reporting a NotFound gap over
`10..21` where the direct one reports none.

[moq-dev/moq#9639](https://github.com/moq-dev/moq/pull/9639) traced it. The relayed reader is not
wrong: the gap is the publisher's own answer. The two readers differ in what they ask the
publisher's history handler (`track::Requests`):

- A copy whose window covers `Time(10)..20` asks upstream for keys `10..21` with a range fill. The
  test's handler drops it unanswered, which records a `NotFound` miss on the publisher's own track
  (`fill::Inner::drop`), so the copy reports `Err(10..21)`. A direct reader opened at the publisher
  after that miss, asking only `Time(10)..20`, reports the same gap.
- A direct Rust reader at the publisher looks up only the group containing time 10, and never asks
  the handler for the rest of the range: a local track owes no key between groups
  (`TrackState::owed`). With a handler holding groups 10 and 15, it reads `[10, 66]`, while the
  relayed reader reads `[10, 15, Err(16..21), 66]`.
- A direct JS reader does ask the handler for the rest (a fill over `11..21`), and reads
  `[10, Err(11..15), 15, Err(16..21), 66]`.

[moq-dev/moq#9639](https://github.com/moq-dev/moq/pull/9639) fixed the separate bug that hid this:
a direct ordered reader handed out group 66 before its time start's lookup answered, then never
returned the group the lookup found.

Before the front-drop fix lands, `a_range_before_every_group_reads_past_it` must assert what
the decision below settles, for both readers and 2 items.

## Waiting

- Maintainer: Which answer is right for a local ordered reader whose time start resolves through the track's history handler, with keys in range that no group held covers?
  - Ask the handler (Recommended) - a local reader asks the handler for the unheld keys of a range resolved through a lookup, as a relay copy and JS already do, so direct, relayed, and JS read the same; costs one more fill per history range on a publisher with a handler.
  - Ask nothing - copies stop asking upstream for keys a lookup resolved past, matching today's Rust local reader; a relayed reader then loses history groups like 15 that the handler holds.
  - Unanswered is empty - a fill the handler drops unanswered counts as no group there, not a NotFound miss; removes the gap in this test but still lets readers differ on groups like 15, and hides real history loss.

## Related

- [Previous Delta](/quest/m2/lite07-subscribe/previous-delta.md) - in [moq-dev/moq#9626](https://github.com/moq-dev/moq/pull/9626), changes how a copy learns a range is empty; this gap is the publisher's own NotFound, so it does not settle it.
