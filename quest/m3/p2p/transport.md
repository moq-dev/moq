# [M] Data channel transport

## Goal

`@moq/p2p` exports a transport with the DOM `WebTransport` shape over an
`RTCPeerConnection`, in two modes, that `Connection.connect` and
`Connection.accept` in `@moq/net` consume without change. The moq-lite draft
gains the two bindings.

## Plan

`@moq/qmux` and `MockTransport` are the models: the contract is the DOM
interface, and `transportOf` in `js/net` gains a `webrtc` arm so stats and
logs name it. The negotiated moq ALPN is handed to the transport by the caller
(signaling agrees it before the peer connection exists), because `accept()`
reads `transport.protocol` to pick the SETUP flavor.

Mode `qmux`: one reliable ordered channel. A small object with the
`WebSocketStream` shape (`opened`, `closed`, `close`) wraps the channel and is
handed to `Session` or `Session.accept` with the protocol override; the
readable enqueues one `Uint8Array` per message with `binaryType` set to
`arraybuffer`, the writable calls `send` per record and derives `desiredSize`
from `bufferedAmount` against `bufferedAmountLowThreshold`. Records above the
negotiated `maxMessageSize` (256 KiB in Chrome) are chunked.

Mode `stream`: one channel per moq stream, opened with a `protocol` string
naming the direction so a unidirectional stream is a channel only one side
writes. The first channel is the SETUP stream. `finish` closes the channel;
`reset` and `stop` carry their codes on one reserved control channel, since
DCEP has no error code. A separate `ordered: false, maxRetransmits: 0` channel
carries moq-lite datagrams and reports the negotiated message size as
`maxDatagramSize`. Chrome frees a stream id only after the close event, so the
transport counts open channels and parks `createUnidirectionalStream` near
the 1024 cap rather than failing.

Tests: framing, chunking, and the control channel run under `bun test`
against an in-memory channel pair; the real thing runs under the Playwright
driver in `test/p2p`, two peer connections in one page over both modes,
completing SETUP through `accept()` and `connect()`.

Draft: `draft-lcurley-moq-lite.md` Transports gains rows for qmux over
RTCDataChannel (one record per message, no datagrams, like WebSocket) and
RTCDataChannel per stream (with the control and datagram channels), and the
sentence declaring P2P out of scope goes. `just drafts check` passes.

## Related

- [Signaling over the relay](/quest/m3/p2p/signal.md) - supplies the peer connection and the agreed ALPN
- [Native data channel transport](/quest/m3/p2p/webrtc.md) - the same two mappings in Rust
