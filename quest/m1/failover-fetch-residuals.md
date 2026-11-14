# [M] A fetch survives the track ending on a relay chain

## Goal

On a relay chain, a fetch already in flight when the publisher ends the track
still delivers the rest of its group, and a fetch asked of a copy that has
completed goes on the wire instead of waiting out the subscriber's budget.
Both `residual_*` tests in `rs/moq-net/tests/integration/failover_chain.rs`
flip to assert delivery, and `EndedRelay.cfg`'s open finding is gone.

## Plan

Found 2026-11-14 by the [feature review](/quest/m3/release/feature-review.md):
the tests pin two bugs `verify/failover/FailoverChain.tla` rules out, and no
quest or PR owns them.

- `residual_ending_the_track_fails_a_fetch_in_flight` (`NoSkipWhileHeld`):
  E's fetch of group 0 is in flight through B when the track ends; B's copy,
  asked from group 1, completes, which cancels B's fetch from P, so E's read
  fails though P still writes the rest.
- `residual_a_fetch_from_a_completed_copy_never_leaves_the_relay`
  (`Settles`): reading group 0 on after E's track ended asks B's completed
  copy, and that fetch never goes on the wire, a hang bounded only by the
  subscriber's budget.

Replay `EndedRelay.cfg` on the real stack, fix at the cause, and run
`/verify-failover` and `/verify-resume` on the head.

Public API: none. Wire: none.
