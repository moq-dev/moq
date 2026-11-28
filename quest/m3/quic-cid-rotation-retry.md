# [XS] Retry deferred QUIC CID rotation when CID capacity returns

## Goal

In `moq-quic`, if `on_cid_timeout` hits `cids_full()` and receives zero replacement CIDs, it either preserves `retire_seq`/`retire_timestamp` or schedules a retry once CID capacity frees, so expired CIDs are never stranded unrotated.

## Plan

Decided 2026-11-28 (maintainer, `/quest-plan` of the pre-0.22 design-gate audit): ✅ track in `m3` (low real-world impact with default 8-byte lifetime-less CIDs, but a real gap accepted without a follow-up on [moq-dev/moq#9710](https://github.com/moq-dev/moq/pull/9710)).

Caused by [moq-dev/moq#9710](https://github.com/moq-dev/moq/pull/9710).

In `rs/moq-quic/src/connection/cid_state.rs:95-126,158-170` and `rs/moq-quic/src/endpoint.rs:432-471`:
- `on_cid_timeout` pops `retire_timestamp.pop_front()` and increments `retire_seq` before requesting replacement CIDs.
- When `send_new_identifiers` hits `cids_full()` (7/8 cap) and returns an empty `ids` slice, `new_cids(&[])` exits early without scheduling a retry, leaving expired CIDs unrotated even after capacity frees.

Fix: only advance `retire_seq` and pop `retire_timestamp` for replacement CIDs actually issued, or record pending rotation and retry when a CID retires.

Test: a `moq-quic` unit test with a finite CID lifetime hitting `cids_full()` on timeout, then retiring a CID and verifying the deferred rotation completes.

Public API: none. Wire: none.
