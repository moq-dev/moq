# [XS] A typed DTLS handshake timeout error

## Goal

A DTLS handshake that times out after ICE connects surfaces as its own error
variant in `moq-ice` and `moq-rtc` (or `moq-webrtc`, after the
[rename](/quest/m1/webrtc-rename.md)), so callers match it instead of the text
`rtc error: DTLS error: timeout: handshake` inside `Error::Rtc(String)`.
Session-end handling and logging treat it like an ICE timeout: `session::end`
logs it at debug as abandoned-session churn, not as a warning.

## Plan

Today `Agent::drain` (`rs/moq-ice/src/agent.rs`) maps any DTLS failure before
ICE connects to `Error::Timeout`, and stringifies everything else. str0m flattens
dimpl's typed `Error::Timeout(TimeoutError::Handshake)` into
`CryptoError::Other(String)` (`str0m/src/dtls.rs`), so the variant is lost
before moq-ice sees it.

- Classify it once at the str0m boundary in moq-ice, the only place that sees
  str0m's error. Until the [vendored stack](/quest/m1/webrtc-vendor.md) lands,
  that is a text match there, pinned by a test against the crate's real
  message. If the vendoring lands first, carry dimpl's variant through the
  vendored `dtls.rs` typed instead, and add no text match. Otherwise the
  vendoring quest replaces the match; its plan says so.
- Add the variant to `moq_ice::Error` and `moq_rtc::Error`, named by role (for
  example `DtlsTimeout`), and map it in `moq-rtc`'s session loop and moq-tokio's
  `webrtc` transport, which also matches `moq_ice::Error`.
- Decide in the PR whether the variant stays distinct from `IceTimeout` or
  folds into one establishment-timeout variant, and record why. Distinct is
  recommended: an ICE timeout means no path, a DTLS timeout means a path that
  never finished the handshake.

Public API: additive variants on non-exhaustive error enums. Wire: none.

## Related

- [Vendor and trim the WebRTC stack](/quest/m1/webrtc-vendor.md) - replaces this quest's text match with dimpl's typed timeout
- [DTLS waits for ICE](/quest/m2/webrtc/dtls-after-ice.md) - after it, every DTLS handshake timeout comes after nomination, which is the case this types
