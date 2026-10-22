# [XS] Stream sockets disable Nagle

## Goal

Every TCP socket that carries a qmux session sets `TCP_NODELAY`, whether
accepted or dialed: the relay's WebSocket listener (plain and TLS), `tcp://`
and `tls://` qmux, and the WebSocket client. A flushed record leaves the host
immediately instead of waiting on the peer's delayed ACK.

## Plan

Nothing on these paths sets it today. The only `set_nodelay` in `rs` is in
moq-rtmp (`rs/moq-rtmp/src/server.rs`). The relay's `WebAcceptor`
(`rs/moq-relay/src/web.rs`) wraps a plain `DefaultAcceptor` or
`RustlsAcceptor`. `qmux::tcp` and `qmux::tls` dial with a bare
`TcpStream::connect`. `qmux::ws::Client::connect` passes
`disable_nagle = false` to `connect_async_with_config` and
`connect_async_tls_with_config`, and moq-tokio's TLS-name-override dial opens
its own `TcpStream`.

The qmux writer already batches records and flushes once per batch
(`rs/qmux/src/session.rs`, `BATCH_BYTES`), so Nagle has nothing left to
coalesce and only adds delay.

Set it where each socket is created or accepted. Prefer the shared helpers
(`moq_sock::bind::tcp` for listeners, and one dial path), so a new listener
gets it by default rather than by remembering. Test that an accepted relay
WebSocket socket and each dialed client socket report `nodelay() == true`.

## Related

- [Stream transports](/quest/m2/perf/stream/README.md) - the measured perf work on the same sockets
