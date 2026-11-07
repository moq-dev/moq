# [XS] A typed DTLS handshake timeout error

## Goal

A DTLS handshake that times out after ICE connects surfaces as its own error
variant in moq-webrtc's ICE module and gateway, so callers match it instead of the text
`rtc error: DTLS error: timeout: handshake` inside `Error::Rtc(String)`.
Session-end handling and logging treat it like an ICE timeout: `session::end`
logs it at debug as abandoned-session churn, not as a warning.

## Plan

Today `Agent::drain` (`rs/moq-webrtc/src/ice/agent.rs`) maps any DTLS failure before
ICE connects to `Error::Timeout`, and stringifies everything else. The vendored
str0m's `dtls.rs` carries dimpl's typed `Error::Timeout(TimeoutError::Handshake)`
through as `RtcError::Dtls(DtlsError::CryptoError(CryptoError::DtlsImpl(_)))`,
where upstream flattened it into `CryptoError::Other(String)`.

- Classify it once at the str0m boundary in moq-webrtc's ICE module, the only place that sees
  str0m's error, by matching that variant; add no text match.
- Add the variant to moq-webrtc's `ice::Error` and `moq_webrtc::Error`, named by role (for
  example `DtlsTimeout`), and map it in `moq-webrtc`'s session loop and its
  `p2p` transport, which also matches moq-webrtc's `ice::Error`.
- Decide in the PR whether the variant stays distinct from `IceTimeout` or
  folds into one establishment-timeout variant, and record why. Distinct is
  recommended: an ICE timeout means no path, a DTLS timeout means a path that
  never finished the handshake.

Public API: additive variants on non-exhaustive error enums. Wire: none.

## Related

- [DTLS waits for ICE](/quest/m3/webrtc/dtls-after-ice.md) - after it, every DTLS handshake timeout comes after nomination, which is the case this types
