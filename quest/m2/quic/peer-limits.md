# [S] Relay peers get wider limits

## Goal

A relay-to-relay session runs with wider stream and data limits than a
viewer's session, decided after the handshake rather than at bind time. A
cluster peer carrying thousands of broadcasts is never throttled by the
concurrent-stream count sized for one browser, and the viewer-facing limits
stay small so one client cannot reserve a relay's memory.

## Plan

noq-proto already exposes runtime `Connection::set_max_concurrent_streams`,
`set_receive_window`, and `set_send_window`; a raise queues `MAX_STREAMS` and
`MAX_DATA` on the next packet, and a shrink is a debt paid as the peer
consumes credit. Nothing MoQ builds on top needs a fork change.

- `web-transport-noq` (and the trait, with an unsupported default for the
  browser) exposes a `set_limits(Limits)` on the session, `Limits` carrying
  the three values.
- moq-tokio's `[quic]` section gains a `peer` sub-table with the same three
  window fields plus `max_streams`, defaulting to an order of magnitude above
  the client defaults. `moq-relay` applies it once SETUP identifies the
  session as a cluster peer (the cluster extension's role, not the peer's
  address), on the io_uring workers too.
- Refuse a `peer` value below the client default rather than silently
  shrinking.

Tests: a cluster session sees the raised `MAX_STREAMS` after SETUP and a
viewer session does not; the io_uring path applies the same values; a
`peer` table below the defaults is refused at resolve time.

## Related

- [io_uring flow control](/quest/m2/uring-flow-control-windows.md) - the
  static windows on the same workers
