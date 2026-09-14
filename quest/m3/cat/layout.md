# [S] The JWT lives under moq_auth::jwt

## Goal

`moq_auth` is organized by token format: `moq_auth::jwt` holds `Claims`,
`Scope`, `Permissions`, `Key`, `KeySet`, `KeyId`, `Algorithm`, and the
signing and verification that the package quest put at the crate root, so a
second format gets a sibling module instead of prefixed names. The contract
types (`Request`, `Grant`, `lease`, `Client`, `Counters`) stay at the root
because every format produces them. Every consumer compiles against the new
paths.

## Plan

- `rs/moq-auth/src/jwt/` with `mod.rs` re-exporting what `algorithm.rs`,
  `claims.rs`, `key.rs`, `key_id.rs`, `set.rs`, `generate.rs`, and `fs.rs`
  export today; `lib.rs` keeps `pub mod jwt` and stops glob-exporting them.
  `path.rs` stays at the root if `Grant` uses it.
- Consumers: `rs/moq-cli` (`moq auth generate|sign|verify`), `rs/moq-room`,
  `rs/moq-relay` (until [Relay](/quest/m1/auth/relay.md) deletes its
  adapter), the examples, and `doc/lib/rs/moq-auth.md`.
- `@moq/auth` is untouched: nothing else lands in JS, so its flat exports
  stay.

Public API: breaking rename inside the unpublished `moq-auth` 0.1.0, on dev.
Wire: none.

## Required

- [Package](/quest/m1/auth/package.md) - the crate this reorganizes
