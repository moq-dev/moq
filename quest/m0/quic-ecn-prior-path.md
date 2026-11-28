# [XS] moq-quic stops sticky ecn_prior_path from discarding new-path CE feedback

## Goal

A late old-path packet marked ECT(0) whose ACK does not advance largest-acked (`new_largest == false`) never leaves `ecn_prior_path` set to discard a genuine CE congestion signal on a later new-path ACK.

## Plan

Decided 2026-11-28 (maintainer, `/quest-plan` of the pre-0.22 design-gate audit): ✅ fix before `0.22.0` (this was Codex's Finding #2 on [moq-dev/moq#9709](https://github.com/moq-dev/moq/pull/9709), which had 3 open findings while the acceptance message addressed only 2).

Caused by [moq-dev/moq#9709](https://github.com/moq-dev/moq/pull/9709).

In `rs/moq-quic/src/connection/mod.rs:2131,2195,2225,2257-2270`:
- Both the ACK loop (`mod.rs:2131`, `self.spaces[space].ecn_prior_path |= info.ecn && !this_path`) and `detect_spurious_loss` (`mod.rs:2225`, `space.ecn_prior_path |= lost.ecn_path.is_some_and(|path| path != generation)`) set `ecn_prior_path = true` for any prior-path ECN-marked packet, even when `new_largest == false`.
- Because `process_ecn` (`mod.rs:2195`) runs only when `new_largest` advances, `ecn_prior_path` stays sticky until a later in-order ACK for new-path packets arrives, at which point `process_ecn` (`mod.rs:2258,2269`) sees `ecn_prior_path == true`, silently discards genuine new-path CE feedback, and advances `ecn_feedback` past it.

Fix: handle both set sites (`mod.rs:2131` and `mod.rs:2225`) so `ecn_prior_path` is either consumed/cleared on every ACK carrying ECN counts (advancing `ecn_feedback` only for the prior-path ACK's own counts when appropriate) or only carried forward when the prior-path ACK actually accounts for an unresolved CE delta, never suppressing a later new-path CE increment.

Test: a `moq-quic` unit test migrating paths, delivering a late old-path ECT(0) packet with `new_largest == false`, then delivering a new-path ACK reporting a CE increment and asserting congestion control reacts to it.

Public API: none. Wire: none.
