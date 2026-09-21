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
stream alike.

Tests: an HTTP request, a WebSocket upgrade, a raw RTMP C0, and an RTMPS C0
against one listener each land in the right arm.

## Related

- [UDP demux](/quest/next/one-port/udp-demux.md) - the UDP half
