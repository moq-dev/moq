# [M] A moq-transport read ends over a hole in its range

## Goal

A Rust moq-transport subscriber settles every sequence in its range, so a read
over a group the publisher dropped or never produced ends with the track
instead of stalling. The PR un-ignores
`a_relay_with_holes_answers_a_wide_past_range_plus_live_over_moq_transport` in
`rs/moq-net/tests/integration/subscribe_ranges.rs`, which then passes on
drafts 18 and 22, and drops its `sh/gh/test-inventory.allow` entry.

## Plan

Found by the subscribe-ranges end-to-end test (2026-11-14). On drafts 18 and
22, with or without a relay hop, a subscriber gets every existing group in
range, then never ends after END_OF_TRACK once its range covers a missing
sequence. The smallest repro needs no ranges: a plain `Range::live()`
subscriber over a direct session that receives groups 0, 1, 3 and then the
track's end waits forever for group 2. moq-lite-06 and lite-07 end at once.

- Find where the moq-transport copy's end waits on group 2 (likely the
  copy's fill of the gap, which no FETCH answer settles), and settle it as a
  drop when the publisher refuses or reports the group absent. Prior Group ID
  Gap, which [IETF time addressing](/quest/m2/time-addressing/ietf.md) reads,
  is best-effort and absent from many peers, so the fix must not rely on it.
- Also explain or fix: `Range::groups(0..)` over cached history 0, 1, 3 on a
  direct draft-22 session delivers only group 3, while `[groups(0..8), live]`
  through a relay delivers the whole past range. If the one-group FETCH
  refusal ([moq-dev/moq#7204](https://github.com/moq-dev/moq/pull/7204))
  drops 0 and 1 by design, the two shapes should still agree.

## Related

- [moq-transport FETCH answers ranges over known holes](/quest/m4/ietf-range-fetch.md) - widens what a range FETCH answers
