# [M] moq-relay grants a request token only the request kinds its auth server allows

## Goal

An auth server answering a moq-relay `attach` can admit a request token for some
request kinds and not others: a token granted for a SUBSCRIBE at a path does not
also admit a TRACK_STATUS, a standalone FETCH, or a SUBSCRIBE_NAMESPACE there,
including one that reuses the SUBSCRIBE's token. A server that names no kinds
admits every kind, as today.

## Plan

moq-net's request-borne `auth::Grant` carries `kinds`, a `RequestKinds` set that
`RequestKind::covers` checks for every request naming the token, and
`auth::Request::{kind, path}` report the request that named it first. moq-relay
is a consumer of both, beside any acceptor answering a subscriber credential, which
rides several request kinds on one token:

- `rs/moq-relay/src/auth/refresh.rs` answers each in-band `auth::Request`. Send its
  `kind()` and `path()` on the `attach` nested as one `request` object, apart from
  the dialed `path` `moq_auth::Request` already carries, so the server decides for
  the path actually requested rather than every path the token could reach.
- `moq_auth::Grant` (`rs/moq-auth/src/grant.rs`) gains an optional kinds field, absent
  meaning every kind. `Credential::wire` in `refresh.rs` maps it into
  `auth::Grant::kinds` for a request token. A connection credential's grant joins the
  session's scope, which ignores kinds, so the relay refuses a kinds field there
  rather than drop it.
- `js/auth` mirrors the new `attach` facts and the grant field, with the contract
  and corpus tests both sides read.
- Test through the relay on moq-transport 18 and 22: a token granted for SUBSCRIBE
  only keeps its subscription and refuses the other three kinds at its path; a
  grant with no kinds admits all four.

Public API (Rust, JS): additive fields on `moq_auth::{Request, Grant}` and their JS
mirrors. Wire: the auth server's JSON gains optional fields, which a relay before
this change refuses as unknown, so a server sends them only to relays that list
them in `Request::relay`.
