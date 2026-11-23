# [L] A resumed reader's ranges place where the resume model expects

## Goal

The Nightly `test resume-model` sweep's Rust half passes on seeds 22, 28 and
33: a resumed reader whose time range a copy placed from the groups it held
reads the groups the model in `rs/moq-net/src/model/resume_model.rs` owes it,
or the model is corrected where it is wrong. JS (`js/net/src/resume.ts`) is
checked against the same cases.

## Plan

Found on [moq-dev/moq#9326](https://github.com/moq-dev/moq/pull/9326)'s
sweeps at 4096 cases. Each fails the same way on `main` at `77a10b58d8`.
None is the exhaustion check that PR rewrote:

- `MOQ_RESUME_SEED=33`: `reader 3: missing groups [3, 4, 5]`. The case is
  `join r3 unordered [0,1]; up 0/1; write 0/0 1; write 0/0 8; write 0/1
  next; ranges r3 [0,6]; up 0/2; write 0/1 2; write 0/2 next`. The replaced
  copy placed time end 5 at group 1, because it held groups 1 and 8 and
  nothing between. The serving copy is asked for the range clamped by that
  resolution (`Range::clamped`), which leaves it empty above its floor of 2.
  So the read ends without groups 3 to 5, which land later.
- `MOQ_RESUME_SEED=22`: `reader 0: missing groups [6]` for range `[6,7]`.
  The range was placed when no copy held a group at or below time 6, and the
  model owes from the start time.
- `MOQ_RESUME_SEED=28`: `reader 0: missing groups [0]`. The reader's range
  `[3,4]` is dropped after a route failover, and the unbounded read never
  hands out group 0.

- The oracle owes a reader nothing below where the serving copy placed its
  range, so the regression case `join r2 unordered [8,9]; bounds r2 [1,9];
  write 0/0 next; up 0/1` (in both models) never checks group 0, which the
  local floor keeps out under that placement: an early end passes it too
  (found by CodeRabbit on moq-dev/moq#9326). Once placement is settled, the
  oracle should owe such a group until a copy declares it skipped, and the
  case should fail when the kept-out check in `Reader::accounts` is
  disabled.

Start from seed 33. A copy's resolution of a time end it cannot prove, like
a time start ([moq-dev/moq#9307](https://github.com/moq-dev/moq/pull/9307)),
should not narrow what the next copy is asked for. Decide per case whether
the code or the oracle (`Reader::place`) is wrong. Add each case to both
models' `regressions` once it passes.
