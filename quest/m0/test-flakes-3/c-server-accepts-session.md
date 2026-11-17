# [S] moq-c's server session test closes its client after the server did

## Goal

`rs/moq-c/src/test.rs::server_accepts_a_session` passes every run.

## Plan

Found 2026-11-16 by `just check` on
[moq-dev/moq#8964](https://github.com/moq-dev/moq/pull/8964), whose change
does not touch moq-c's session or server code. Running it alone with
`cargo nextest run -p moq-c server_accepts_a_session` fails 4 of 12 runs at
main 73d65edbf7: `moq_session_close(client)`, after the accepted session
closed and its tail drained, returns -21 where the test expects 0.

The client's session likely already ended on its own once the server side
closed, so its id is gone by the time the test closes it. Find which: if a
closed peer is meant to leave the handle valid until `moq_session_close`, as
the session's terminal callback contract suggests, the early release is a
library bug, fixed there with a regression test. Only if releasing it is the
contract does the test change, by waiting on the client's terminal callback
before it closes the handle.

Public API: none. Wire: none.
