# [S] moq-c's session tests close their client after the server did

## Goal

`rs/moq-c/src/test.rs::server_accepts_a_session` and `rs/moq-c/src/test/parity.rs::session_tokens_and_grants` pass every run.

## Plan

Found 2026-11-16 by `just check` on
[moq-dev/moq#8964](https://github.com/moq-dev/moq/pull/8964), whose change
does not touch moq-c's session or server code. Running it alone with
`cargo nextest run -p moq-c server_accepts_a_session` fails 4 of 12 runs at
main 73d65edbf7: `moq_session_close(client)`, after the accepted session
closed and its tail drained, returns -21 where the test expects 0.

The client's session likely already ended on its own once the server side
closed, so its id is gone by the time the test closes it. That release is the
contract: `rs/moq-c/src/session.rs` removes a session's entry before its
terminal `CLOSED` callback, so `moq_session_close` on a handle whose terminal
callback has fired (or is about to) returns a negative code. Find why the client
ends: if a dialed client is meant to keep reconnecting after its server closes
the accepted session, its early end is a library bug, fixed there with a
regression test. Otherwise the test changes: it waits on the client's terminal
callback and drops the `moq_session_close(client) == 0` assertion, or asserts
the close is negative after that callback, as `moq_server_close` already does.

`session_tokens_and_grants` has the same shape: it closes the accepted session,
waits for its terminal callback, then asserts `moq_session_close(client) == 0`
(`parity.rs:530`). It fails 3 of 8 runs alone at main a79bbae919, found while
merging main into #8964. Fix both the same way.

Public API: none. Wire: none.
