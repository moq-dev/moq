# [S] qmux byte counters

## Goal

A qmux session over TCP (WebSocket or raw TLS) reports cumulative bytes sent
and received in its `SessionStats`, so moq-net's `Stats.bytes_sent` /
`bytes_received` is nonzero on WebSocket. Sent bytes include TLS, WebSocket,
and qmux framing and TCP retransmits, and exclude TCP/IP headers. Received
bytes are what TCP delivered (`tcpi_bytes_received` is RFC 4898's
application-throughput counter), so a peer's duplicate retransmitted payload
is not counted: an accepted undercount on the receive side, documented
beside the field.

## Plan

- moq-dev/web-transport: qmux's `TcpStats` already reads `TCP_INFO` for rtt
  and delivery rate. Add `tcpi_bytes_sent` and `tcpi_bytes_received` from
  the same read. A platform without them (non-Linux, or Linux before 4.19)
  reports `None`, which moq-net maps to 0.
- Release qmux on every line moq builds against, then bump it here and map
  the fields in moq-net's WebSocket `Stats` and in the relay's
  `SocketStats` wiring (`rs/moq-relay/src/websocket.rs`).
- Test: a loopback WebSocket session's `bytes_sent` covers at least the
  payload it wrote.
