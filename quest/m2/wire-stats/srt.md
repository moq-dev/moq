# [S] moq-srt wire source

## Goal

moq-srt gives its embedder each SRT connection's cumulative wire bytes, both
directions, for publish and subscribe. These are the SRT packets (headers,
control packets, and retransmits included) without the IP and UDP headers,
the same definition as QUIC's `udp_tx`.

## Plan

- srt-protocol's `SocketStatistics` carries `tx_all_bytes` /
  `rx_all_bytes`, which include control packets but also a fixed 20 B IPv4 +
  8 B UDP estimate per packet. Subtract that estimate per packet so the
  count is SRT packet bytes. The stats arrive on the socket's
  `statistics()` watch.
- `serve_subscribe` and the publish path own the socket today and expose
  nothing. Hand the embedder a cumulative `(tx, rx)` source per connection,
  in the shape [Presence wire counters](/quest/m2/wire-stats/presence.md)
  registers.
- Target the shared-socket integration that drives `srt-protocol` directly,
  not `srt-tokio` socket handling.
- Test: an SRT pull's reported tx covers at least the TS payload it sent.

## Required

- [Shared SRT socket](/quest/m2/one-port/srt-demux.md) - replaces today's srt-tokio ownership with the direct srt-protocol integration this source targets
