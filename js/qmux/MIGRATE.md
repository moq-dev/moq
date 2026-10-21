# Migrating @moq/qmux

How to update your code for each breaking change, newest first.

## Unreleased

### `@moq/web-socket-stream` is folded into `@moq/qmux`

`@moq/qmux` is now built and published from [moq-dev/moq](https://github.com/moq-dev/moq/tree/main/js/qmux) and no longer depends on `@moq/web-socket-stream`, which is deprecated.
Import the socket types `Session` accepts, `WebSocketLike` and `WebSocketStreamLike`, from `@moq/qmux`.
The standalone `WebSocketStream` ponyfill, `openWebSocketStream`, and its global `install()` are not exported; keep `@moq/web-socket-stream@0.1` pinned if you used them directly, or use the native `WebSocketStream`.

### `serverCertificateHashes` throws

`new Session(url, { serverCertificateHashes })` with a non-empty list now throws instead of warning and connecting without pinning, since WebSocket can't enforce it.
Omit the option when falling back to WebSocket; the platform's certificate validation applies there.
