# [S] A moq-lite-06 session's auth requests end when its peer leaves

## Goal

On a moq-lite-06 session, the server's `Request::requests()` stream ends once
the client cancels or the connection closes, as it does on lite-07 and
moq-transport. A server loop draining it then ends instead of holding a task,
and its session, for each departed lite-06 client.

## Plan

Decided 2026-11-11 (maintainer, /quest-complete pick of
[moq-dev/moq#8516](https://github.com/moq-dev/moq/pull/8516)): ✅ plan this in
m1 (rejected: m3; ignoring it).

Found while iterating #8516: in `rs/moq-ffi/src/test.rs`,
`a_credential_without_auth_is_unsupported` dials a `token_server("moq-lite-06")`
whose task drains `request.requests()` in a `while let`. After the client's
`session.cancel(0)`, that loop never ends, so the test calls
`serving.abort()` (the test lands with that PR). The queue behind
`requests()` closes only through `auth::Handle::close`, which every moq-lite
version reaches from `Driver::end` or `Driver`'s `Drop` in
`rs/moq-net/src/lite/session.rs`. So suspect the lite-06 driver not ending
(check what it watches to see the peer leave; see `has_setup_stream`) or the
server keeping it alive, rather than the stream itself.

- Reproduce in moq-net or moq-tokio with a lite-06 pair: the client cancels,
  and the server's `requests()` must yield `None` within the test's timeout.
  Check lite-07 and moq-transport the same way.
- Fix the root cause, then drop `serving.abort()` from that test so it
  asserts the server task ends on its own.

Public API: none expected. Wire: none.
