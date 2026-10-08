# [M] Lite NOT_SUPPORTED

## Goal

A lite presenter learns that its token is unsupported whenever that happens,
not only on the first reply. Today, if a grant update is too large for AUTH_OK
after a grant was issued, the acceptor resets the stream with INTERNAL_ERROR.
The grant is revoked, but the presenter sees a generic stream error, while the
moq-transport binding answers `AUTH_ERROR { NOT_SUPPORTED }`. After this
change, lite has a NOT_SUPPORTED session code, so `Error::Unsupported`
round-trips on both wires.

## Plan

Decided while planning the follow-ups of
[#4446](https://github.com/moq-dev/moq/pull/4446):

- **The carrier is AUTH_ERROR, not a stream reset code.** AUTH_ERROR after
  AUTH_OK already revokes a grant, so this mirrors the IETF binding exactly.
  A lite acceptor answers `AUTH_ERROR { NOT_SUPPORTED }` for every
  unsupported case: no in-band verification, a grant that cannot be
  expressed, or a grant too large for AUTH_OK, whether as the first reply or
  after one. A presenter still reads a reset before any reply as unsupported,
  because older acceptors and peers without the Auth Stream reset.
- **The code is lite session code 0x30 NOT_SUPPORTED**, the first of
  moq-lite's own range (48 to 63). moq-transport's session registry has no
  NOT_SUPPORTED. A bridge maps it to REQUEST_ERROR NOT_SUPPORTED (0x3), which
  `Error::Unsupported` already does on IETF.
  [Expired token error](/quest/m1/auth/expired-error.md) allocates after it.
- **`Error::Unsupported` maps to 0x30 everywhere on lite**, not just in
  AUTH_ERROR. Rust `SessionError` and JS `SessionCode` round-trip it, so a
  session closed for an unsupported feature stops reading as INTERNAL_ERROR.
- **The message ceiling is not this quest's** (decided in the 2026-10-06
  audit): [Request caps](/quest/m0/request-caps.md) (#4820) caps lite
  messages at 65,535 bytes, moq-transport's 16-bit control Message Length, so
  a grant too large for AUTH_OK is too large on both wires. This quest only
  answers that case with NOT_SUPPORTED.
- **Compatibility.** An old peer that receives 0x30 treats it as an
  unspecified error, as the draft requires. That is no worse than today's
  reset.
- Drafts: add the session code to `drafts/draft-lcurley-moq-lite.md`, and
  replace the reset rule in the Auth Stream section. Update `drafts/draft-lcurley-moq-auth.md` only where it
  refers to lite. `just drafts check`.
- Tests: a regression test per side (Rust and JS). An oversized update after
  an AUTH_OK reports `Unsupported` and revokes the grant. Also add an interop
  case, run with `just test interop --all`.

Public API: none new (`Error::Unsupported` exists). Wire: a new lite session
code.

## Related

- [Request caps](/quest/m0/request-caps.md) - owns the 65,535-byte lite message ceiling that makes a grant too large for AUTH_OK
- [AUTH violations](/quest/m1/auth/violations.md) - AUTH protocol violations close the session on the same path (AUTH endings landed in #4550)
