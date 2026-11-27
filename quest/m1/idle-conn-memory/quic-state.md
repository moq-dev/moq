# [M] An established idle QUIC connection drops what it no longer needs

## Goal

Once the handshake is confirmed, a moq-quic server connection no longer
holds handshake buffers, handshake-space keys, or rustls handshake state,
and an idle connection's sent-packet tracking holds about what its few
in-flight packets need rather than a 64-slot chunk.

## Plan

The 2026-11-27 idle profile ([epic](/quest/m1/idle-conn-memory/README.md))
put 13.5 KiB per connection in TLS, 14.3 KiB in moq-quic and
`moq-congestion`, and a 10 KiB `moq_tokio::quic::driver::ConnectionInner`
allocation that holds moq-quic's `Connection` inline. Sites to check:

- `TlsSession::read_handshake` (`rs/moq-quic/src/crypto/rustls.rs`)
  resizes a receive buffer to 2.6 KiB during the handshake and keeps it.
- rustls `TlsSession` and `ServerConnectionData` (2 KiB) stay live after the
  handshake.
- Packet and header-protection keys built in `Connection::write_crypto` and
  by `DirectionalKeys::new` (about 3.5 KiB) are still live on an established
  connection. RFC 9001 §4.9.2 lets a server discard handshake keys once the
  handshake is confirmed; confirm moq-quic does and that these are 1-RTT
  keys, which stay (the current and next generation, for key updates).
- `connection::slots::Slots` allocates a `Box<[Option<T>; 64]>` chunk of
  about 8 KiB for the first in-flight packet, so a keep-alive alone holds
  one: 4.2 KiB per connection on average. A smaller first chunk, or chunks
  sized to the packets in flight, keeps the O(1) lookups.
- The outgoing datagram `Schedule` B-tree (1.4 KiB), the stream waits table
  (1 KiB) and `moq_congestion::Lost` growth (1.2 KiB) on a connection that
  sends no datagrams or streams.

Measure `ConnectionInner`'s size by field before changing it: whatever of
the 10 KiB is fixed per-connection state stays, and the rest moves behind a
box allocated only when used.
