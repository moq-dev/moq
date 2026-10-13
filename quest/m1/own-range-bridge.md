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

Decided 2026-10-12: map it in the IETF session-code encoder, mirroring
`lite::error::to_session_code`, and route `Version::session_code` and the
direct IETF close sites through it. Not in the shared
`SessionError::to_code`: moq-ffi and moq-c expose that value verbatim
(`from_session`), so a received Lite `Unknown(0x31)` would reach every
binding as 0x1. Lite keeps its own arm. Rust only: JS was checked and can't
emit these codes (`abort()` takes no code, IETF closes use fixed codes, and
auth rejections send UNAUTHORIZED or NOT_SUPPORTED).

Tests: an IETF close with `Unknown(0x31)` sends 0x1, a Lite close and
`to_code()` are unchanged, and `Unknown(0x20)` (the reserved range) still passes through on
both.

Public API: none; `SessionError::to_code` is unchanged. Wire: encoding and
negotiated versions are unchanged, but the close code value changes: a MoQT
close carrying `Unknown(0x30..=0x3f)` sends 0x1 instead of that code.

## Related

- [Malformed moq-lite input](/quest/m1/lite-malformed-close.md) - the neighbouring Lite close-code rules
