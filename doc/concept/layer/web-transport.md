---
title: WebTransport
description: The layer that makes MoQ browser compatible
---

# WebTransport
[RFC9000](https://datatracker.ietf.org/doc/html/rfc9000) introduced QUIC and [RFC9114](https://datatracker.ietf.org/doc/html/rfc9114) introduced HTTP/3.

However, HTTP/3 is not the only way to use QUIC in a browser.
HTTP semantics can make things awkward as the client needs to initiate each request; there's no (good) way for the server to push live content.

WebTransport was created as an alternative to WebSockets, using QUIC instead of TCP.
It exposes more-or-less the same API as QUIC so it works great for MoQ.

- [Network Specification](https://www.ietf.org/archive/id/draft-ietf-webtrans-http3-14.html)
- [Browser Specification](https://www.w3.org/TR/webtransport/)

## Handshake
WebTransport actually uses HTTP/3 under the hood.
A `CONNECT` request is sent by the client to the server, indicating it wants to establish a WebTransport session.
There's also an optional `protocol` header similar to ALPN and WebSocket's subprotocol.

This sharing of a QUIC connection actually makes WebTransport a bit problematic.
HTTP/3 requests and other WebTransport sessions fight for resources, requiring extra management in the form of capsules.
Both sides negotiate the maximum number of WebTransport sessions that can be established on the same QUIC connection; my recommendation is to set it to 1.

## API
There are some subtle differences between the WebTransport API and the QUIC API.
Most of the time, you can assume they are the same, but there are some differences:

1. **Stream ID Gaps**: Because WebTransport shares a connection, we can't always tell if a RESET stream was for a WebTransport session or a HTTP/3 request.
2. **Smaller Errors**: WebTransport supports a smaller set of error codes than QUIC.
3. **Browser API**: The [W3C API](https://developer.mozilla.org/en-US/docs/Web/API/WebTransport_API) is more limited than most QUIC libraries.

Shameless plug: use my [web-transport](/rs/crate/web-transport) libraries for Rust.
It implements most of the Quinn API so you can support both QUIC and WebTransport with minimal changes.
