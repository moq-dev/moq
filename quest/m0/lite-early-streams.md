# [S] moq-lite streams that arrive before SETUP are held

## Goal

On lite-05 and later (the versions with a Setup Stream), a uni stream that
arrives before the peer's SETUP is held and handled once SETUP lands, never
aborted, in Rust and JS. A second Setup Stream closes the session with
PROTOCOL_VIOLATION in both languages, as the lite draft requires.

## Plan

Facts (`origin/main`, 2026-10-01):

- Rust `lite::accept_setup` (`rs/moq-net/src/lite/session.rs`) aborts every
  non-SETUP uni stream with `UnexpectedStream`.
  `accept_request_skips_uni_stream_before_setup` in `rs/moq-net/src/server.rs`
  pins that. An unknown `DataType` fails the handshake.
- JS lite has no pre-SETUP gate: `#runUni` in `js/net/src/lite/connection.ts`
  handles SETUP like any other uni stream, so early groups already work. It
  also accepts a second SETUP silently, and the `#runUnis` catch only stops the
  stream, so throwing `ProtocolViolation` there would not close the session.
  The `#runBidis` catch closes on `ProtocolViolation`, but with a bare
  `close()` rather than the PROTOCOL_VIOLATION code.
- `drafts/draft-lcurley-moq-lite.md`: neither endpoint waits for the peer's
  SETUP. A receiver MUST buffer only streams whose encoding depends on a
  negotiated extension, and a second Setup Stream MUST close with
  PROTOCOL_VIOLATION. Rust's abort is stricter than the draft.
- Lite-01 to lite-04 have no Setup Stream and are out of scope.

Decided (2026-10-01):

- ✅ Rust holds early uni streams until SETUP, then hands them to the normal
  classifier, the same shape as
  IETF early streams (#4686). Rust can't
  route before SETUP anyway, since path and role come from it. Rejected:
  processing immediately unless extension-dependent, which needs a session
  before SETUP.
- ✅ The duplicate-SETUP check rides along, in both languages. In JS the uni
  catch must close the session with the PROTOCOL_VIOLATION code, not copy the
  bidi path's bare `close()`.

The remaining JS gap is the duplicate SETUP and its close path; the hold is
Rust work unless JS turns out to need an extension-dependent hold.

Crib from #4686: its `died_before_header` helper and peek-then-hold pattern.
QUIC stream credit bounds the held queue. Check whether JS lite must buffer any
extension-dependent stream type today, and hold those until SETUP.

Tests: flip `accept_request_skips_uni_stream_before_setup` to expect the group
to be delivered. In both languages, a duplicate SETUP closes the session.

Public API: none. Wire: none; matches the lite draft.
