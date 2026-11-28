# [XS] moq-quic stops sticky ecn_prior_path from discarding new-path CE feedback

## Goal

A late old-path packet marked ECT(0) whose ACK does not advance largest-acked (`new_largest == false`) never leaves `ecn_prior_path` set to discard a genuine CE congestion signal on a later new-path ACK.

## Plan

Decided 2026-11-28 (maintainer, `/quest-plan` of the pre-0.22 design-gate audit): ✅ fix before `0.22.0` (this was Codex's Finding #2 on [moq-dev/moq#9709](https://github.com/moq-dev/moq/pull/9709), which had 3 open findings while the acceptance message addressed only 2).

Caused by [moq-dev/moq#9709](https://github.com/moq-dev/moq/pull/9709).

In `rs/moq-quic/src/connection/mod.rs:2120,2206,2236,2269-2285`:
- `detect_spurious_loss` (`:2236`) sets `self.spaces[space].ecn_prior_path = true` whenever a prior-path packet has `sent.ecn_marked == true`, even when `new_largest == false` (or `newly_acked.is_empty()` at `:2120`).
- Because `process_ecn` (`:2206`) runs only when `new_largest` advances, `ecn_prior_path` stays sticky until a later in-order ACK for new-path packets arrives, at which point `process_ecn` (`:2280`) sees `ecn_prior_path == true`, silently discards genuine new-path CE feedback, and advances `ecn_feedback` past it.

Fix: only set `ecn_prior_path` when the ACK carries an unattributed CE delta (`ce_delta > 0`), or consume/clear `ecn_prior_path` on every ACK carrying ECN counts rather than gating `process_ecn` solely on `new_largest`.

Test: a `moq-quic` unit test migrating paths, delivering a late old-path ECT(0) packet with `new_largest == false`, then delivering a new-path ACK reporting a CE increment and asserting congestion control reacts to it.

Public API: none. Wire: none.
