# [M] Auth access is patterns

## Goal

moq-auth and `@moq/auth` claims, grants, and permissions carry path patterns
only, as on release, until a quest that enforces bitrate caps adds rates
back. No published token or grant field is one every relay refuses. The
`Key::decode` footgun that skips expiry and claim checks is gone.

## Plan

Decided 2026-10-22 (API audit). `rs/moq-auth/src/access.rs` added `Rate` and
`Access { path, rate }` in place of `Patterns` on `Claims`, `Grant`, and
`Permissions` (JS mirrors `Access`, `AccessSchema`), but
`rs/moq-relay/src/auth.rs` `check_rates` refuses any cap and
`doc/bin/relay/auth.md` says so. moq.pro's only use is a test asserting the
refusal. It also regressed ergonomics (`grant.publish` became
`grant.publish.path`) and added a second `Rate` beside
`moq_net::bandwidth::Rate`.

- Revert to `Patterns`, delete `check_rates`, and drop the doc section.
  Re-adding an optional rate later is additive, since no token carries one
  today.
- Delete `Key::decode` (Rust `key.rs`, JS `key.ts`): no consumer outside
  tests.
- Update [publisher caps](/quest/m2/rate-grant.md),
  [QUIC rate](/quest/m2/rate-quic.md), and
  [WebSocket rate](/quest/m2/rate-websocket.md): whichever enforces first
  re-adds the claim with a rate type moq-auth owns (or a lower shared units
  crate), converted at the relay boundary, so moq-auth never depends on
  moq-net.

Public API: moq-auth and `@moq/auth` breaking against main, closer to
release. Wire: the token and grant lose the unenforced `rate` field.
