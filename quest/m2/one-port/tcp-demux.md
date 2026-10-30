# [M] TCP acceptor: tls:// on the web port

## Goal

The relay's HTTPS listener also carries `tls://` qmux, so one TCP port serves
TLS-terminated HTTP, RTMP, RTMPS, and `tls://` MoQ. The HTTP, RTMP, and RTMPS
split already landed: `listener::classify` peeks each connection on its own
task, and an embedder takes RTMP from `Relay::rtmp`.

## Plan

`tls://` is served today by its own `listen.tcp` listener with TLS. It
negotiates a moq ALPN in the TLS handshake, so split the decrypted arm on ALPN:
a moq ALPN goes to the qmux server and an HTTP ALPN to the router. A connection
with no ALPN keeps the decrypted-byte sniff, since RTMPS clients negotiate none.
Selecting the server config from the ClientHello (rustls' `LazyConfigAcceptor`)
lets the moq ALPN serve the listen TLS identity while HTTP keeps
`web.https.cert`.

Open: how an operator turns this on, and what becomes of the separate
`listen.tcp` TLS bind. Asked in the PR that landed the HTTP and RTMP split.

Decided 2026-10-08: under one acceptor, the client-certificate request from
[mTLS on tls://](/quest/m2/tls-listener-mtls.md) is optional (never required)
and sits on the moq-ALPN TLS config only, so browsers and RTMPS clients on
the same port are never asked for one. Whichever lands second keeps `tls://`
client-certificate verification and the `MtlsUnsupported` lift working.

Keep the accepted `TcpStream` reachable (`listener::Stream::tcp`): qmux
reads socket stats from it, and
[WebSocket bitrate caps](/quest/m2/rate-websocket.md) set socket buffer sizes
on the same socket.

Tests: a `tls://` qmux client against the HTTPS listener lands in the qmux
server, beside the HTTP and RTMP cases already covered.

## Related

- [UDP demux](/quest/m2/p2p/udp-demux.md) - the UDP half
- [mTLS on tls://](/quest/m2/tls-listener-mtls.md) - client-certificate verification on the `tls://` arm this acceptor carries
- [WebSocket bitrate caps](/quest/m2/rate-websocket.md) - sizes the kernel buffers of the socket this acceptor boxes
