# [S] Malformed grant closes the session

## Goal

A moq-lite AUTH_OK carrying a malformed or non-canonical grant pattern, or an
out-of-range `Expires`, closes the session with PROTOCOL_VIOLATION, in Rust
and in JS. `drafts/draft-lcurley-moq-lite.md` already says so for patterns
under Path Pattern. Today a bad pattern ends only its token. When that token
is the session's own setup token, the grant union stays unknown, so the
client's fail-loud publish check never runs and an unauthorized publish waits
silently, as it did before AUTH.

## Plan

[#4277](https://github.com/moq-dev/moq/pull/4277) specified the rule. Codex
flagged the mismatch
([r4113773014](https://github.com/moq-dev/moq/pull/4277#discussion_r4113773014))
and the agent deferred it
([r4114097664](https://github.com/moq-dev/moq/pull/4277#discussion_r4114097664)).
The maintainer decided in the 09-28 merged-PR audit: fail loud and match the
draft, with tests in both languages.

Where it lives, as of the auth line's code:

- Rust: `AuthOk` decoding (`rs/moq-net/src/lite/auth.rs`) refuses a bad
  pattern with `DecodeError::InvalidValue`, and `PresentToken`
  (`rs/moq-net/src/lite/session.rs`) ends only that token on it. Only
  `Error::ProtocolViolation` ends the session there.
- JS: the auth loop (`#run` in `js/net/src/auth_session.ts`) catches the
  decode error and ends the token.
- Both: a token that ends without a reply leaves the union unset, and
  `Enforce` (`rs/moq-net/src/auth.rs`) and `enforceGrant` (JS) skip the check
  while it is unset.

Decisions:

- A pattern that fails to parse or is not canonical (`/room`, `*/**`, a 33rd
  segment) closes the whole session with PROTOCOL_VIOLATION, not just the
  token.
- A refusal (AUTH_ERROR) and a peer that predates AUTH keep their current
  meaning: those end the token, not the session.
- An out-of-range `Expires` closes the session the same way. Rust already
  does so when the expiry overflows the local clock, but its decoder accepts
  any `u64` while JS's `u53` read refuses anything past 2^53 - 1. The draft
  does not say what out of range means, so define it next to the other
  AUTH_OK fields, and make Rust and JS refuse the same values. The maintainer
  decided this in [#4380](https://github.com/moq-dev/moq/pull/4380).

Tests in Rust and JS send a malformed pattern, a non-canonical pattern, and an
out-of-range `Expires`, and assert that the session closes with
PROTOCOL_VIOLATION. If a shared vector is easy, add them to the interop suite
as well, since both sides must agree.

This quest owns the lite AUTH_OK decode gap; the wider
[AUTH violations](/quest/m1/auth/violations.md) sweep covers every other AUTH
read path (decided 2026-10-07, folding the overlap #4868 planned).

## Related

- [AUTH violations](/quest/m1/auth/violations.md) - the sweep of every other AUTH read path
- [In-band auth](/quest/m1/auth/README.md) - the line this blocks
