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

**Investigation:** A real QUIC request held at embedded `Auth::admit` reproduces the bug deterministically: after `Admissions::next()` hands out the unanswered admission, starting shutdown makes `Observer::drained()` resolve immediately. The local reproducer fails with `drain completed while auth admission is pending` (0.065 s, `just rs test -p moq-relay --features test-support pending_admission_holds_drain`). It used an unanswered embedded admission as the barrier, with no timing sleep. The exploratory reproducer is not a production test or a claimed regression fix.

Counting in `Connection::with_shutdown` is insufficient: `moq_tokio::server::Request` explicitly represents a connection whose transport and MoQ SETUP handshake already completed. Native handshake tasks are spawned inside the server's backend loops before `Listener::accept` returns that Request. A relay-only guard move fixes the auth gap but leaves the actual handshake gap. WebSocket has a separate pre-upgrade auth path in `serve_ws`, so counting only `handle_socket` also starts too late.

Recommended design is a transport-admission RAII counter owned by moq-tokio Server/Setup, shared by every worker Server and relay shutdown. Its guard starts when the transport request is accepted, moves through handshake and Request, and remains with the spawned session driver. WebSocket acquires the same guard before auth and carries it through the upgrade. A refused or failed handshake releases its guard immediately; an unfinished handshake holds the drain only until the existing drain deadline. Alternatives are a callback to obtain a relay guard (adds a callback API across the package boundary), or counting returned Requests (does not meet this quest). The counter's public shape and transfer through every backend need review before implementation.

This investigation does not change runtime behavior, remove the io_uring straggler, or update shipped-behavior docs. The quest remains open for the admission API, all-backend implementation, deadline/GOAWAY regressions, and removal of the workaround once its original window is actually covered.
