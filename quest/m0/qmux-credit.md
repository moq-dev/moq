# [M] qmux returns every byte's credit and sends its close frame

## Goal

A qmux session returns connection-level credit for every byte it receives,
whether the app reads it, drops the stream unread, or it arrives after
STOP_SENDING, so a long-lived session never stalls on MAX_DATA. `close()`
delivers its APPLICATION_CLOSE frame before the transport drops, so a TCP or
WebSocket peer sees the close code whenever the transport stays writable
within the close bound. Both hold on the 0.5 line that
`release` pins and the 0.6 line `main` uses.

## Plan

Facts from moq-dev/web-transport `rs/qmux/src/session.rs` (0.5.1 and main):

- Credit returns only through `RecvStream::report_consumed` on reads.
  `Drop for RecvStream` sends STOP_SENDING and returns stream-count credit,
  but never consumes bytes still buffered or queued in `inbound_data`.
- On STOP_SENDING the writer removes the recv entry, so later STREAM data is
  dropped before the connection-credit check, and a later RESET_STREAM
  returns early, so its final-size gap is never consumed. The peer counts all
  of it against MAX_DATA.
- A new stream's entry is inserted only after `accept_uni`/`accept_bi` hands
  it over, so a stream dropped in that window leaves a stale entry that keeps
  charging the connection window.
- `close()` queues APPLICATION_CLOSE and marks the session closed at once. A
  writer that is mid-write treats that as interrupted and drops the frame.
  0.6 made `close()` idempotent and keeps the first reason, but not the order.

Work, drafted upstream as
[web-transport#412](https://github.com/moq-dev/web-transport/pull/412) (0.6,
targets `main`) and
[web-transport#413](https://github.com/moq-dev/web-transport/pull/413) (0.5,
targets `qmux-v0.5.x`):

- Consume credit for unread bytes on drop, for STREAM data and the RESET
  final-size gap after STOP_SENDING (keep enough of a retired stream's state
  to account for its final size), and close the accept gap.
- Mark the session closed only once the writer has sent the close frame, with
  a bound so a stalled transport still drops.
- Tests in `rs/qmux`: a session that drops many unread streams keeps
  delivering past its initial window, and a peer reads the close code after a
  close issued mid-write.

Remaining here, once both ship: bump `main` to that 0.6.x and backport
`release`'s pin to that 0.5.x. `release` stays on 0.5: 0.6 needs web-transport-trait 0.5, a
breaking change.

Why m0: the WebSocket fallback and the planned edge-to-core `tls://` links
both run on qmux, and MoQ drops streams constantly.

Public API: none. Wire: none.

## Required

- [web-transport releases the fixes](/quest/m0/qmux-credit-upstream.md) - #412 and #413 merge and ship as patched 0.6.x and 0.5.x

## Related

- [qmux on noq-proto](/quest/m2/quic-qmux.md) - replaces these stream maps later
