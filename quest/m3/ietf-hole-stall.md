# [M] A moq-transport read skips a hole its max age rules out

## Goal

A Rust moq-transport reader, arrival or ordered, skips a missing group once
its max age makes that group impossible to arrive, and ends with the track
instead of stalling. The PR un-ignores
`a_relay_with_holes_answers_a_wide_past_range_plus_live_over_moq_transport` in
`rs/moq-net/tests/integration/subscribe_ranges.rs`, which then passes on
drafts 18 and 22, and drops its `sh/gh/test-inventory.allow` entry.

## Plan

The rule (maintainer, on [moq-dev/moq#8762](https://github.com/moq-dev/moq/pull/8762)):
do as the ordered consumer does, and skip a hole once max age renders it
impossible to arrive. Reuse the existing stale verdict (`TrackState::drifted`,
which already judges an uncached sequence by its successor's reach, and the
give-ups `Misses::expire` records); don't add a second mechanism.

What #8762 measured, with groups 0 to 9 stamped within a second, group 4
aborted, group 7 never produced, a 60s subscriber budget, and live group 10
at 100s, read through an origin (`[groups(0..8), live]`, direct or through a
relay):

- The moq-transport copy's own cursor reaches the feed's end with nothing
  waiting and both holes already given up as stale (`given_up = {4..5, 7..8}`).
- The reader the origin hands out still never ends: the arrival reader stalls
  after every existing group, and the ordered reader stalls at group 4. So the
  copy's stale verdict is lost in the front's reader (`rs/moq-net/src/model/resume.rs`:
  `exhausted`, `poll_end`, and how a copy's given-up runs reach the logical
  reader). lite-07 takes the same path and ends, because its holes arrive as
  SUBSCRIBE_DROP.
- A live-only range with a finite budget already ends on moq-transport.

Fix it where the front's reader settles a copy's runs, so both readers count a
copy's stale give-ups. Run `/verify-resume` when the change touches
`resume.rs`.

Not in scope: with no max age at all (subscriber budget `None`), nothing makes
a moq-transport hole impossible, since moq-transport egress never sends the
publisher's max age (`Info::max_age` in `rs/moq-net/src/model/track.rs`), so
the reader waits by design, as the ordered reader's docs say.

## Related

- [moq-transport FETCH answers ranges over known holes](/quest/m4/ietf-range-fetch.md) - widens what a range FETCH answers
