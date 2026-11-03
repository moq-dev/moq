# [M] moq-rtc on rind

## Goal

moq-rtc and moq-ice run the vendored str0m and dimpl on rind:
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

Decided 2026-10-31 (maintainer): the vendored crates are private modules, so
rind's published adapters can't plug into them. Port the provider code from
moq-dev/rind's `rind-dimpl` and its deleted `rind-str0m` (git history) into
the vendored stack. The vendor pins the revision those adapters targeted, so
the code drops in unchanged. No str0m release is needed.

Public API: none. Wire: unchanged; the same SRTP profiles and DTLS suites are
offered.

## Required

- [Vendor and trim the stack](/quest/m2/webrtc/vendor.md) - where str0m and dimpl live once vendored
