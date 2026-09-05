---
title: Transport
description: QUIC, WebTransport, the WebSocket fallback, raw QUIC, stream listeners, and iroh
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
client; the relay enables it with `[web.https]`.

## Raw QUIC (native)

Native clients dial the relay directly with the MoQ ALPN, skipping the HTTP/3
handshake. The URL path and `?jwt=` token travel in the MoQ `SETUP` message
instead of a `CONNECT` request, so the server sees the same thing either way.
`moq-native` picks this automatically for `https://` URLs; use `moql://` or
`moqt://` to force it.

## TCP and Unix sockets (local workers)

For trusted processes on the same host, the relay can accept the qmux wire
format over plain TCP or a Unix socket with no TLS. Gateways and stats
publishers use this to avoid UDP and certificate setup. Both listeners
authenticate through the same JWT and public-access rules as QUIC, and the Unix
socket can additionally allowlist the connecting process by uid, gid, or pid.
See [stream listeners](/bin/relay/auth#stream-listeners).

## iroh (peer-to-peer, experimental)

[iroh](https://www.iroh.computer/) swaps "dial a hostname" for "dial a public
key". Two native MoQ endpoints on the same network can exchange media with no
relay, no DNS, and no certificate authority: the endpoint's key is its address
and its TLS identity.

```bash
moq --iroh-enabled --iroh-disable-relay \
    --client-connect "iroh://<endpoint-id>/anon" --broadcast cam.hang play
```

Keep it on the local network. Across the internet iroh falls back to an n0
relay that forwards opaque packets, which is a TURN-shaped hop that cannot
cache or fan out; a [MoQ relay](/bin/relay/) is the right tool there.
`--iroh-disable-relay` pins traffic to direct addresses but also removes hole
punching. Discovery still publishes the endpoint's addresses to n0's DNS
whichever way it is configured. Browsers can't use iroh at all: WebTransport
gives a page a connection, not an endpoint.

The relay opts in with `[iroh] enabled = true` and a persisted `secret` so the
endpoint id survives restarts. See the [config reference](/bin/relay/config#iroh).
