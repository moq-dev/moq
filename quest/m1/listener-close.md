# [S] Closing a listener releases its socket before returning

## Goal

Closing a moq-tokio server, and moq-ffi's `Listener::close` on top of it,
returns only after the UDP socket is released, so a restart can rebind the
same port at once. Go's `TestReconnectAcrossRelayRestart`
(`go/wrapper/reconnect_test.go`) fails on main today because the rebind
races the old socket.

## Plan

`Server::shutdown` (`rs/moq-tokio/src/server.rs`) calls `noq.close()` and
sleeps 100 ms without waiting for the endpoint driver to drop the socket.
Await the driver's exit instead and delete the sleep; moq-ffi's
`Listener::close` already documents a synchronous release, so make it true
rather than change the doc. Check the io_uring and WebSocket listeners for
the same gap. Add a Rust test that closes a server and rebinds its port
immediately; the Go test becomes the cross-language regression. No wire
change; the only API change is that close takes as long as the release.
