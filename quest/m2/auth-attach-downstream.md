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

## Waiting

- Maintainer: check the moq.pro and Kixel auth handlers read `attach` from
  `token`, and fix one that does not before it meets a relay with this change.
  An agent cannot read those private repositories from here. Done when both
  handlers' tests present an attach whose `query` holds an expired `jwt` and
  whose `token` holds a valid one.
