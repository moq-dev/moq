# [S] One CLI duration type

## Goal

moq-cli, moq-bench and moq-relay parse and serialize human-readable
durations through moq-tokio's `cli::Duration`, made public, instead of their
own copies. moq-relay's hand-written serde modules for `std::time::Duration`
fields use the existing `humantime-serde` dependency.

## Plan

Found by the 2026-10-22 refactor audit (main at d44813cb7): moq-cli's and
moq-bench's `duration.rs` are byte-identical, moq-relay's adds serde
adapters, and moq-tokio's `cli::Duration` is `pub(crate)` but already
implements `FromStr`, `Display` and serde. All three crates depend on
moq-tokio.

Decided 2026-10-22 (maintainer): export moq-tokio's type over keeping
private per-crate wrappers. Preserve accepted units, zero handling,
formatting, and config file round-trips; check the CLI help and config docs
still match.

Public API: adds `moq_tokio::cli::Duration`. Wire: none.
