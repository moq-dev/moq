---
title: WebSocket
description: A TCP fallback for corporate nerds who block QUIC, and Safari users.
---

# WebSocket
I'm bored of writing so much documentation.
Check out the next page on [moq-lite](/concept/layer/moq-lite) instead; it's way more interesting.

Here's some AI slop for now:

## AI SLOP
WebSocket is a TCP fallback for when QUIC/WebTransport isn't available.
This happens more often than you'd think: corporate firewalls love blocking UDP, and Safari didn't support WebTransport until 26.4.

We use a thin polyfill called [web-transport-ws](https://github.com/moq-dev/web-transport/tree/main/web-transport-ws) that emulates the WebTransport API over a WebSocket connection.
It multiplexes streams using a simple binary framing protocol, so the rest of the stack doesn't need to know or care that it's running over TCP.

### How It Works
When establishing a connection, the client races QUIC/WebTransport against WebSocket in parallel.
WebSocket wins the race when UDP is blocked or WebTransport isn't supported.

The polyfill converts the URL scheme (`https://` → `wss://`, `http://` → `ws://`) and connects with the `webtransport` subprotocol.
Once connected, it provides the same API as WebTransport: bidirectional streams, unidirectional streams, and connection management.

Obviously it won't perform as well as QUIC during congestion — everything gets head-of-line blocked over TCP.
But it's better than not working at all.

### Streams
Stream IDs are encoded as variable-length integers with the lower 2 bits indicating the stream type:

| Bit 0 | Bit 1 | Type |
|-------|-------|------|
| 0 | 0 | Client-initiated bidirectional |
| 1 | 0 | Server-initiated bidirectional |
| 0 | 1 | Client-initiated unidirectional |
| 1 | 1 | Server-initiated unidirectional |

This matches the [QUIC stream ID scheme](https://datatracker.ietf.org/doc/html/rfc9000#section-2.1).

### Framing
Each WebSocket binary message contains a single frame.
The frame type is identified by the first byte, borrowing values from the QUIC spec:

#### `STREAM` (0x08) / `STREAM_FIN` (0x09)
Carries data for a stream. `STREAM_FIN` indicates the final data on the stream.

```text
+--------+-----------+---------+
| Type   | Stream ID | Payload |
| 1 byte | VarInt    | ...     |
+--------+-----------+---------+
```

No length field is needed since WebSocket already provides message boundaries.

#### `RESET_STREAM` (0x04)
Abruptly terminates the sending side of a stream with an error code.

```text
+--------+-----------+------------+
| Type   | Stream ID | Error Code |
| 1 byte | VarInt    | VarInt     |
+--------+-----------+------------+
```

#### `STOP_SENDING` (0x05)
Requests the peer stop sending on a stream. The peer should respond with a `RESET_STREAM`.

```text
+--------+-----------+------------+
| Type   | Stream ID | Error Code |
| 1 byte | VarInt    | VarInt     |
+--------+-----------+------------+
```

#### `APPLICATION_CLOSE` (0x1d)
Gracefully closes the connection with an error code and reason.

```text
+--------+------------+--------+
| Type   | Error Code | Reason |
| 1 byte | VarInt     | UTF-8  |
+--------+------------+--------+
```

### VarInt Encoding
Variable-length integers use the [QUIC VarInt encoding](https://datatracker.ietf.org/doc/html/rfc9000#section-16).
The first two bits indicate the length:

| Prefix | Length | Max Value |
|--------|--------|-----------|
| `00`   | 1 byte | 63 |
| `01`   | 2 bytes | 16,383 |
| `10`   | 4 bytes | 1,073,741,823 |
| `11`   | 8 bytes | 4,611,686,018,427,387,903 |

### Limitations
Let's be real: this is a polyfill, not a replacement.

- **Head-of-line blocking**: TCP delivers bytes in order, so a lost packet stalls everything. The whole point of QUIC streams is to avoid this.
- **No prioritization**: The sender can't choose which stream gets bandwidth first. With QUIC, we prioritize new video over old video — over WebSocket, they're all stuck in line.
- **No partial reliability**: You can reset a logical stream, but the bytes already in the TCP buffer will still be delivered (and block everything behind them).

It's good enough for low-congestion scenarios and ensures your app works everywhere.
For the best experience, use a browser that supports WebTransport.

### Future
There is a [WebTransport over HTTP/2 draft](https://datatracker.ietf.org/doc/html/draft-ietf-webtrans-http2-13) that could replace this WebSocket polyfill.
It's too little, too late for now, but maybe one day.
In the meantime, WebSocket is the only reliable fallback for Safari and older iOS devices.
