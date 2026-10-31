# [M] qmux frames WebSocket itself

## Goal

qmux implements the RFC 6455 subset it uses, and tungstenite and
tokio-tungstenite leave the dependency tree, but only if the stream
transport bench (see the [epic](/quest/m2/perf/stream/README.md)) attributes a
material share of WebSocket CPU to tungstenite once the
[driver](/quest/m2/perf/stream/driver.md) has landed. If the bench shows no such share, delete this quest.

## Plan

Gate: re-run the bench after the driver lands. If the profile no longer
shows tungstenite's read-buffer zero-fill, its `out_buffer` copy, or its
memmove after a partial write as a real share, abandon the quest.

The subset to implement, from the 2026-10-22 audit:

- **Frames.** 7-, 16-, and 64-bit lengths. Reject unmasked client frames and
  non-zero RSV bits.
- **Opcodes.** Binary is one qmux record. Continuation frames are reassembled
  up to `max_record_size`. Ping gets a Pong. Close is echoed, then the session
  shuts down. Text is a protocol violation. Control frames are at most 125
  bytes and never fragmented.
- **Inbound.** Unmask in place, then slice frames straight into the driver's
  read buffer.
- **Outbound.** Encode the frame header in front of each record in the
  driver's write buffer. The client masks in place.
- **Server handshake.** None: the relay keeps hyper's upgrade, so this covers
  only the moq-tokio standalone listener (`Sec-WebSocket-Key` to SHA-1/base64
  accept, `Version: 13`, subprotocol pick, 400 when nothing matches).
- **Client handshake.** One GET with `Sec-WebSocket-Protocol`, then parse the
  101 response.

A maintained crate was weighed against this. The codec is small, sits on the
hot path, and must write into the driver's buffers, so qmux owns it.

Public API: the `tokio_tungstenite` and `tungstenite` re-exports and the
`tungstenite` types in `Client::with_config`, `Client::with_connector`,
`Upgraded`, and `Error::WebSocket` go away. The Impact bullet says how to
migrate in `qmux` and `moq-tokio`. The existing qmux interop tests (the `qmux06`
dev-dependency) and the browser path in `js/qmux` must keep passing
unchanged.

## Required

- [Driver](/quest/m2/perf/stream/driver.md) - the codec writes into the driver's buffers, and the bench re-runs after it
