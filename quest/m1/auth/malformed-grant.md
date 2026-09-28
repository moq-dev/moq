# [S] Malformed grant closes the session

## Goal

A moq-lite AUTH_OK carrying a malformed or non-canonical grant pattern, or an
out-of-range `Expires`, closes the session with PROTOCOL_VIOLATION, in Rust
and in JS. `drafts/draft-lcurley-moq-lite.md` already says so for patterns
under Path Pattern. Today both implementations only end the offending token
and leave the session up.

## Plan

[#4277](https://github.com/moq-dev/moq/pull/4277) specified the rule, but
Rust's `PresentToken` (`rs/moq-net/src/lite/session.rs`) turns the decode
error into that token's end, and the JS auth loop (`#run` in
`js/net/src/auth_session.ts`) catches it the same way. Codex flagged the
mismatch
([r4113773014](https://github.com/moq-dev/moq/pull/4277#discussion_r4113773014)).
The agent declined because every malformed AUTH reply behaves this way, and
proposed deciding it separately
([r4114097664](https://github.com/moq-dev/moq/pull/4277#discussion_r4114097664)).
The maintainer decided in the 09-28 merged-PR audit: fail loud and match the
draft, with tests in both languages.

- A pattern that fails to parse or is not canonical (`/room`, `*/**`, a 33rd
  segment) closes the whole session with PROTOCOL_VIOLATION, not just the
  token.
- A refusal (AUTH_ERROR) and a peer that predates AUTH keep their current
  meaning: those end the token, not the session.
- An out-of-range `Expires` closes the session the same way. Rust already
  labels it a ProtocolViolation in `PresentToken` but only ends the token.
  The maintainer decided this in
  [#4380](https://github.com/moq-dev/moq/pull/4380). The draft does not say
  what out of range means yet, so define it there, next to the other AUTH_OK
  fields. Rust and JS must then refuse the same values.

Tests in Rust and JS send a malformed pattern, a non-canonical pattern, and an
out-of-range `Expires`, and assert that the session closes with
PROTOCOL_VIOLATION. If a shared vector is easy, add them to the interop suite
as well, since both sides must agree.

## Related

- [In-band auth](/quest/m1/auth/README.md) - the line this blocks
