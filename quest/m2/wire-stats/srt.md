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
  count is SRT packet bytes. Today they arrive on `srt-tokio`'s
  `SrtSocket::statistics()` stream.
- `serve_subscribe` and the publish path own the socket today and expose
  nothing. Update a `moq_net::transport::Wire` per connection
  from each statistics update and hand it to the embedder, which registers it
  with `moq_net::stats::Handle::wire`.
- Read `srt-tokio`'s statistics now rather than waiting on one port
  (decided 2026-10-20, maintainer, because the source needs no socket ownership
  change). When [SRT on the shared socket](/quest/m2/one-port/srt-demux.md)
  replaces `srt-tokio`, it reads the same `SocketStatistics` from
  `srt-protocol` and keeps this source.
- Test: an SRT pull's reported tx covers at least the TS payload it sent.

## Related

- [SRT on the shared socket](/quest/m2/one-port/srt-demux.md) - carries this source over when it replaces `srt-tokio`
