# [L] moq-ffi's run-end tests pass on a loaded host

## Goal

`server_client_run_end_draft18` and its siblings in `rs/moq-ffi/src/test.rs`
(`server_client_run_end` for lite-07 and moq-transport 18, 20, and 22) pass
every run of a loaded `just ci`, without a real-clock deadline deciding the
result.

## Plan

Found 2026-11-26 while gating
[moq-dev/moq#9665](https://github.com/moq-dev/moq/pull/9665):
`server_client_run_end_draft18` hit its 10 s `TIMEOUT` (`kio::clock::timeout`
on real sockets, `rs/moq-ffi/src/test.rs`) while other agents loaded the host,
and passed on rerun.

Decided 2026-11-26 (maintainer, /quest-run): ✅ add it to this epic (rejected:
dropping it). Reproduce it under load first (for example `stress-ng` beside
`cargo nextest run -p moq-ffi run_end` repeated), find which step waits past
10 s and why, and fix that per the epic's rules: an event or a paused or
`kio::clock::Manual` clock instead of the wall-time deadline, or the slow step
itself if the code under test is slow. Delete this quest if it no longer
reproduces on `main`, naming the PR that fixed it.

### Findings

Reproduced 2026-11-26 on `main` (4fba5df73f): 16 parallel loops of the four
run-end tests under `stress-ng --cpu 64`, one test binary run per process,
failed about 1 in 8 runs. Only the moq-transport versions (18, 20, 22) fail;
lite-07 never did.

The 10 s `TIMEOUT` is not a slow host. With `TIMEOUT` raised to 120 s the
failing runs still pass, each after a fixed ~45 s (a run is ~0.15 s otherwise):
`next_encoded_frame` for the first frame of a fresh audio track (or of the
replacement run) returns only after that hold.

What is known, from `TRACE` logs of failing runs:

- The publisher serves the group and its frame, and the subscriber's QUIC
  connection receives and decodes the group header at once. The frame reaches
  `MoqMediaTrackConsumer::next` only ~45 s later (about 1.5x the track's 30 s
  max age, the subscription's max delay clamp that `ledger::grace` and
  `TrackState::arrival_grace` use).
- SUBSCRIBE_OK carried no Largest (the track was empty), so
  `ietf::subscriber` takes `(plan.live, None) => track.start_at(0)`: every group
  from 0 is owed, since a lower group's stream may land after a higher one's,
  and the ordered read holds the first group until that grace folds.
  `moq_mux::container::Consumer` stays at `pending=[] joined=None` meanwhile.
- A passing run hands the same group out within a millisecond, so something
  normally accounts for the keys below the first arrival; which step it is,
  and the race under load that skips it, was not found. The `may_land`,
  `owed`, and `poll_time_start` waits of `rs/moq-net/src/model/track.rs` were
  not the ones holding it (instrumented, never fired in a failing run); the
  hold is below `Cursor::poll_peek_group` or in the front's
  `model/resume.rs` reader (the `Splice` actions after the second `TrackInfo`).

Ruled out: waiting for the publisher's `demand().until_wanted()` before the
first write (the race is on the subscriber, not on write-before-subscribe), and
raising the subscription's max delay (that shortens the hold, which the epic
forbids as a workaround).

Fix at the cause in `moq-net`: either account for the keys below the first
group of an empty IETF live join without waiting the grace (as lite-07 settles
starts with GROUP_END and SUBSCRIBE_DROP(SKIP)), or find and close the race that
makes a loaded run wait it out when a quiet one does not. Verify with
`verify-resume` if `model/resume.rs` changes, and re-run the loaded loop above
with the 10 s `TIMEOUT` (no run may exceed a second). This also holds up every
reader of a new track over moq-transport, not only this test.
