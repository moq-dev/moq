# [M] TCP acceptor

## Goal

`moq-relay`'s listener accepts TLS-terminated HTTP, RTMP, and RTMPS on one
TCP port and yields classified connections, so an embedder routes RTMP to
`moq-rtmp` and everything else reaches the axum router as today. The relay
binary has no RTMP consumer and gains none here: it refuses a classified
RTMP connection with a log line, and the embedder that carries RTMP today
(moq.pro's edge) is the consumer.

## Plan

`moq-relay/src/listener.rs` grows the acceptor: peek the first byte; 0x16 is
TLS, accepted with the served identity, then the first decrypted byte is
peeked; 0x03 is RTMP, anything else is HTTP. A plaintext 0x03 is RTMP; a
plaintext ASCII method is HTTP. The acceptor yields
`Accepted::{Http(stream), Rtmp(stream)}` where the stream is a boxed
`AsyncRead + AsyncWrite` that may already be TLS. HTTP connections are pushed
into a channel that stands behind `axum_server::Server::from_listener`, so
the router and its ALPN list are untouched. RTMP connections are the
embedder's to hand to `moq_rtmp::Server::accept_stream`; upstream nothing
consumes them, so the relay logs and drops them unless a consumer is wired.

The accept loop never peeks or handshakes. Each accepted socket is spawned
onto its own task with a bounded read timeout; that task peeks, optionally
terminates TLS, and yields `Accepted`. An idle or slow client stalls only
its own task, so a peer that connects and sends nothing cannot block later
accepts. The peek is non-destructive on `TcpStream` and on the rustls
stream alike. Reuse the handshake bound `listen.timeout` already sets
(`moq_tokio::listen::Config::timeout`) rather than a new knob.

The TLS arm also carries `tls://` qmux, served today by its own `listen.tcp`
listener with TLS. It negotiates a moq ALPN in the TLS handshake, so split
the decrypted arm on ALPN: a moq ALPN goes to the qmux server, and HTTP
ALPNs (or none) go to the router.

Keep the accepted `TcpStream`'s handle reachable after it is boxed: the
relay already captures socket stats at accept for qmux (`SocketStats` in
`web.rs`), and [WebSocket bitrate caps](/quest/m2/rate-websocket.md) set
socket buffer sizes on the same socket.

Tests: an HTTP request, a WebSocket upgrade, a raw RTMP C0, and an RTMPS C0
against one listener each land in the right arm.

## Related

- [UDP demux](/quest/m2/one-port/udp-demux.md) - the UDP half
- [WebSocket bitrate caps](/quest/m2/rate-websocket.md) - sizes the kernel buffers of the socket this acceptor boxes
