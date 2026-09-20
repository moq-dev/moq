# [S] Flow-control windows on the io_uring workers

## Goal

The `[quic]` flow-control windows (`receive_window`, `stream_receive_window`,
and `send_window`, added for the tokio workers in #816) reach the relay's
io_uring workers instead of being refused at startup, so the same `[quic]`
section tunes both worker runtimes.

## Plan

`moq_uring::quic::Transport` carries the per-connection knobs the relay maps
its `[quic]` section onto, and today it has no window fields: the backend
hardcodes them (`STREAM_WINDOW` / `CONNECTION_WINDOW` in
`rs/moq-uring/src/quic/noq/mod.rs`). `transport()` in
`rs/moq-relay/src/uring.rs` therefore refuses all three rather than dropping
them.

Add the three as `Option<u64>` on `Transport`, with the current constants as
the defaults, and apply them. Then drop the refusal in `transport()` and the
paragraph in `doc/bin/relay/config.md` that documents it.

## Related

- [Relay peers get wider limits](/quest/m2/quic/peer-limits.md) - raises the
  same windows at runtime for cluster peers
