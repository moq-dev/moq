# @moq/qmux

A [WebTransport](https://developer.mozilla.org/en-US/docs/Web/API/WebTransport_API) implementation using WebSockets as the underlying transport with [QMux](https://www.ietf.org/archive/id/draft-ietf-quic-qmux-02.html) (draft-ietf-quic-qmux-02, negotiating down to draft-01 and draft-00) framing: a polyfill in browsers, and a server on any runtime that can accept a WebSocket.

QMux brings QUIC's multiplexed streams and flow control to reliable, ordered byte-stream transports like WebSockets. This allows WebTransport applications to seamlessly fall back when QUIC/UDP is blocked by network middleboxes.

## Install

```bash
npm install @moq/qmux
```

## Usage

Use as a drop-in `WebTransport` replacement:

```ts
import Session from "@moq/qmux"

const transport = new Session("https://example.com/endpoint")
await transport.ready

const stream = await transport.createBidirectionalStream()
```

### Datagrams

Use `transport.datagrams.readable` to receive packets and `.writable` to send them.
`.maxDatagramSize` reports the negotiated payload limit; oversized or unsupported writes are dropped.
The datagram transport is created and managed by the session.

Set `.incomingHighWaterMark` to change the live receive queue limit (1024 packets by default).
New arrivals are dropped when the queue reaches that limit. Lowering it preserves queued packets
until they are read. Values below 1 are clamped to 1; negative values and `NaN` throw `RangeError`.
The value must be a number.

Expiry and changes to the outbound queue policy are unsupported. Assigning anything except `null`
to `.incomingMaxAge` or `.outgoingMaxAge`, or anything except 1024 to `.outgoingHighWaterMark`,
throws `NotSupportedError`.

### Servers

`Session.accept` runs the other role, over a WebSocket your server has already accepted. The host
performs the upgrade, so it — not this library — chooses the subprotocol; `selectSubprotocol` picks
the value to accept from what the client offered, and the session reads the wire-format version back
off the socket.

```ts
import Session, { selectSubprotocol } from "@moq/qmux"

Deno.serve((req) => {
	const protocol = selectSubprotocol(req.headers.get("sec-websocket-protocol"), {
		protocols: ["moq-lite-04"],
		versions: { "moq-lite-04": null },
	})
	if (!protocol) return new Response("no supported protocol", { status: 400 })

	const { socket, response } = Deno.upgradeWebSocket(req, { protocol })
	const session = Session.accept(socket)
	handle(session) // a WebTransport, same as the client side
	return response
})
```

Any already-accepted `WebSocket` works — `Deno.upgradeWebSocket`, Node's `ws`, or a
`WebSocketStream`. The session refuses missing or unsupported subprotocols. Reject the upgrade
when `selectSubprotocol` returns `undefined`. If your host doesn't expose the
negotiated value on the socket, pass it explicitly as `{ protocol }`.

The result is an ordinary `WebTransport`: the only difference from a client session is which half of
the stream-id space each side owns.

### Detecting a dropped session

`closed` follows the WebTransport contract:

- **Fulfills** with `{ closeCode, reason }` on a graceful end: either side called `close()`, which
  puts an `APPLICATION_CLOSE` (`0x1d`) on the wire.
- **Rejects** on an abnormal end: a `CONNECTION_CLOSE` (`0x1c`) arrived — the transport variant a
  peer sends when *it* caught a protocol violation — or no close frame arrived at all, because the
  socket dropped or the peer went idle. This endpoint rejects the same way when it catches the peer
  violating the protocol, and sends a `CONNECTION_CLOSE` so the peer rejects too.

```ts
try {
	const info = await transport.closed
	console.log("closed gracefully", info.closeCode, info.reason)
} catch (err) {
	// err is a WebTransportError-shaped SessionError: err.source === "session"
	console.warn("session dropped, reconnecting", err)
}
```

Don't reach for `closeCode` to tell the two apart — close codes are application-defined, so a
graceful close may carry any value, including one that looks like a violation, and an app closing
with `1006` is indistinguishable from a dropped socket. Which frame arrived is the signal, and the
settled state is how you read it.

### Stream reset codes

A reset carries an application error code in both directions, like native WebTransport:

```ts
import { StreamError } from "@moq/qmux"

// Send RESET_STREAM with code 26.
await stream.writable.abort(new StreamError(26))

// Receive one: the code is a field, not text buried in the message.
try {
	await reader.read()
} catch (err) {
	// err.source === "stream"
	console.warn("reset by peer with code", err.streamErrorCode)
}
```

The outgoing code comes off the reason passed to `abort()` / `cancel()`, so any `WebTransportError`
(native or the exported `StreamError`) sends its `streamErrorCode` and anything else sends 0.
`STOP_SENDING` works the same way, erroring the sender's writable with the code the peer chose.

### Polyfill

Install as a global `WebTransport` polyfill:

```ts
import { install } from "@moq/qmux"

// Only installs if native WebTransport is unavailable
install()

// Now use the standard WebTransport API
const transport = new WebTransport("https://example.com/endpoint")
```

## License

Licensed under either of [Apache License, Version 2.0](../../LICENSE-APACHE) or [MIT license](../../LICENSE-MIT) at your option.
