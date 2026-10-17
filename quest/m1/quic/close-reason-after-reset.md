# [S] A stateless reset keeps a closed connection's reason

## Goal

After a connection closes on its own transport error, a later stateless reset
drains it without changing what the application sees: the original close
reason is reported once. Today, while the connection is `Closed`, the reset
replaces the unread reason with `Reset`, or, once the reason was read,
yields a second `ConnectionLost`.

`stateless_reset_after_local_close_keeps_reason` and
`stateless_reset_after_local_close_is_not_a_second_loss` in
`rs/moq-quic/src/tests/characterize/frames.rs` must pass with their
`#[ignore]` removed.

## Plan

`Connection::finish_packet` in `rs/moq-quic/src/connection/mod.rs` preserves
the close reason on a reset only in `Draining` or `Drained`. Extend that to
`Closed`: move to `Drained` without touching `self.error`, so `poll`, which
takes the error once, reports nothing new.
