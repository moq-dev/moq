# [XS] Downstream auth servers read an attached credential from the token field

## Goal

Every auth server a deployment runs behind a relay reads a credential
attached in band, on `attach` and on that credential's `revalidate` and `end`
(the events with `auth`), from `token`, not from a `jwt` in `query`.

## Plan

The relay now sends an attached credential as `token` (`kind` 0) and leaves
`query` as dialed. `moq auth serve` already reads it so; a server that
verifies `query` at `attach` sees the connection's first credential instead.
Neither the presented token nor its expiry is then the one admitted.

Found at moq.pro `9373cdfb3`: `rs/edge/src/auth/mod.rs` reads the credential
only from `request.query` through `query_jwt` (`:393`), never from
`request.token`. See `:1312-1317` (`End` forgets the query credential),
`:1331` (`Attach` and `Revalidate` decide on the query's `jwt`), `:1061`, and
`limits.rs:179`, `:383`, `:419`. Plain `?jwt=` dials are unaffected. Kixel's
`src/mesh.ts:557` reads a `credential` query parameter, and its client has no
attach use today.

Until moq.pro reads `token`, it must not meet a relay with this change:
`moq auth serve` reads a previous-release relay's `query` shape for now (a
marked temporary migration in `rs/moq-auth/src/serve.rs`), but moq.pro has no
such reader for the new shape.

## Waiting

- Maintainer: check the moq.pro and Kixel auth handlers read `attach` from
  `token`, and fix one that does not before it meets a relay with this change.
  An agent cannot read those private repositories from here. Done when both
  handlers' tests present an attach whose `query` holds an expired `jwt` and
  whose `token` holds a valid one.
