# [S] Relay peers get wider limits

## Goal

A relay-to-relay session runs with wider stream and data limits than a
viewer's session, decided after the handshake rather than at bind time. A
cluster peer carrying thousands of broadcasts is never throttled by the
concurrent-stream count sized for one browser, and the viewer-facing limits
stay small so one client cannot reserve a relay's memory.

## Plan

The in-tree `moq-quic` `Connection` (`rs/moq-quic/src/connection/mod.rs`)
already exposes runtime `set_max_concurrent_streams`, `set_receive_window`,
and `set_send_window`; a raise queues `MAX_STREAMS` and `MAX_DATA` on the next
packet, and a shrink is a debt paid as the peer consumes credit. The core
needs no change; the work waits only for
[the fork switch](/quest/m1/quic/fork/README.md), so moq-tokio and moq-uring
run on `moq-quic` instead of `moq-noq`.

Decided 2026-10-08: [relay session limits](/quest/m1/relay-session-limits.md)
lands first and introduces the relay's peer config surface (classifying a
session as a cluster peer, and a peer table); this quest extends that
surface with the QUIC values rather than adding its own.

- moq-net's transport trait gains `set_limits(Limits)` on the session,
  `Limits` carrying the three values, implemented by the moq-tokio and
  moq-uring adapters over `moq-quic` after the fork switch; every other
  backend, the browser included, reports it unsupported.
- The peer table gains the same three values (the stream limit and the two
  windows),
  defaulting to an order of magnitude above the client defaults. `moq-relay`
  applies it once a session is classified as a cluster peer, on the io_uring
  workers too.
- Refuse a `peer` value below the client default rather than silently
  shrinking.

Tests: a cluster session sees the raised `MAX_STREAMS` after SETUP and a
viewer session does not; the io_uring path applies the same values; a
`peer` table below the defaults is refused at resolve time.

## Required

- [Relay session limits](/quest/m1/relay-session-limits.md) - introduces the peer classification and config table this extends
- [Hard fork](/quest/m1/quic/fork/README.md) - moq-tokio and moq-uring run on `moq-quic`, whose setters this calls

