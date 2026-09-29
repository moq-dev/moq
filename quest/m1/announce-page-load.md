# [S] The live marker waits for the first connection on page load

## Goal

An announcement stream in `@moq/net` opened before the first session connects
does not yield `Live` on an empty set. `Live` comes only after that first
session's initial set has been delivered. There is no give-up: a failed
connection shows up as a connection error, never as an empty `Live`, so a
player can't conclude "offline" before the relay has answered.

Every other JS place with the same page-load gap follows the same rule.

Out of scope: the order of events that race the `Live` boundary once the
replay lands. JS keeps its append-only announce queue.

## Plan

- Decided by the maintainer (2026-09-29): fix page load only. Rust's
  per-prefix barrier (`OriginConsumerState::landed` in
  `rs/moq-net/src/model/origin.rs` records the prefixes still owed, and `Live`
  follows once each is delivered or cancelled) is the target ordering. It
  arrives when [generated lite](/quest/m1/rs2ts/lite.md) replaces js/net's
  model layer, so don't rebuild the JS queue around per-prefix folding by
  hand. Both orderings answer "is it offline?" the same way.
- The JS reconnect loop (`js/net/src/connection/reload.ts`) already counts as
  an answerer for requests through `expect()`; it also takes a replay hold on
  the origin (`replaying` in `js/net/src/origin.ts`) until its first
  session's initial set lands. The hold never times out.
- The one-shot `Connection.connect` and `Connection.accept` (`connect.ts`,
  `accept.ts`) register their replay source only after the handshake, so they
  take the same hold before it and release it on failure, which surfaces as
  the connection error.
- Rust needs no change: a Rust app calls `connect` before it opens the
  stream, which then waits on that session's replay.
- Tests: open the stream before the first connection and get `Live` only
  after its replay; fail the first connection and get no `Live`; the same
  pair for one-shot `connect` and `accept`.

Public API: `@moq/net` changes when its announcement stream yields `Live`, so
it lands on `dev`. Wire: none.

## Related

- [Generated lite](/quest/m1/rs2ts/lite.md) - brings Rust's per-prefix `Live` barrier to JS
