# [S] Lazy stream slots on moq-quic

## Goal

`moq-quic` allocates stream state lazily, so relay memory on it matches
`moq-noq` on the bulk and fanout workloads.

## Plan

quinn#2601 (the same change noq took as noq#667) merged upstream on
2026-09-30, after the MAX_STREAM_DATA advisory fix, and arrived with the
import at quinn `7616e6b2`. So `moq-quic` already allocates remote stream
slots lazily, and only the measurement remains. #3342 measured the difference
it makes: 75 vs 141 MiB relay memory on bulk and 31 vs 97 MiB on fanout.
Re-run those workloads on `moq-quic` with and without the change.

## Related

- [quinn#2601](https://github.com/quinn-rs/quinn/pull/2601) - the upstream change, already in `moq-quic`
- [noq#667](https://github.com/n0-computer/noq/pull/667) - the same change merged into noq
