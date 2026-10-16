# Wire stats

## Goal

The relay's session stats carry `wire_tx` and `wire_rx` in `Presence`: the
bytes each root's connections put on and took off the wire, on each tier.
They appear in the `totals.json` track per tier and in the per-root
`presence.json` track ([Stats](/doc/concept/stats.md#tracks)). An operator
reads what each tenant root costs in transport bytes, not only the payload
`Traffic.bytes` counts.

Wire bytes are the transport's payload, with IP, UDP, and TCP headers
excluded:

- **QUIC and WebTransport:** `udp_tx` / `udp_rx`, which include handshake,
  ACKs, and retransmits.
- **WebSocket:** the TCP stream's bytes, which include TLS, WebSocket, and
  qmux framing and retransmits.
- **Gateways an embedder terminates:** SRT and WebRTC through moq-srt's and
  moq-rtc's sources. An embedder terminating RTMP on its own socket
  registers that socket's `TCP_INFO` counts; moq-rtmp's own `Server` and
  client expose no source until a consumer needs one.
- **TCP receive:** bytes TCP delivered, so a peer's duplicate retransmits
  are not counted.

Non-goals:

- **Splitting a shared connection's bytes across roots.** A cluster
  connection's wire bytes count under its own root, which is empty.
- **Per-broadcast wire bytes.** Wire bytes belong to connections, not tracks.

## Plan

Decided 2026-10-13. Payload undercounts what delivery costs, and by how much
depends on the workload. Measured wire/payload in moq.pro's bench:

| Workload | Wire/payload |
|---|---|
| Video | 1.04x |
| Audio | 1.23x |
| Small chat-sized messages | 1.29-1.56x |
| Announce-only | 14.4x |

A provider bills the wire, so a relay operator needs per-root wire bytes to
see cost per tenant.

- **`Presence`, not `Traffic`.** `Presence` is already per (tier, root) and
  per session. `Traffic.bytes` stays payload, and its docs say so.
- **Named `wire_tx` / `wire_rx`.** Rejected: `bytes_sent`, which reads like
  payload beside `Traffic.bytes`.
- **Sampled on the stats tick.** Rejected: a once-only count at session end,
  which loses a crashed session and lags a long one.
- **One source registry for sessions and embedders.** An embedder registers
  a gateway connection's wire source against a (tier, root) on its stats
  handle, the same path the relay's own sessions use, like the existing
  `stats::Handle::refuse` API.

## Required

- [Presence wire counters](/quest/m2/wire-stats/presence.md) - QUIC,
  WebTransport, and WebSocket sessions and embedder sources count per
  (tier, root)
- [qmux byte counters](/quest/m2/wire-stats/qmux.md) - a WebSocket session
  reports its TCP bytes
- [moq-srt wire source](/quest/m2/wire-stats/srt.md) - an SRT connection
  reports its wire bytes
