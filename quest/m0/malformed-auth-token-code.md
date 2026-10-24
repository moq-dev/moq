# [S] MALFORMED_AUTH_TOKEN session code

## Goal

A session closed with MALFORMED_AUTH_TOKEN (0x16) reports "malformed auth
token" instead of `unknown code=22`, in Rust and JS, and moq-net closes with
it where it refuses a connection token it cannot parse, so a client learns
its token's format is wrong rather than its grant.

## Plan

✅ Decided 2026-10-23 (maintainer, importing #6567): add
`SessionError::MalformedAuthToken` = 0x16 beside `UnknownAuthTokenAlias`,
encoded and decoded, rather than leave it unknown. Why: its neighbours 0x13,
0x17, and 0x18 are mapped, and moqx sends it for a token that isn't a CWT.

- Add it to the round-trip table in `rs/moq-net/src/error.rs`, mirror it in
  `js/net`, and add the row to draft-lcurley-moq-lite's session-code table
  with a changelog bullet.
- Sending needs the auth contract to say why: moq-net treats the SETUP token
  as opaque, `moq auth serve` already tells `Refusal::InvalidToken` apart but
  `Refusal::body` collapses it into `Reason::Refused`, and the relay's
  `refuse` maps that to `Unauthorized`. Add a malformed `refusal::Reason`
  (older relays still read it as `Refused` through `#[serde(other)]`), map
  `InvalidToken` to it, and have moq-relay close with this code; keep
  `Unauthorized` for a well-formed token that grants nothing. Follow the
  Cross-Package Sync row for `rs/moq-auth` (`js/auth`,
  `doc/bin/relay/auth.md`).

Public API: additive variants on `SessionError` and `moq_auth::refusal::Reason`,
and their JS mirrors. Wire: a newly assigned moq-lite session code with moq-transport's
meaning.

## Closes

- [#6567](https://github.com/moq-dev/moq/issues/6567) - 0x16 reports `unknown code=22`
