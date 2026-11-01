---
title: Transport
description: QUIC, WebTransport, the WebSocket fallback, raw QUIC, stream listeners, WebRTC data channels, and iroh
---

# Transport

MoQ runs on QUIC. Every other transport on this page is a way to get a
QUIC-shaped connection (independent streams, optional datagrams) from a
particular environment.

## Why QUIC

HTTP/1 serialized requests; HTTP/2 multiplexed them but kept one TCP byte
stream, so one lost packet stalled everything. RTMP, HLS, and SRT have the same
head-of-line problem: old frames block new ones during congestion, and latency
climbs. QUIC ([RFC 9000](https://datatracker.ietf.org/doc/html/rfc9000)) gives
each stream its own ordering and flow control, so a lost packet only holds
back the stream it carried, plus:

- **Partial reliability.** A stream can be reset mid-flight. MoQ resets a group once it is too old to matter.
- **Prioritization.** The sender decides which stream's packet goes next, so new groups and audio starve old video rather than the other way around.
- **Encryption, congestion control, and connection migration** for free, from a library every HTTP/3 stack already ships.

"Just use UDP" is the road WebRTC and SRT took, and each had to reinvent all
of that. MoQ spends its effort on media instead.

## WebTransport (browsers)

[WebTransport](https://www.w3.org/TR/webtransport/) exposes QUIC streams to a
web page over an HTTP/3 `CONNECT`. Chrome and Edge (97+) and Firefox (114+)
support it. The TypeScript client is pickier than the browsers: it uses
WebTransport on Firefox only from 153 (earlier releases allow too few incoming
streams) and keeps Safari on the WebSocket fallback, since WebKit bugs stall
long sessions even though Safari 26.4 ships the API. Native
clients can use WebTransport too, but usually skip it.

## WebSocket fallback

Where UDP is blocked or WebTransport is missing, clients race a WebSocket
connection (`wss://`) and keep whichever wins. A small multiplexer,
[qmux](/draft/qmux-websocket), carries MoQ streams over the socket. It works
everywhere but cannot escape TCP head-of-line blocking, so priority and resets
only help once bytes leave the TCP queue. The fallback is automatic in every
client; the relay serves it on its `[web.http]` or `[web.https]` listener. A session that lands on
WebSocket stays there until it ends; the next connect races again.

## Raw QUIC (native)

Native clients dial the relay directly with the MoQ ALPN, skipping the HTTP/3
handshake. The URL path and any `?jwt=` token travel in the MoQ `SETUP` message
instead of a `CONNECT` request, so the server sees the same thing either way.
On every transport, a SETUP token (moq-lite-07-wip and moq-transport) can carry
the credential in place of the query.
`moq-tokio` dials WebTransport for `https://` URLs; use `moql://` or `moqt://`
for raw QUIC. A browser cannot dial raw QUIC, so `@moq/net` reaches a `moqt://`
or `moql://` URL over WebTransport at the same host, port, and path.

## TCP and Unix sockets (local workers)

For trusted processes on the same host, the relay can accept the qmux wire
format over plain TCP or a Unix socket with no TLS. Gateways and stats
publishers use this to avoid UDP and certificate setup. Both listeners
authenticate through the same JWT and public-access rules as QUIC, and the Unix
socket can additionally allowlist the connecting process by uid, gid, or pid.
See [stream listeners](/bin/relay/auth#stream-listeners).

## WebRTC data channels (experimental)

The private workspace P2P transport is exercised by `test/p2p`; it is not a
published package. Applications in this checkout that negotiate a browser
`RTCPeerConnection` can pass an
[`@moq/p2p` transport](https://github.com/moq-dev/moq/tree/main/js/p2p) to
`@moq/net`'s connect or accept. One reliable ordered data channel carries qmux
records, with no datagrams. The application supplies signaling and ICE and
keeps ownership of the peer connection; closing the transport closes only its
channel. SCTP ordering means one lost packet can hold back every stream.

Native peers can opt into `moq-tokio`'s internal `webrtc` feature, which runs its own ICE agent:
host candidates on every interface, server-reflexive candidates from the STUN
servers it is given, and mDNS resolution of a browser's `.local` candidates.
The application still carries the offer, answer, and candidates; the offering
peer connects as the MoQ client and the answering peer accepts.

## iroh (peer-to-peer, experimental)

[iroh](https://www.iroh.computer/) swaps "dial a hostname" for "dial a public
key". Two native MoQ endpoints on the same network can exchange media with no
relay, no DNS, and no certificate authority: the endpoint's key is its address
and its TLS identity. Build `moq` from source with
`cargo build -p moq-cli --release --features iroh,play,openh264` for native playback;
published binaries, Docker images, and the Nix package leave iroh out.

```bash
moq --iroh-enabled --iroh-disable-relay \
    --connect "iroh://<endpoint-id>/anon" --broadcast cam.hang play
```

Keep it on the local network. Across the internet iroh falls back to an n0
relay that forwards opaque packets, which is a TURN-shaped hop that cannot
cache or fan out; a [MoQ relay](/bin/relay/) is the right tool there.
`--iroh-disable-relay` pins traffic to direct addresses but also removes hole
punching. Discovery still publishes the endpoint's addresses to n0's DNS
whichever way it is configured. Browsers can't use iroh at all: WebTransport
gives a page a connection, not an endpoint.

Build the relay from source with `cargo build -p moq-relay --release --features iroh`,
then opt in with `[iroh] enabled = true` and a persisted `secret` so the
endpoint id survives restarts. See the [config reference](/bin/relay/config#iroh).

## Custom Rust transports

`moq-net` runs over its own poll traits, so a custom transport implements
those directly. `moq-tokio` and `moq-uring` are the built-in
adapters. See the [moq-net API](/lib/rs/moq-net).
