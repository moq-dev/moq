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

The copy asks upstream to fill the range before the only group; upstream refuses it, since no
group exists there, and the copy records the refusal as a gap. A range that resolves empty at the
publisher is no loss: the copy must answer it the same way, per the cache rule in `AGENTS.md`.
Start by checking how the refusal reaches `find_gap` on the copy, and whether lite-07's
SUBSCRIBE_DROP or the fill's refusal carries it. Check JS's twin of the test too.
