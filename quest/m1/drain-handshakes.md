# [S] Drain in-flight handshakes

## Goal

A relay drain counts a session from admission, not only once it is
established, so a client still in its handshake when the drain starts is sent
a GOAWAY and waited for instead of being cut off by the relay exiting. The
drain still ends at its deadline whatever a handshake does.

## Plan

`shutdown::Observer::serve` is taken in `supervise`, after the handshake. A
drain that starts before any session reaches it sees nothing to wait for and
`Relay::run` returns at once. #4186 chose this on purpose, reasoning that with
DNS withdrawn first, arrivals are rare. The io_uring drain test hit it anyway: a
moq-lite-06 client sees its session before the relay has admitted it, so the
test now holds a straggler to keep the drain open.

Take the count where the relay first commits to a session (the accept or
registration point on each of the QUIC, io_uring, WebSocket, and iroh paths),
and hand it to the session so the guard spans auth and the handshake. A
session admitted during a drain already gets a GOAWAY carrying the time left,
so the change should be the count alone. Revisit whether a handshake that fails
auth, or never completes, should hold a drain until the deadline.

Add a regression test that triggers the drain between a client's connect and
the relay's admission and expects a GOAWAY, then drop the straggler workaround
in `rs/moq-relay/tests/runtime_uring.rs`. Update `doc/bin/relay/config.md`,
which documents the current behavior.
