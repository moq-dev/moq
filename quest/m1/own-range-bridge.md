# [XS] MoQT never sends moq-lite's own session codes

## Goal

An IETF (MoQT) session close never carries a moq-lite own-range code
(0x30-0x3f). An unmapped own-range `SessionError::Unknown` becomes an
unspecified error (0x1) on MoQT, as the moq-lite draft requires at a bridge.

## Plan

Today Rust IETF closes pass `SessionError::Unknown(0x30..=0x3f)` through
unchanged: `Version::session_code` and the direct `to_code()` calls in
`ietf/session.rs` and `ietf/publisher.rs`. Stream resets are already safe,
since IETF `to_stream_code` maps unknown codes to INTERNAL_ERROR. The relay
never forwards one session's close code onto another, so this is latent and
reachable through `Session::abort(Error::Session(Unknown(0x3x)))`.

Decided 2026-10-12: map it in the shared `SessionError::to_code`, so every
IETF close site is covered, and delete Lite's now-redundant arm in
`lite/error.rs`. Rust only: JS was checked and can't emit these codes
(`abort()` takes no code, IETF closes use fixed codes, and auth rejections
send UNAUTHORIZED or NOT_SUPPORTED).

Tests: an IETF close with `Unknown(0x31)` sends 0x1, a Lite close is
unchanged, and `Unknown(0x20)` (the reserved range) still passes through on
both.

Public API: `SessionError::Unknown(0x30..=0x3f).to_code()` returns 0x1 on
MoQT. Wire: no change for any published version.

## Related

- [Malformed moq-lite input](/quest/m1/lite-malformed-close.md) - the neighbouring Lite close-code rules
