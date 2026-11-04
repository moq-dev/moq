# [S] A bounded read that ended keeps returning None

## Goal

Once a `moq_net::track::Subscriber` with a bounded range has returned
`Ok(None)` from `recv_group`/`next_group`, every later call returns
`Ok(None)` at once instead of parking, until an update widens its bounds.
The contract is documented on both
methods and tested on the Plain and Resume cursors. A wire publisher's
deliberate park past its bounded end (`parks`) is left as it is.

## Plan

Decided 2026-11-03 (maintainer, /quest-plan of the follow-ups from
[moq-dev/moq#7708](https://github.com/moq-dev/moq/pull/7708)): milestone m1,
in the ranges epic, since it is the bounded-range read contract.

- Decision prompt (maintainer, 2026-11-03): what a second read after a
  bounded range's `Ok(None)` does:
  - ✅ Fused: `None` again, as with Rust's `FusedStream`.
  - Park until new data.
  - Leave the choice to this quest.
- Refinement made while planning: the model's doc says a group outside the
  bounds is held rather than skipped past, so widening the bounds later makes
  it available again. Keep that. The read stays fused only until the
  subscription's bounds widen. After that, reads resume over the new range.
  If that reopening turns out not to be reachable through the public API,
  plain fused is enough.
- Today a later read can park. The first completion returns `Ok(None)`
  through `cursor.is_exhausted` (`rs/moq-net/src/model/track.rs`, Plain
  arm of `poll_peek_group`). A later read goes back through
  `TrackState::readable()`, which is false while the copy is idle, and parks
  without reaching that check. Find the Resume arm's equivalent and make both
  report exhaustion before parking.
- Tests: a bounded read that ends, then two more reads, on Plain and Resume
  cursors and with `parks` off. If bounds widening is reachable, a widened
  update after the end delivers the new groups.

Public API: documented behaviour of `recv_group`/`next_group` (no
signature change). Wire: none.

## Related

- [Delete the old start bookkeeping](/quest/m1/subscribe-ranges/start-bookkeeping.md) - lite end semantics per cursor
