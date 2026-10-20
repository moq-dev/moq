# [S] qmux byte counters

## Goal

A qmux session over TCP (WebSocket or raw TLS) keeps its connection's
`moq_net::transport::Wire` counters current, so the stats tick counts
WebSocket wire bytes instead of zero. Sent bytes include TLS, WebSocket,
and qmux framing and TCP retransmits, and exclude TCP/IP headers. Received
bytes are what TCP delivered (`tcpi_bytes_received` is RFC 4898's
application-throughput counter), so a peer's duplicate retransmitted payload
is not counted: an accepted undercount on the receive side, documented
beside the field.

## Plan

- moq-dev/web-transport: qmux's `TcpStats` already reads `TCP_INFO` for rtt
  and delivery rate. Add `tcpi_bytes_sent` and `tcpi_bytes_received` from
  the same read. A platform without them (non-Linux, or Linux before 4.19)
  reports `None`, which moq-net maps to 0. Upstream source work:
  moq-dev/web-transport#435 and #436.
- The transport pushes its counters: the stats tick never polls a session,
  and there is no per-connection timer. `TCP_INFO` is read on demand, so
  decide how qmux's I/O path refreshes the `Wire` (for example, after each
  socket write batch) without a syscall per write, then return it from
  moq-tokio's `Grouped::wire` for `qmux::Session`, including the relay's
  `SocketStats` wiring (`rs/moq-relay/src/websocket.rs`).
- Release qmux on every line moq builds against, then bump it here.
- Test: a loopback WebSocket session's `sent_bytes` covers at least the
  payload it wrote.
