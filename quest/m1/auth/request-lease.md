# [M] moq-relay honors a request token with its own lease

## Goal

moq-relay verifies a moq-transport request token through `moq_auth::Client`,
each request getting a lease of its own that is never counted as a session
grant and ends when the request ends. Today the relay has no `requests()`
consumer, so it refuses every request token `Unsupported`.

## Plan

An application acceptor built on moq-net answers request tokens through
`requests()` itself and does not need this; it is the relay's half of
[Request tokens](/quest/m1/auth/request-token.md).

- A per-request call on `moq_auth::Client`, not the `Client::attach` that
  [Relay tokens](/quest/m1/auth/relay-refresh.md) builds. `attach` connects
  with the connection's id, and the auth server treats that as one more grant
  on the session that never POSTs `end`, so a request token would widen the
  whole connection for its life. The per-request call carries the token in
  `moq_auth::Request.token` with its kind and the request's path, is never
  counted as a session grant, and ends when the request ends. Two requests
  carrying the same bytes get two leases. Name the call while implementing.
- The request is resolved against the origin with the path checked against
  that lease's grant, not through the session's scoped origin handle.
- Docs: `doc/bin/relay/auth.md` states the order (session grant, then the
  request's token, then `UNAUTHORIZED`) and that a request token covers only
  its request.
- Tests: through the relay, the auth server never counts a request token as a
  session grant and sees its lease end with the request.

Public API: additive on `moq_auth::Client`. Wire: none.

## Required

- [Request tokens](/quest/m1/auth/request-token.md) - the per-request grant
  this lease backs
- [Relay tokens](/quest/m1/auth/relay-refresh.md) - supplies the lease
  revalidation this reuses
