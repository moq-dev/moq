# [M] noq carries quinn's stream reassembly cap

## Goal

The QUIC stacks the relay builds on bound how many out-of-order chunks a
stream or CRYPTO buffer holds, as quinn-proto 0.11.15 does, and a connection's
receive window is finite by default. Today `moq-noq-proto` 1.3.2 (`main`) and
2.0.0 (`dev`), and upstream `noq-proto` 1.3.0 (pulled in by the relay's default `iroh`
feature), predate quinn's fix, and `cargo audit` cannot match them because
the crates are renamed.

## Plan

- Port quinn [fed0321a](https://github.com/quinn-rs/quinn/commit/fed0321a)
  ([quinn#2694](https://github.com/quinn-rs/quinn/pull/2694),
  RUSTSEC-2026-0185) to moq-dev/noq: `Assembler::insert` returns an error past
  1024 buffered chunks after defragmenting, the stream path closes with
  `INTERNAL_ERROR`, and the CRYPTO path does the same. Keep quinn's test.
  Release 1.3.3 and 2.0.1, then bump the pins here.
- Open the same port as a PR on n0-computer/noq (approved 2026-09-29; the
  advisory is public). iroh builds stay on the unfixed upstream crate until
  n0 releases it; bump when they do.
- Give `moq-tokio` a finite default connection `receive_window` instead of
  the backend's `VarInt::MAX` (`rs/moq-tokio/src/noq.rs` `apply_windows`,
  `quic.rs`). Pick the value with a throughput measurement, not a
  guess, and update `doc/bin/relay/config.md`.
- `rs/moq-uring` depends on `moq-noq-proto` too, so the same pin bump
  covers the io_uring workers.

Public API: none. Wire: a peer that exceeds the chunk cap is closed.

## Related

- [QUIC release](/quest/m1/quic/release.md) - owns the fork's security-update procedure this gap shows is missing
- [Peer limits](/quest/m1/quic/peer-limits.md) - per-peer windows and stream limits
