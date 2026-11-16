# [S] An attached token arrives as a token, not in the query

## Goal

When a client attaches a credential in band, the relay's `attach` request
to the auth server carries it in `Request.token` and leaves `query` exactly
as the client dialed it. The relay no longer knows the name `jwt`, and an
in-band token need not be UTF-8. `moq auth serve` and the docs read the
attached credential from `token`.

## Plan

Decided 2026-11-16 (maintainer, /quest-plan of the pre-cut audit): ✅ the
in-band token rides `Request.token` and the query stays as dialed (rejected:
keeping the rewrite and documenting it; a third field for the attached
credential). The contract says nothing is parsed on the relay's behalf, and
the rewrite is the one place the relay breaks that.

Facts found by the audit:

- The contract: "Nothing is parsed on the relay's behalf" and `query` is
  "The raw query string" (`rs/moq-auth/src/request.rs:13,60`).
- For an in-band token the relay clones the connection's request, replaces
  or appends a `jwt=` pair in `query`, and clears `token`
  (`rs/moq-relay/src/auth/refresh.rs:259-265`, `with_jwt` at `:497-523`).
  So an `attach` request carries a query the client never dialed, and a
  server whose credential is not called `jwt` reads its own parameter at
  `connect` and `jwt` at `attach`.
- The lite draft's token is opaque bytes, but the relay refuses one that is
  not UTF-8 (`refresh.rs:259-262`), only because it must fit a query string.
  A binary token works in SETUP (`token.value` is base64url) and fails in
  band.
- `jwt` is also fixed in `presents_token`
  (`rs/moq-relay/src/auth.rs:878-885`) and in the clients
  (`rs/moq-net/src/auth/tokens.rs:456-532`,
  `js/net/src/connection/tokens.ts:25-50`).
- `token`'s docs still say "a moq-transport client"
  (`request.rs:63`, `js/auth/src/contract.ts:123`), though lite-07's Token
  Setup Parameter lands there with `kind` 0.

For the PR:

- Relay: on `attach`, set `token` to `{kind: 0, value}` and leave `query`
  alone. Delete `with_jwt` and the UTF-8 refusal. `presents_token` is the
  relay's only other reader of `jwt`; replace it with a fact the request
  already has, or move the question to the server, so the relay holds no
  parameter name.
- `moq auth serve` verifies `token` when `event` is `attach`. Today it
  counts a query `jwt` and a SETUP token once when equal and refuses when
  they differ (`rs/moq-auth/src/serve.rs:195-197`); on `attach` the dialed
  query still holds the first credential, so that rule must not fire there.
- The clients keep `?jwt=` only as the fallback for versions before
  lite-07. Don't change them here.
- Both downstream auth servers read `attach` requests. This is a contract
  break for them: record it in the Impact section, and check moq.pro's and
  Kixel's handlers before merging.
- Update `request.rs`, `contract.ts`, and `doc/bin/relay/auth.md` (the
  `token` field's description and the `attach` event).
- Tests: an attached binary token reaches the server unchanged; an `attach`
  request's `query` equals the dialed one; `moq auth serve` renews a session
  whose dialed query holds an expired `jwt`.

Public API: none in Rust or JS beyond doc comments. HTTP contract: breaking
for `attach` (the credential moves from `query` to `token`). Wire: none.

## Related

- [A relay refuses a grant it does not fully understand](/quest/m2/auth-grants.md) - the other change to the same request and its docs
