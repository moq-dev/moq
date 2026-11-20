# [M] QMux serializes frames when the TCP segment leaves

## Goal

A QMux session over the user-space TCP connection, over plain TCP, TLS, or
WebSocket with or without TLS, builds its frames, TLS records, and WebSocket
frames only when the connection pulls bytes for a segment. So data queued
on a stream that is reset or expires before that moment never reaches the wire.
The same stack also offers a plain `AsyncRead`/`AsyncWrite` stream for
everything else on the port.

## Plan

- The QMux mux already serializes frames lazily in `poll_transmit`
  (`rs/moq-quic/src/connection/mux/mod.rs`). What commits early today is the
  driver writing into the kernel, so the work is a pull path from the TCP
  connection through the WebSocket and TLS layers to the mux.
- Size TLS records and WebSocket frames to the pull, so each layer commits only
  what the segment carries. A TLS record that spans segments commits whole when
  its first byte leaves, so keep records near one segment.
- Report the controller's RTT and estimate through the session's
  `SocketStats` (`rs/qmux/src/socket.rs`) in place of `TCP_INFO`.

## Required

- [TCP connection](/quest/m4/userspace-tcp/engine.md) - the pull interface this feeds
