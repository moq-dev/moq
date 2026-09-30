# [S] Lazy stream slots on moq-quic

## Goal

`moq-quic` allocates stream state lazily, so relay memory on it matches
`moq-noq` on the bulk and fanout workloads.

## Plan

Cherry-pick the approved commits of quinn#2601 (the same change noq took as
noq#667); decided 2026-09-30 not to rebase the upstream PR first. #3342 measured the
difference it makes: 75 vs 141 MiB relay memory on bulk and 31 vs 97 MiB on
fanout. Re-run those workloads on `moq-quic` with and without the change.

Check the port against quinn's 2026 MAX_STREAM_DATA advisory fix: noq's lazy
stream code was not exploitable there because it checks the limit itself, so
make sure the combined code keeps exactly one check.

## Required

- [Import quinn](/quest/m1/quic/fork/import.md)

## Related

- [quinn#2601](https://github.com/quinn-rs/quinn/pull/2601) - the approved upstream PR this cherry-picks
- [noq#667](https://github.com/n0-computer/noq/pull/667) - the same change merged into noq
