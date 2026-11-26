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

Decided 2026-11-25 (maintainer, /quest-run): a local Rust reader whose time range resolves through
the history handler asks it for the keys with no group held, as copies and JS already do, narrowed
to ranges whose start a history lookup placed (`Window::found` with `Found::Group` or
`Found::Dropped`) (rejected: ask nothing, unanswered means empty)
([answer](https://github.com/moq-dev/moq/pull/9639#issuecomment-7177793296)).

Decided 2026-11-25 (maintainer, /quest-run): once a history lookup comes back empty from key 0, no
group starts at or before the time, so the range starts one past the time, in Rust
(`Window::found`) and JS (`Window.found`): the time's own key is out of range everywhere, and the
lookup's own NotFound drop of it is no gap (rejected: include the key everywhere). This changes the
deliberate `a_time_start_the_groups_cant_place_is_looked_up` and
`a_time_start_before_the_cache_asks_nothing_below_it`.

Decided 2026-11-25 (maintainer, /quest-run): a publisher answers a range with a start, never "no
group to deliver", whenever it dropped keys in it, so a copy waits for those drops rather than
handing out a later group first (rejected: the copy waits on every key of a range answered empty).
Built on the lite-07 wire of [moq-dev/moq#9686](https://github.com/moq-dev/moq/pull/9686), where `GROUP_END` and `SUBSCRIBE_DROP(SKIP)` tell a copy a range is empty.

What a broad first attempt found, on top of [moq-dev/moq#9639](https://github.com/moq-dev/moq/pull/9639):

- Making `TrackState::unfed` treat every hole on a local track below the first group the producer
  created (`arrivals.first()`) as `never`, so `Misses::judge` asks it now rather than after the miss
  grace, makes a history handler holding groups 10 and 15 read the same through direct, relayed,
  and JS readers: `[10, Err(11..15), 15, Err(16..21), 66]`. It breaks 23 tests of the deliberate
  grace for local holes, such as `a_hole_below_an_undeclared_start_is_asked_after_a_bounded_grace`
  and `creating_a_group_past_a_gap_skips_nothing`, which is why the ask is narrowed: thread the runs
  of lookup-placed spans from `Window` through `Cursor::probe` and `Misses::judge` into `unfed`.
- With that ask, a relay subscribing after the publisher already recorded the drops hung: the
  publisher answered the range "no group to deliver", and the copy placed the time before every
  group (`TimeStart::containing` gives `Containing::Before`), reading back to key 0 and waiting on
  keys no upstream accounts for. The publisher-side answer above removes that case.
- Gaps split by when their drops land, so the publisher reports `Err(10..11)` then `Err(11..21)`
  where a relay gets one drop over `10..21`. Compare readers with adjacent gaps joined, and take
  the excluded time key above into account.

Then `a_range_before_every_group_reads_past_it` asserts the same items for both readers through
the later group, before the front-drop fix lands.
