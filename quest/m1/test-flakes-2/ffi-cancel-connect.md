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

Public API: none, unless the race is a product bug. Wire: none.
