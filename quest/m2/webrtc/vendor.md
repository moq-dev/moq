# [L] Vendor and trim the WebRTC stack

## Goal

moq-webrtc builds str0m, dimpl, `is`, and sctp-proto from private in-tree modules instead of crates.io, with the code MoQ doesn't use removed, DTLS 1.2 only, and every existing WHIP, WHEP, data-channel, and browser interop test still passing.

## Plan

Decided 2026-10-30 with the maintainer (planning interview); see the [epic](/quest/m2/webrtc/README.md) for what goes and why.

- Vendor the exact versions in the lockfile (str0m 0.24.1, str0m-proto, `is` 0.11.1, dimpl 0.7.4, sctp-proto 0.10.5), first unchanged in one commit so later diffs show only our changes, then trim.
- Remove: str0m's `bwe/`, leaky pacer, H.266, RED, DTMF, G.7xx and comfort noise, VLA and simulcast layers, RTCP XR and app feedback, stats, `_internal_test_exports`, the crypto backends other than the one rind will replace (keep the provider trait until [moq-rtc on rind](/quest/m2/rind/rtc.md)); dimpl's `dtls13/` and `auto.rs`. Keep upstream's tests for what remains.
- Keep the license files and record each upstream version in the module docs.
- Drop the crates.io dependencies on str0m and friends from the workspace once nothing uses them.

Public API: none beyond what [rename](/quest/m2/webrtc/rename.md) changed. Wire: unchanged; the same DTLS 1.2 suites and SRTP profiles are offered.

## Required

- [Rename moq-rtc to moq-webrtc](/quest/m2/webrtc/rename.md) - the crate the stack is vendored into
