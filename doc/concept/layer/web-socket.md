---
title: WebSocket
description: A TCP fallback when QUIC or WebTransport is unavailable
---

# WebSocket

WebSocket is the compatibility transport for networks or clients that cannot
use QUIC. It keeps the same stream-oriented interface as WebTransport, so the
moq-lite layer does not need a separate protocol path.

## Connection behavior

The client can race WebTransport and WebSocket connections and keep the first
one that succeeds. The WebSocket path changes `https://` to `wss://` (or
`http://` to `ws://`) and negotiates a QMux subprotocol for the MoQ version.

The [QMux implementation](https://github.com/moq-dev/web-transport/tree/main/rs/qmux)
multiplexes bidirectional and unidirectional logical streams over the WebSocket
connection. Its frame types cover stream data, stream completion, reset,
stop-sending, and connection close.

## Tradeoffs

WebSocket improves reach, but it cannot reproduce QUIC's behavior under loss:

- TCP loss blocks every logical stream behind the missing bytes.
- Stream priority cannot change the order of bytes already queued in TCP.
- Resetting a logical stream does not remove its buffered bytes from the TCP
  connection.

These differences matter most during congestion, when a delayed old media
group can block newer groups. Prefer WebTransport when it is available and keep
WebSocket as a fallback.

## Related

- [QUIC](/concept/layer/quic)
- [WebTransport](/concept/layer/web-transport)
- [moq-lite](/concept/layer/moq-lite)
