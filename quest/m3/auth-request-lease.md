# [M] moq-relay ends a request token's lease with its request

## Goal

The auth server sees a moq-transport request token's lease end when its
request ends. Since #5148, a request token reaches the relay on `requests()`
like any in-band token and is verified through the same attach path; moq-net
confines its grant to the request. The auth server still counts it as one
more attached grant on the session that never reports `end`, against its own
`--limit-token` cap until the connection ends.

## Plan

✅ Decided 2026-10-15 (maintainer, #5148): a request token is one kind of
auth, named by its request, so the relay keeps a single verification path and
needs no request-specific variant on `requests()`.

Deferred until a consumer needs per-request accounting at the auth server.

- The relay drops a request token's credential when its issued grant closes.
  The missing piece is telling the auth server: either a per-request call on
  `moq_auth::Client` that ends the lease, or an `end` for an attachment that
  closes before its connection. Name the call while implementing.
- Two requests carrying the same bytes get two leases.
- Docs: `doc/bin/relay/auth.md` states that a request token's lease ends with
  its request.
- Tests: through the relay, the auth server sees a request token's lease end
  with the request, while the connection's own lease continues.

Public API: additive on `moq_auth::Client`. Wire: none.
