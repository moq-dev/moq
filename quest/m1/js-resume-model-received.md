# [L] The JS resume model's routes are received copies, as Rust's are

## Goal

`js/net/src/resume.model.test.ts` drives its fronts over routes whose tracks
are received copies, as `rs/moq-net/src/model/resume_model.rs` has since
[moq-dev/moq#7670](https://github.com/moq-dev/moq/pull/7670) (`track.received()`),
and its sweep passes on any seed. Today the JS routes are local publishers, so
the JS half never exercises how a front reads copies fed by a wire: the JS
reader's behavior there is unchecked, where real viewers read it.

## Plan

Found on [moq-dev/moq#9319](https://github.com/moq-dev/moq/pull/9319): the
ordered-read counterexamples it fixed came from the two models testing
different topologies. On a local publisher an ordered read passes a key below a
group the publisher created (the Rust `Cursor::passes`), while on a received
copy it waits for that key.

Call `hooks.receiveTrack(producer)` on each route's track in the JS model's
`up`, the counterpart of Rust's `received()` (a received copy whose wire
reports no range starts, as `ietf/subscriber.ts` marks its copies). With
that one line, the JS sweep fails where Rust passes, so decide for each case
from the Rust reader whether the JS reader or the JS oracle is wrong:

- Regression 27 of the JS `regressions` (a give-up, an expiry, then a backfill
  of group 2): an arrival reader hands out `2,0,3..11` and never group 1. Rust
  passes the same ops.
- `MOQ_RESUME_SEED=3` and `11` (4096 cases): `join r unordered none; ranges r
  [1,2]; write 0/0 next; up 0/2; down 0/2; write 0/0 next` misses group 0, the
  group holding the time start.
- `MOQ_RESUME_SEED=10`: `up 1/0; join r0 unordered [1,2]; write 1/0 next; up
  1/1; close 1/1 g0 down; up 0/0` misses group 0.
- `MOQ_RESUME_SEED=12`: `join r0 unordered short [2,3]; giveUp 0/0 g1; expire;
  join r1 ordered none; advance 1` misses groups 2 to 11.

Add each case to the JS `regressions`, and to the Rust model's when it reaches
code both share. Keep the local-publisher coverage the JS model has now only if
a case needs it; Rust tests local tracks in `track.rs` unit tests instead.

Public API: none. Wire: none.
