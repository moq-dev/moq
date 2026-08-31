# Custom QUIC: the quiche fork

## Goal

Own the QUIC transport the relay runs on. A maintained fork of
[cloudflare/quiche](https://github.com/cloudflare/quiche) carries the
media-aware features vanilla quiche cannot express: per-viewer delivery
telemetry, bandwidth probing that sends useful bytes instead of padding,
congestion control tuned for real-time media, and a rustls crypto backend.

The backend itself already exists and is NOT this questline's work:
[moq#2875](https://github.com/moq-dev/moq/issues/2875) built `rs/moq-uring`,
which drives sans-IO quiche behind `web_transport_trait::poll` on a
thread-per-core io_uring runtime, with the relay integration merged behind
`--runtime-io-uring`. This questline is the layer on top: each quest lands its
fork half in the moq-dev/quiche repository and its adoption half here.

Out of scope: the tokio-quiche path (`web-transport-quiche`) stays on
boringssl until it retires naturally; no MoQ wire changes anywhere.

## Plan

Motivation, in order: media-aware transport features quinn's and quiche's
public APIs cannot express, relay efficiency (the io_uring epic's own chase,
still being measured), and ownership, so future experiments (FEC, multipath) do not
wait on an upstream roadmap. Pieces that come out clean (rustls, stats hooks)
are offered upstream to cloudflare/quiche; the fork never depends on them
being accepted.

### Wart ledger

Known quiche-backend gaps, promoted to quests only when one becomes real:

- TLS hot-reload: the quiche listener snapshots TLS material at build time;
  quinn/noq reload via `notify`. Erased by [rustls](/quest/m2/quiche/rustls.md).
- Platform certificate verification: the quiche backend cannot use
  `rustls-platform-verifier`, so iOS/Android fail closed. Erased only when a
  mobile-capable client consumer adopts the fork's rustls feature (the
  rustls quest's boundary explains why it can't).
- QUIC-LB connection-id encoding: quinn and noq have it; the quiche backend
  warns and ignores. Matters if a deployment ever load-balances QUIC below
  GeoDNS.
- qlog is one file per connection (quinn: per endpoint, tagged by group).
- 0-RTT is unused on every backend; the server disables resumption whenever
  client authorization can change (mTLS roots or a peer set configured).

## Quests

- [rustls crypto backend](/quest/m2/quiche/rustls.md) - the fork handshakes
  with rustls instead of boringssl on the moq-uring path, erasing the TLS
  hot-reload wart
- [Per-stream ACK stats](/quest/m2/quiche/ack-stats.md) - each subscriber's
  delivered-vs-queued progress and cwnd are exposed, giving per-viewer latency
  visibility to the QoS line
- [Probe by early retransmission](/quest/m2/quiche/probe.md) - egress probes
  for headroom by retransmitting in-flight data early instead of sending
  PADDING, feeding the existing PROBE and publisher rate adaptation

## Related

- [GCC egress experiment](/quest/m3/quiche-gcc.md) - a measured verdict on
  WebRTC-style delay-based congestion control for media egress versus BBRv2
- [FEC experiment](/quest/m3/quiche-fec.md) - a measured verdict on
  transport-level forward error correction for loss-recovery latency
- [Backlog counters](/quest/m2/qos/backlog.md) - the QoS consumer of the
  telemetry this questline exposes; it names the same trait hook ack-stats
  implements
