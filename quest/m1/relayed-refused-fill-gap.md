# [L] A relayed reader reports a refused fill as a gap a direct reader never sees

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

Decided by the maintainer on 2026-11-25 ([answer](https://github.com/moq-dev/moq/pull/9639#issuecomment-7177793296)):
a local Rust reader whose time range resolves through the history handler asks it for the keys
with no group held, as copies and JS already do (rejected: ask nothing, unanswered means empty).

What implementing it found, on top of [moq-dev/moq#9639](https://github.com/moq-dev/moq/pull/9639)'s head:

- Asking now, rather than after the miss grace, has to be scoped to the spans a lookup resolved
  (`Window::found` with `Found::Group` or `Found::Dropped`). Asking every local hole below the
  first group the producer created (`TrackState::unfed` treating them as `never`) makes the
  history test agree across direct, relayed, and JS (`[10, Err(11..15), 15, Err(16..21), 66]`), but
  breaks 23 tests of the deliberate grace for local holes, such as
  `a_hole_below_an_undeclared_start_is_asked_after_a_bounded_grace` and
  `creating_a_group_past_a_gap_skips_nothing`. The scoped rule threads the lookup-resolved runs
  from `Window` through `Misses::judge` into `TrackState::unfed`.
- With asking scoped or not, `a_range_before_every_group_reads_past_it` still disagrees, for the two
  reasons the decisions below settle: the publisher keeps the time's own key in range after a
  dropped lookup, while a relay re-asks its upstream from the next time once that key is dropped;
  and a copy reads an upstream answer of "no group to deliver" as proof the range holds nothing,
  handing out the later group before the drops for the range land.

Before the front-drop fix lands, `a_range_before_every_group_reads_past_it` must assert what
these settle, for both readers and through the later group.

## Waiting

- Maintainer: After a time start's lookup from key 0 is dropped (no group starts at or before time T), does the range include key T itself? The publisher keeps T in range, by design (`a_time_start_before_the_cache_asks_nothing_below_it`), so the lookup's own NotFound drop of T is a gap there; a relay copy that learned T was dropped re-asks its upstream from time T+1 and leaves T out, so the two readers report different gaps.
  - Exclude T everywhere (Recommended) - the dropped lookup already proved no group starts at T; the window starts the span at T+1 in Rust and JS, changing 2 deliberate tests.
  - Include T everywhere - a copy never moves a time start past a key it learned was dropped; keeps the publisher's tests, and its readers report the lookup's drop of T as a gap.
- Maintainer: A copy reads an upstream's "no group to deliver" answer for a time range as proof the range holds nothing (`an_empty_bounded_range_holds_back_no_later_range`), so it hands out a later group before the drops for that range land, while a reader at the publisher, whose handler left those keys unanswered, reports them as a NotFound gap. Which should change?
  - Publisher answers with a start (Recommended) - a range whose keys the publisher dropped is answered with its start rather than "no group to deliver", so the copy waits for those drops, as for any range with keys in it.
  - Copy waits for drops - the copy keeps reading "no group to deliver" as empty but holds a later group until each key of the range is accounted for; costs a wait on every truly empty range.
## Related

- [Previous Delta](/quest/m2/lite07-subscribe/previous-delta.md) - in [moq-dev/moq#9626](https://github.com/moq-dev/moq/pull/9626), changes how a copy learns a range is empty; this gap is the publisher's own NotFound, so it does not settle it.
