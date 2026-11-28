# [M] Lite-07 timestamp mismatch egress, proven ledger bounds, and finish_at binding parity

## Goal

On `moq-lite-07`, both Rust and JS publishers reset a group stream with terminal `TIMESTAMP_MISMATCH` (`0x39`) whenever frame or end timestamps decrease (never `MALFORMED_TRACK` or retryable `INTERNAL_ERROR`), a proven lite-07 `Ledger` never fabricates gap proof by folding active gaps at `MAX_RUNS`, `draft-lcurley-moq-lite.md` states both rules clearly, and Dart and C++ bindings expose `finish_at` alongside Python, Swift, Go, and Kotlin.

## Plan

Decided 2026-11-28 (maintainer, `/quest-plan` of the pre-0.22 design-gate audit): ✅ fix all three lite-07 wire and cross-package parity bugs before `0.22.0` (rejected: deferring past `0.22.0`).

Caused by [moq-dev/moq#9794](https://github.com/moq-dev/moq/pull/9794) (Decision H claimed lite-07 egress matched draft-23 `TimestampMismatch`, which was false on `main`) and [moq-dev/moq#9686](https://github.com/moq-dev/moq/pull/9686) (high-risk breaking PR reviewed by only one reviewer, omitting Kotlin/Dart/C++ wrappers and allowing proven-ledger gap folding).

1. **Lite-07 decreasing-timestamp egress (`#9794`):**
   - Rust (`rs/moq-net/src/lite/publisher.rs:5140-5144`, `rs/moq-net/src/error.rs:757,765`): have `Clock::encode_delta` return a typed error distinguishing decreasing timestamps (`Error::TimestampMismatch` -> `0x39`) from `i64` overflow (`Error::BoundsExceeded` -> `0x12`), matching `buffer_group_end` (`publisher.rs:7729`) and draft-23 `TimedIds`.
   - JS (`js/net/src/lite/publisher.ts:152-154,167-169,3370-3378`): throw `TimestampMismatch` (not raw `RangeError`, which `servedError` converts into retryable `StreamCode.Internal` `0x0`) when `unsigned && delta < 0n` in `frameHeader` and when `end < base` in `groupEnd`.
   - Draft (`drafts/draft-lcurley-moq-lite.md:205`): broaden `0x39 TIMESTAMP_MISMATCH` from frame 0 alone to cover non-monotonic frame and `GROUP_END` timestamps.
2. **Proven ledger gap folding & Q8 draft clarity (`#9686`):**
   - In `rs/moq-net/src/ledger.rs:32,167,203-212`, prune runs strictly below the lowest active range floor before checking `MAX_RUNS = 4096`. Only fold oldest gaps on non-proven (grace-based) ledgers; on a proven lite-07 ledger (`grace == Duration::MAX`), fail/bound a peer exceeding 4096 live disjoint runs instead of fabricating proof that missing groups were accounted for.
   - Tighten `drafts/draft-lcurley-moq-lite.md:887-888` to note that a publisher with no fill handler waits until `SUBSCRIBE_END` or explicit skip/end rather than emitting `NOT_FOUND` mid-track for undeclared gaps.
3. **Cross-package `finish_at` binding parity (`#9686`):**
   - Wrap `finish_at` in Dart (`dart/`) and C++ (`cpp/moq/`), matching `rs/moq-ffi/src/producer.rs:1165-1174`, `rs/libmoq/src/api.rs:4261-4266`, Python, Swift, and Go; verify Kotlin (`kt/`, whose `GroupProducer` aliases `MoqGroupProducer`) already exposes `finish_at` cleanly.

Tests: Rust and JS egress tests verifying relayed decreasing-timestamp groups reset with terminal `TIMESTAMP_MISMATCH` (`0x39`), a proven-ledger unit test verifying `Ledger::account` never folds active gaps past 4096 runs, and binding tests for Dart, C++, and Kotlin `finish_at`. Run `just test interop --all`.

Public API: additive `finish_at` wrappers in Dart and C++ (closing pre-0.22 binding parity). Wire: aligns lite-07-wip error codes and draft text before `lite-07` freezes.

## Related

- [Timestamp IDs handle the two legacy edges #9794 accepted](/quest/m2/timestamp-ids-legacy-edges.md) - owns #9794 Decision G's two legacy edges
