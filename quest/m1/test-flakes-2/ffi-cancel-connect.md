# [XS] moq-ffi cancel reliably aborts a pending connect

## Goal

The moq-ffi test `client_cancel_aborts_a_pending_connect`
(`rs/moq-ffi/src/test.rs`) passes reliably under load. It fails with
`assertion failed: matches!(connect_err, Err(MoqError::Cancelled))`, locally
2 of 960 runs under load; no CI hit was found.

## Plan

Hypothesis, unconfirmed: `spawn_parked` only guarantees one poll, not that the
connect reached `established()`, so the dial can resolve before `cancel()`.

- Confirm the race first.
- Order the cancel deterministically: in the test, or in the code if a cancel
  can lose to a completing connect in production.
- On failure, print the value the connect actually returned.

The 2026-10-20 investigation did not reproduce the reported failure: the
original fixture passed once and in 128 bounded repetitions on an unsaturated
host. Its failing return value and the historical root cause remain unknown.
A controlled two-poll diagnostic confirmed that `spawn_parked` can return a
completed call before cancellation, even when its first poll was `Pending`;
this establishes the helper's ordering gap, not the historical failure's cause.

The fixture now polls the actual foreign connect in place, asserts `Pending`,
then cancels synchronously before its next poll. Both an unexpectedly completed
first poll and a wrong cancellation result print the actual error (or session
success). This ordering passed 128 repetitions; it does not establish that the
historical loaded-host flake is fixed. Reproduce that failure before changing
production cancellation behavior, and verify under naturally saturated load.

Public API: none, unless the race is a product bug. Wire: none.
