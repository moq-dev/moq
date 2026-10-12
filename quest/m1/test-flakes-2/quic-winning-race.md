# [S] QUIC-wins race test holds under load

## Goal

`connection::tests::quic_winning_opens_one_session` (`rs/moq-tokio`) passes
under a loaded `just check`, fixed at its cause. It still proves that when
QUIC wins, the WebSocket fallback is dropped and never opens a session.

## Plan

Seen 2026-10-11 by two agents independently, at load averages of 50 to 130.
WebSocket won the race, so the test's assertion that the transport is
`WebTransport` failed. It passes alone.

Cause: the test races a real QUIC handshake against the 200 ms WebSocket head
start (`DEFAULT_DELAY` in `rs/moq-tokio/src/websocket.rs`). A loaded machine
can take longer than that to finish the handshake, and then WebSocket winning
is the product working as designed. The test asserts the outcome of a
wall-clock race.

Decided 2026-10-11: make QUIC's win part of the test's setup rather than the
result of a race. For example, the fixture's WebSocket side doesn't answer
before the QUIC session is accepted. Pick the mechanism that keeps the
"dropped before it opened a session" assertion meaningful. Don't block on
[Controlled time](/quest/m1/time/README.md): real sockets fight a paused
clock, so the fixture needs this shape either way. Never widen the head start
or a wait, per this questline's rules.

Observe the fallback's cancellation directly, for example by a drop or
connection-close signal. Today `accepted.try_recv()` only proves the queue
is empty at one instant, and a held WebSocket side could hide a fallback
that was never dropped.

Public API: none. Wire: none.
