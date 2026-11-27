# [M] qmux frames WebSocket itself

## Goal

qmux implements the RFC 6455 subset it uses, and tungstenite and
tokio-tungstenite leave the dependency tree, but only if the stream
transport bench (see the [epic](/quest/m4/stream-transports/README.md)) attributes a
material share of WebSocket CPU to tungstenite now that qmux's `Driver`
owns the socket. If the bench shows no such share, delete this quest.

## Plan

Gate: re-run the bench against the driver's numbers in the epic README. If the profile no longer
shows tungstenite's read-buffer zero-fill, its `out_buffer` copy, or its
memmove after a partial write as a real share, abandon the quest.

The stopgap of shrinking tungstenite's `read_buffer_size` (2026-10-31, from
[moq-dev/moq#7086](https://github.com/moq-dev/moq/pull/7086)'s benchmark) landed
in 2026-11 with a copy of each message out of tungstenite's buffer: the read
buffer is now 16 KiB, which cut the micro-bench's WebSocket CPU per message
by 9-73%. Run the gate above against that.

The subset to implement, from the 2026-10-22 audit:

- **Frames.** 7-, 16-, and 64-bit lengths. Reject unmasked client frames and
  non-zero RSV bits.
- **Opcodes.** Binary is one qmux record. Continuation frames are reassembled
  up to `max_record_size`. Ping gets a Pong. Close is echoed, then the session
  shuts down. Text is a protocol violation. Control frames are at most 125
  bytes and never fragmented.
- **Inbound.** Unmask in place, then slice records straight out of the
  transport's read buffer, as `transport::Stream` does for byte streams.
- **Outbound.** Encode the frame header in front of each record in the
  transport's write buffer. The client masks in place.
- **Server handshake.** None: the relay keeps hyper's upgrade, so this covers
  only the moq-tokio standalone listener (`Sec-WebSocket-Key` to SHA-1/base64
  accept, `Version: 13`, subprotocol pick, 400 when nothing matches).
- **Client handshake.** One GET with `Sec-WebSocket-Protocol`, then parse the
  101 response.

A maintained crate was weighed against this. The codec is small, sits on the
hot path, and must write into the driver's buffers, so qmux owns it.

Public API: none, since qmux already names no tungstenite type. The existing qmux interop tests (the `qmux06`
dev-dependency) and the browser path in `js/qmux` must keep passing
unchanged.

Changed 2026-11-07 (maintainer, /quest-plan from the API sweep of open PRs and remaining quests): qmux's tungstenite exposure is removed before the cut (done in [moq-dev/moq#8300](https://github.com/moq-dev/moq/pull/8300)), whatever this bench finds; this quest is perf-only.
