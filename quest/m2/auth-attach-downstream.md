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

Rechecked 2026-11-20 by the main → release audit at moq.pro `828766e510`:
still unfixed. `rs/edge/src/auth/mod.rs` reads only `query_jwt(request.query)`
(`:1064`, `:1316` on `End`, `:1339` on `Attach` and `Revalidate`) and never
`request.token`, and no moq.pro quest owns the change. The planning PR lists
it as a follow-up for moq.pro's own `/quest-plan`, required by moq.pro's
[cut pin adoption](https://github.com/moq-dev/moq.pro/blob/main/quest/m0/adopt-cut-pin.md),
so moq.pro never runs a relay with this change before it reads `token`. Its
test presents distinct credentials through attach, revalidate, and end
against both the last released relay and the candidate.

## Waiting

- moq-dev/moq.pro: the edge auth handler reads an attached credential from
  `token` on `attach`, `revalidate`, and `end`. Check with
  `grep -n "request.token" rs/edge/src/auth/mod.rs` on moq.pro `main`, and a
  test whose attach carries an expired `jwt` in `query` and a valid `token`.
- kixel-ai/kixel.ai: `Mesh.admit` (`src/mesh.ts:557`, rechecked 2026-11-20)
  reads a `credential` query parameter on every event, `attach` included, and
  its client attaches nothing yet. Before a Kixel client attaches, it reads
  `token` there too; check the same way.
