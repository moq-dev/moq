# We own our WebRTC stack

## Goal

All of MoQ's WebRTC lives in one published crate, `moq-webrtc`, holding the WHIP/WHEP gateway, the ICE driver, the native data-channel transport (`p2p`), and vendored copies of str0m, dimpl, `is`, and sctp-proto. Fixes to the stack land in normal MoQ PRs instead of waiting on upstream, and code MoQ doesn't use is gone.

Non-goals: TURN; DTLS 1.3; replacing a vendored layer before there's a reason to.

## Plan

Decided 2026-10-30 with the maintainer (planning interview), after the str0m workarounds piled up: one `b=` line per SDP media section ([moq-dev/moq#7373](https://github.com/moq-dev/moq/pull/7373)), DTLS retransmits running out before ICE connects ([moq-dev/moq#7379](https://github.com/moq-dev/moq/pull/7379)), no hook to charge or serve NACK resends ([rtc-pacing](/quest/m3/rtc-pacing.md)), and `std::time::Instant` in a `kio::clock` workspace. No upstream reports were approved; vendoring replaces them.

- **Vendor, then replace.** Copy the upstream crates in, trim them, and fix them in-tree. Replacing a layer with our own (SDP, RTP/RTCP, SRTP, SCTP) is a later child once a concrete need appears; none is planned yet.
- **One crate.** The ICE driver (once `moq-ice`) and moq-tokio's former `webrtc` data-channel transport live in `moq-webrtc`, which depends on moq-tokio rather than the reverse, so moq-tokio needs no ICE or SCTP. The vendored crates become private modules, not separate published crates, keeping their MIT/Apache-2.0 notices (str0m's `packet/` carries webrtc-rs's Apache notice).
- **What goes** (from the 2026-10-30 inventory: we use frame mode, full ICE, and DTLS 1.2, and never enable str0m's BWE, pacer, or stats): GCC bandwidth estimation (8k lines; GCC is ported into `moq-congestion` as `moq_congestion::gcc`), the leaky pacer (replaced by moq-congestion's pacer, made public by [rtc-pacing](/quest/m3/rtc-pacing.md)), H.266, RED, DTMF, G.7xx and comfort noise, VLA and simulcast layers, RTCP app feedback, stats, internal test exports, and dimpl's DTLS 1.3 and version auto-selection. RTCP XR stays: its RRTR/DLRR blocks are the receive side's only RTT measurement, which sets NACK retries and the video recovery deadline. Roughly 17k of str0m's 72k lines and 11k of dimpl's 33k.
- **DTLS 1.2 only.** Every browser negotiates 1.2 today, and the vendored rind crypto provider offers 1.2 only to match.
- **SCTP stays**, for browser data channels in [P2P](/quest/m4/p2p/README.md), but compiles only with the data-channel feature, so WHIP/WHEP builds carry none of it. Vendored, it can later be simplified or put on moq-congestion.
- **Upstream fixes:** after vendoring, nobody merges upstream automatically. The vendored upstream commits are recorded in `rs/moq-webrtc/src/vendor/mod.rs`'s `Upstream` section; the [vendor sync](/quest/m3/vendor-sync/webrtc-stack.md) checks the upstream changes once per release cycle for security fixes (malformed SRTP/RTCP input, for example) and ports them.
- Docs a change makes stale (crate names, `doc/lib/rs`, `doc/setup/upgrade.md`, other quests naming a crate or module) change in the PR that makes them stale.

## Related

- [moq-webrtc paces WHEP viewers](/quest/m3/rtc-pacing.md) - adds the NACK hook and the shared pacer on the vendored stack
