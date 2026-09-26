# [M] Close codes on every transport

## Goal

A client sees the application close code its peer sent, for example
`SessionError::App(4011)` from `Session::abort` or `Request::reject`, over
WebSocket (qmux) and raw QUIC (`moqt://`), as it already does over
WebTransport. Never `Transport("connection closed")` or its own `Internal`.

## Plan

Both bugs are upstream; fix them at the source, release, and bump the pins.

- qmux 0.5.1 (`moq-dev/web-transport`) lets later writes overwrite the
  recorded close in `session.rs`: the WS Close frame read after
  APPLICATION_CLOSE (reader loop, backend `send_replace`), a local `close()`
  after the peer closed, and a second peer APPLICATION_CLOSE. `accept_uni` and
  `accept_bi` also return a bare `Closed`. Make the first close win, make
  `close()` a no-op once closed, and have `accept_*` return the recorded
  reason. Add a qmux test where APPLICATION_CLOSE and EOF arrive together.
- `web-transport-moq` 1.3.1 (`moq-dev/noq`) maps `ApplicationClosed` only
  through the HTTP/3 code space in `error.rs`, so a raw `moqt://` code yields
  no `session_error()`. Map raw QUIC codes directly.
- moq-net's `close(Internal)` after a transport error is correct: closing a
  closed connection does nothing. Do not work around it here.
- One moq-tokio regression runs the issue's three cases (abort after accept,
  abort then drop, reject during handshake) over `https://`, `ws://`, and
  `moqt://`, and fails on the current pins.
- Check whether `@moq/net`'s qmux peer keeps the first close too; fix it in
  the same PR if not.

## Closes

- [#4249](https://github.com/moq-dev/moq/issues/4249) - application close code is lost over the WebSocket (qmux) transport

## Related

- [qmux on noq](/quest/m1/quic/qmux.md) - the rewrite must keep first-close-wins
- [io_uring close](/quest/m1/quic/uring-close.md) - the same symptom class on the io_uring backend
