# [S] Flow-control windows on the io_uring workers

## Goal

The `[quic]` flow-control windows (`receive_window`, `stream_receive_window`,
and `send_window`, added for the tokio workers in #816) reach the relay's
io_uring workers instead of being refused at startup, so the same `[quic]`
section tunes both worker runtimes.

## Plan

`moq_uring::quic::Transport` carries the per-connection knobs the relay maps
its `[quic]` section onto, and today it has no window fields: both sans-IO
backends hardcode them (`STREAM_WINDOW` / `CONNECTION_WINDOW` in
`rs/moq-uring/src/quic/quinn/mod.rs`, the `set_initial_max_*` calls in
`rs/moq-uring/src/quic/quiche/mod.rs`). `transport()` in
`rs/moq-relay/src/uring.rs` therefore refuses all three rather than dropping
them.

Add the three as `Option<u64>` on `Transport`, with the current constants as
the defaults, and apply them in each backend. Then drop the refusal in
`transport()` and the paragraph in `doc/bin/relay/config.md` that documents it.

The one wrinkle is `send_window`: quiche has no local send cap, so a
`moq-uring/quiche` build cannot honor it while the `noq` and `quinn` builds
can. Which backend is compiled is a moq-uring cargo feature the relay cannot
see, so refusing it accurately needs moq-uring to report its own capability
rather than the relay guessing. Settle that shape first; a silent drop is not
an option.
