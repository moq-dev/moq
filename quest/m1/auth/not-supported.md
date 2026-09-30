# [M] Lite NOT_SUPPORTED

## Goal

A lite presenter learns that its token is unsupported whenever that happens,
not only on the first reply. Today, if a grant update is too large for AUTH_OK
after a grant was issued, the acceptor resets the stream with INTERNAL_ERROR.
The grant is revoked, but the presenter sees a generic stream error, while the
moq-transport binding answers `AUTH_ERROR { NOT_SUPPORTED }`. After this
change, lite has a NOT_SUPPORTED session code, so `Error::Unsupported`
round-trips on both wires. Lite's message ceiling also drops to
moq-transport's 65,535 bytes and is stated in the lite draft, so the same
grant is too large on both wires.

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
- **The lite message ceiling is 65,535 bytes**, copied from moq-transport's
  16-bit control Message Length. It replaces the 64 MiB `MAX_MESSAGE_SIZE` in
  `rs/moq-net/src/lite/message.rs` and `js/net/src/lite/message.ts`. The lite
  draft's Message Length section makes it normative: a longer message is a
  PROTOCOL_VIOLATION, and a receiver MAY reject it from the length prefix
  alone. The separate 65,536-byte SETUP rule is folded into this one. Check
  that frame payloads, which have their own size limit, are not counted
  against it.
- **Compatibility.** An old peer that receives 0x30 treats it as an
  unspecified error, as the draft requires. That is no worse than today's
  reset. Lowering the receive limit on published lite versions only refuses
  messages between 64 KiB and 64 MiB. AUTH_OK is unreleased and is the only
  message plausibly that large; confirm nothing else can reach it before
  landing.
- Drafts: add the session code and the Message Length cap to
  `drafts/draft-lcurley-moq-lite.md`, and replace the reset rule in the Auth
  Stream section. Update `drafts/draft-lcurley-moq-auth.md` only where it
  refers to lite. `just drafts check`.
- Tests: a regression test per side (Rust and JS). An oversized update after
  an AUTH_OK reports `Unsupported` and revokes the grant. Also add an interop
  case, run with `just test interop --all`.

Public API: none new (`Error::Unsupported` exists). Wire: a new lite session
code, and a lower lite message ceiling.

## Related

- [AUTH endings](/quest/m1/auth/error-codes.md) - a malformed or out-of-range AUTH_ERROR code is refused on the same path
