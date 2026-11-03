# Migrating @moq/qmux

How to update your code for each breaking change, newest first.

## Unreleased

### Accepted bidi streams hold credit until both halves end

A peer's bidirectional stream returns its `MAX_STREAMS` credit only once it is accepted and both halves have ended, as in QUIC. Close or abort the writable of every accepted bidi stream, even one you never write to, or the peer eventually runs out of streams to open.

### Only QMux draft-00, 01 and 02 are supported

The legacy `webtransport` subprotocol and implicit fallback are removed. Offer a supported `qmux-NN` or `qmux-NN.<application>` subprotocol. Reject upgrades when `selectSubprotocol` returns `undefined`; accepted sockets without a supported negotiated value now reject `ready` and `closed`. Pass `{ protocol }` to `Session.accept` if the host does not expose it on the socket.

### Receive windows and explicit disabled values

Rename `maxData` to `receiveWindow`. Replace `maxStreamDataBidiLocal`, `maxStreamDataBidiRemote` and `maxStreamDataUni` with one `streamReceiveWindow` for every stream. If those limits differed, choose the single receive budget appropriate for every stream; directional stream counts remain separate.
Replace bigint milliseconds in `maxIdleTimeout` with number milliseconds in `idleTimeout`, or `null` to disable it. Durations must be positive safe integers; zero is refused. Set `maxDatagramFrameSize` to a positive bigint or `null` to disable it, replacing zero. Removed or unknown config keys and malformed values are refused before transport setup; an optional field set to `undefined` uses its default.

### `@moq/web-socket-stream` is folded into `@moq/qmux`

`@moq/qmux` is now built and published from [moq-dev/moq](https://github.com/moq-dev/moq/tree/main/js/qmux) and no longer depends on `@moq/web-socket-stream`, which is deprecated.
Import the socket types `Session` accepts, `WebSocketLike` and `WebSocketStreamLike`, from `@moq/qmux`.
The standalone `WebSocketStream` ponyfill, `openWebSocketStream`, and its global `install()` are not exported; keep `@moq/web-socket-stream@0.1` pinned if you used them directly, or use the native `WebSocketStream`.

### `serverCertificateHashes` throws

`new Session(url, { serverCertificateHashes })` with a non-empty list now throws instead of warning and connecting without pinning, since WebSocket can't enforce it.
Omit the option when falling back to WebSocket; the platform's certificate validation applies there.
