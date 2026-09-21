# [M] Data channel transport

## Goal

`@moq/p2p` exports a transport with the DOM `WebTransport` shape over an
`RTCPeerConnection` that `connect` and `accept` in `@moq/net` consume without
change: qmux over one reliable, ordered data channel, one record per message.
The moq-lite draft gains the binding.

## Plan

`@moq/qmux` and `MockTransport` are the models: the contract is the DOM
interface, and `transportOf` in `js/net` gains a `webrtc` arm so stats and
logs name it. The negotiated moq ALPN is handed to the transport by the
caller (signaling agrees it before the peer connection exists), because
`accept()` reads `transport.protocol` to pick the SETUP flavor.

A small object with the `WebSocketStream` shape (`opened`, `closed`, `close`)
wraps the channel and is handed to the qmux `Session` with the protocol
override, so the WebSocket binding's code path is reused rather than
duplicated. The readable enqueues one `Uint8Array` per message with
`binaryType` set to `arraybuffer`; the writable calls `send` per record and
derives `desiredSize` from `bufferedAmount` against
`bufferedAmountLowThreshold`. `max_record_size` defaults to 16 KiB and is
clamped to the negotiated `maxMessageSize`; a record that would exceed it is
split at a frame boundary, never chunked mid-frame. The channel is created
with `ordered: true` and no retransmit limit unless signaling already
agreed unordered; `ordered` is a constructor option so
[unordered qmux](/quest/next/p2p/unordered.md) flips it without a second
transport. The flip is decided from the roster before `createDataChannel`,
never from the first qmux record.

Tests: framing, the record-size clamp, and backpressure run under `bun test`
against an in-memory channel pair; the real thing runs under the Playwright
driver in `test/p2p`, two peer connections in one page, completing SETUP
through `accept()` and `connect()`.

Draft: `draft-lcurley-moq-lite.md` Transports gains a row for qmux over
RTCDataChannel (one record per message, no datagrams, like WebSocket), and
the sentence declaring P2P out of scope goes. `just drafts check` passes.

## Related

- [Signaling and policy](/quest/next/p2p/signal.md) - supplies the peer connection and the agreed ALPN
- [Native data channel transport](/quest/next/p2p/webrtc.md) - the same binding in Rust
