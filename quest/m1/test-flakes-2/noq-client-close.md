# [S] moq-tokio's noq client close test holds under load

## Goal

`noq_client_close_reaches_server` (`rs/moq-tokio/tests/backend.rs`) passes
reliably under a loaded `just check`. It still proves what it proves today:
a noq client that closes and then loses its runtime, as when a process exits,
still delivers its close to the server instead of leaving the server to idle
out.

## Plan

Root problem: the test lets the client's close race the server's setup. The
client connects, closes, and drops its runtime on its own thread, while the
server's `accept()` is still waiting for the incoming request. When the close
wins, the server drops the half-set-up connection, as it should. `accept()`
then never yields, and the test fails with "accept timed out". The race is
in the test, not the product: a connection closed during its handshake is
correctly never handed out.

Order the steps by events, not by timing. The client closes only once the
server holds the established session. Nothing about the close itself should
need to change.

Follow this epic's rules: no sleeps, and no wider timeouts. Mock the
deadlines that are part of the property (such as the idle timeout that would
hide a lost close) if the real socket allows it. Otherwise keep wall time only
as a hang guard that can never decide a pass. Reproduce under load first and
confirm the race above is the failure.

Public API: none. Wire: none.
