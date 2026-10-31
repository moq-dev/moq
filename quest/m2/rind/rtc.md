# [M] moq-rtc on rind

## Goal

moq-rtc and moq-ice run str0m and dimpl on `rind-str0m` and `rind-dimpl`:
DTLS 1.2, SRTP (AES-CM-HMAC-SHA1-80 and AEAD AES-128/256-GCM), STUN
MESSAGE-INTEGRITY, and DTLS certificates through `rind-rcgen`. Their graphs
hold no aws-lc-rs or str0m-aws-lc-rs, and a browser still connects.

## Plan

Decided with the maintainer on 2026-10-26; see the
[epic](/quest/m2/rind/README.md). str0m's workspace dependency drops its
`aws-lc-rs` feature, the test-only HMAC-SHA1 in `server/mux.rs` moves to rind,
and reqwest switches to its no-provider feature. The existing browser
interop tests prove the switch.

Decided 2026-10-30 (maintainer): the stack is vendored into moq-webrtc
([epic](/quest/m2/webrtc/README.md)) with DTLS 1.2 only, so this quest
changes the vendored crypto provider directly and offers no DTLS 1.3.

Public API: none. Wire: unchanged; the same SRTP profiles and DTLS suites are
offered.

## Required

- [rind covers MoQ](/quest/m2/rind/covered.md) - the rind release this switches to

## Related

- [Vendor and trim the stack](/quest/m2/webrtc/vendor.md) - where str0m and dimpl live once vendored
