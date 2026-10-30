# We own our WebRTC stack

## Goal

All of MoQ's WebRTC lives in one published crate, `moq-webrtc` (today's `moq-rtc` renamed), holding the WHIP/WHEP gateway, the ICE driver from `moq-ice`, the native data-channel transport from `moq-tokio`, and vendored copies of str0m, dimpl, `is`, and sctp-proto. Fixes to the stack land in normal MoQ PRs instead of waiting on upstream, and code MoQ doesn't use is gone.

Non-goals: TURN; DTLS 1.3; replacing a vendored layer before there's a reason to.

## Plan

Decided 2026-10-30 with the maintainer (planning interview), after the str0m workarounds piled up: one `b=` line per SDP media section ([moq-dev/moq#7373](https://github.com/moq-dev/moq/pull/7373)), DTLS retransmits running out before ICE connects ([moq-dev/moq#7379](https://github.com/moq-dev/moq/pull/7379)), no hook to charge or serve NACK resends ([rtc-pacing](/quest/m2/rtc-pacing.md)), and `std::time::Instant` in a moq-clock workspace. No upstream reports were approved; vendoring replaces them.

- **Vendor, then replace.** Copy the upstream crates in, trim them, and fix them in-tree. Replacing a layer with our own (SDP, RTP/RTCP, SRTP, SCTP) is a later child once a concrete need appears; none is planned yet.
- **One crate.** `moq-ice` merges into it, since its only users are `moq-rtc` and moq-tokio's data-channel transport. moq-tokio's `webrtc` feature moves into `moq-webrtc` as well, which breaks the cycle (`moq-webrtc` depends on moq-tokio; moq-tokio would otherwise need ICE and SCTP). The vendored crates become private modules, not separate published crates, keeping their MIT/Apache-2.0 notices (str0m's `packet/` carries webrtc-rs's Apache notice).
- **What goes** (from the 2026-10-30 inventory: we use frame mode, full ICE, and DTLS 1.2, and never enable str0m's BWE, pacer, or stats): GCC bandwidth estimation (8k lines; GCC is being ported into [moq-congestion](/quest/m2/quic-realtime/gcc.md)), the leaky pacer (replaced by moq-congestion's pacer, made public by [rtc-pacing](/quest/m2/rtc-pacing.md)), H.266, RED, DTMF, G.7xx and comfort noise, VLA and simulcast layers, RTCP XR and app feedback, stats, internal test exports, and dimpl's DTLS 1.3 and version auto-selection. Roughly 17k of str0m's 72k lines and 11k of dimpl's 33k.
- **DTLS 1.2 only.** Every browser negotiates 1.2 today. [moq-rtc on rind](/quest/m2/rind/rtc.md) narrows to 1.2 to match.
- **SCTP stays**, for browser data channels in [P2P](/quest/m2/p2p/README.md), but compiles only with the data-channel feature, so WHIP/WHEP builds carry none of it. Vendored, it can later be simplified or put on moq-congestion.
- **Upstream fixes:** after vendoring, nobody merges upstream automatically. Record the vendored upstream versions in the crate docs; the release-cut audit checks the upstream changelogs for security fixes (malformed SRTP/RTCP input, for example) and ports them.
- Docs a change makes stale (crate names, `doc/lib/rs`, `doc/setup/upgrade.md`, other quests naming `moq-rtc` or `moq-ice`) change in the PR that makes them stale.

## Required

- [Rename moq-rtc to moq-webrtc](/quest/m2/webrtc/rename.md) - one crate holds the gateway, the ICE driver, and the data-channel transport
- [Vendor and trim the stack](/quest/m2/webrtc/vendor.md) - str0m, dimpl, `is`, and sctp-proto live in moq-webrtc with unused code removed
- [Several bandwidth lines per media section](/quest/m2/webrtc/sdp-bandwidth.md) - answers carry both `b=TIAS` and `b=AS` without splicing
- [DTLS waits for ICE](/quest/m2/webrtc/dtls-after-ice.md) - DTLS flights start once ICE nominates a pair, so a late ICE path can't exhaust them
- [The stack takes moq-clock time](/quest/m2/webrtc/clock.md) - the vendored code takes `moq_clock::Instant`, so sim tests drive it directly

## Related

- [moq-rtc paces WHEP viewers](/quest/m2/rtc-pacing.md) - adds the NACK hook and the shared pacer on the vendored stack
- [moq-rtc on rind](/quest/m2/rind/rtc.md) - the vendored DTLS and SRTP take rind's crypto
- [GCC candidate](/quest/m2/quic-realtime/gcc.md) - where bandwidth estimation lives instead of str0m
