# [S] moq-rs's client body-error test is cancelled under load

## Goal

`py/moq-rs/tests/test_server.py`'s `test_client_context_keeps_the_body_error`
passes every loaded `just check`.

## Plan

Found 2026-11-17 by `just check` on
[moq-dev/moq#9029](https://github.com/moq-dev/moq/pull/9029), which deletes
socket endpoints and touches nothing the test uses. Under the loaded check the
body's `ZeroDivisionError` was replaced by `MoqError.Cancelled: cancelled`
raised from a uniffi call inside the `async with moq.Client(...)` body. Run
alone it passed 6 times in a row.

So under load the client's session is cancelled before the body raises, and a
call in the body sees that cancel first. Trace which call raises it (the
announcement read or the subscribe) and why the session is cancelled before
the context exits: if the client cancels its session early, such as when its
connect races the server's accept, that is a bug in `py/moq-rs` or moq-ffi,
fixed there with a regression test. Only if the trace shows the test reads a
session it does not yet hold is the fix in the test, by the epic's rules.

Public API: none. Wire: none.
