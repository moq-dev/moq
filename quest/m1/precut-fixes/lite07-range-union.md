# [M] A lite-07 relay's upstream ranges always fit, and an update costs only its edit

## Goal

A lite-07 relay never gives back the upstream copy every viewer reads through
because its viewers' ranges, merged, do not fit one message: when the exact
union is too large it asks for a covering superset. A SUBSCRIBE_UPDATE costs
work proportional to the ranges it edits, not to every range the
subscription holds.

## Plan

Found by the 2026-11-17 pre-cut correctness audit, from source; reproduce
first. Rechecked after [moq-dev/moq#8908](https://github.com/moq-dev/moq/pull/8908)
rewrote lite-07 to address groups by start: it changed the ranges' units, not
how they merge. lite-07 publishes for the first time at this cut, so this
gates it.

- The union in `rs/moq-net/src/model/track/demand/ranges.rs` (`fold`) merges
  only touching runs. One session sending two SUBSCRIBEs of about 10k
  interleaved single-group ranges, each fitting a message, makes a union that
  does not; the upstream SUBSCRIBE_UPDATE fails and the shared copy is given
  back (`lite/subscriber.rs`), and each new route hits the same union, so
  other sessions' viewers end with an error. Gap fills across many viewers
  can fragment the union the same way with no hostile peer. `edit_keyed` in
  `lite/subscribe.rs` also changes its stored ranges before the size check,
  so a refused edit leaves them out of step with what was sent. Recommended:
  when the exact union cannot fit, coarsen it by merging the smallest gaps
  until it fits, and size-check before mutating. Test: a relay with those two
  SUBSCRIBEs over a lite-07 upstream, asserting a third plain viewer keeps
  receiving.
- `LiveRanges::apply` returns every live range (`numbered`), and
  `TrackRun::update` in `lite/publisher.rs` rebuilds its lookups and hands the
  whole list to the track under the lock every subscriber of it shares, so a
  stream of empty updates against a 32k-range subscription copies megabytes
  each. Recommended: apply only the edit, keyed by Range ID, and skip the
  track call when nothing changed.

Caused by, for the fix PR's `## Caused by`:

- [moq-dev/moq#7872](https://github.com/moq-dev/moq/pull/7872) - exact range unions and whole-set updates
