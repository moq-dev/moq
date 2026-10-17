# [M] A reset group keeps its header

## Goal

A moq-lite publisher on a native QUIC or WebTransport stack resets a group
stream with `RESET_STREAM_AT`, still delivering the group header, so the
subscriber can always tell which group the reset belongs to. A subscriber
that knows its publisher does this stops waiting out the tail grace for
groups whose reset lost the header (`rs/moq-net/src/tail.rs`,
`js/net/src/tail.ts`). Subscribers whose publisher cannot do it, such as a
browser, keep the grace.

## Plan

Split from the transport half, which landed in `moq-quic`,
`moq_tokio::quic::web`, and moq-uring's WebTransport adapter (#5690). There,
`SendStream::checkpoint()` marks the data written so far and
`Checkpoint::reset(code)` sends `RESET_STREAM_AT` with that offset as the
Reliable Size. A peer without the extension instead gets a plain
`RESET_STREAM` once it has acknowledged the data before the checkpoint, a round
trip later. The WebTransport adapters already keep their own stream header
this way.

That fallback is weaker than it looks. An ACK only means the peer's QUIC stack
holds the bytes, not that its application read them, and RFC 9000 lets a
receiver discard unread data on `RESET_STREAM`. The WebTransport header
survives because receiving stacks parse it on arrival. A MoQ group header is
read by the application, so check whether Chrome hands it to JS when a plain
`RESET_STREAM` follows the ACK, and treat a peer without `reset_stream_at` as
unable to keep the header unless it does.

- Give moq-net's `transport::poll::SendStream` a reset that keeps a
  checkpoint, using `Checkpoint` rather than a byte count, so a size past the
  written data cannot be asked for. Implement it in the moq-tokio and
  moq-uring adapters. Backends that cannot do it (browser, qmux, iroh) report
  that rather than silently sending a plain reset.
- The lite publisher checkpoints right after writing a group header and
  resets the group from that checkpoint.
- QUIC negotiation alone cannot drop the grace. A peer's stack can advertise
  `reset_stream_at` while its application never sends one, as a browser
  publisher whose WebTransport API has no reliable reset. Dropping the grace
  on negotiation would hang the tail. So the publisher declares it with a
  MoQ-level signal, per session, since each relay hop re-serves groups on its
  own streams. Open: the signal's shape (a SETUP parameter or a version
  feature), and how it composes with SUBSCRIBE_DROP, which already names a
  group reset before its header on lite-05 and later. Any wire change updates
  `drafts/draft-lcurley-moq-lite.md`.
- Remove the grace for subscriptions on sessions that declare it, in Rust and
  `@moq/net`. Datagram holes still wait out the grace.

Test a group reset before any payload with and without the signal, both
directions between Rust and `@moq/net`, and through a relay hop. Cover it in
the Chrome wasm harness and the native interop matrix, which the transport
half did not reach.

## Related

- [SUBSCRIBE_DROP accounts for every group](/quest/m1/subscribe-drop.md) - a
  DROP covers a reset whose header was lost; a reset that keeps its header acts
  as a one-group drop
- [Per-stream ACK progress](/quest/m2/quic-ack-progress.md) - adds
  `Checkpoint::acked()` to the same handle
- [qmux on the QUIC stream state machine](/quest/m2/quic-qmux.md) - qmux reuses
  `moq-quic`'s reset state
