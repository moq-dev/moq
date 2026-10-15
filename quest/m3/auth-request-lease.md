# [M] moq-relay honors a request token with its own lease

## Goal

moq-relay verifies a moq-transport request token through `moq_auth::Client`,
each request getting a lease of its own that is never counted as a session
grant and ends when the request ends. Until this lands the relay explicitly
refuses request-token verification as `Unsupported`, even if it consumes
session-token events and once [Request tokens](/quest/m1/auth/request-token.md)
merges. Every token-carrying request is verified by its token, even one the
session's own permission covers (decided on #5148).

## Plan

Deferred on 2026-10-12 until a consumer needs relay request verification.
This is not required for the experimental Rust hook or the session AUTH epic.
Revisit the design against moq-transport's authentication model at that point;
the lease integration below is future work, not a capability of the hook.

An application built on moq-net opts into request-token verification through
the acceptor API #5148 ships, verifies each token-bearing request itself
before accepting it, and does not need this. This quest is the relay's half of
[Request tokens](/quest/m1/auth/request-token.md).

- A per-request call on `moq_auth::Client`, not the `Client::attach` that
  session tokens use. `attach` connects
  with the connection's id, and the auth server treats that as one more grant
  on the session that never POSTs `end`, so a request token would widen the
  whole connection for its life. The per-request call carries the token in
  `moq_auth::Request.token` with its kind and the request's path, is never
  counted as a session grant, and ends when the request ends. Two requests
  carrying the same bytes get two leases. Name the call while implementing.
- The request is resolved against the origin with the path checked against
  that lease's grant, not through the session's scoped origin handle.
- Docs: `doc/bin/relay/auth.md` states the rule: a request carrying a token
  is decided by its verified request grant alone, a request without one by
  the session grant, and anything else is `UNAUTHORIZED`. It also says that a
  request token covers only its request.
- Tests: through the relay, the auth server never counts a request token as a
  session grant and sees its lease end with the request.

Public API: additive on `moq_auth::Client`. Wire: none.

## Required

- [Request tokens](/quest/m1/auth/request-token.md) - the per-request grant
  this lease backs
