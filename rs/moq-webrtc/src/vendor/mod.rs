//! The WebRTC stack moq-webrtc runs on: str0m (SDP, RTP/RTCP, SRTP, the `Rtc`
//! state machine) with its ICE agent `is`, DTLS from dimpl, and SCTP from
//! sctp-proto. Each lives here as a private module, trimmed to what MoQ uses, so
//! fixes land in normal MoQ PRs.
//!
//! # Upstream
//!
//! Vendored on 2026-11-07, each module keeping its upstream layout and its
//! license (MIT OR Apache-2.0). The `LICENSE-*` files sit beside `str0m` (whose
//! file covers the other crates from its repository, which ship none of their
//! own), `dimpl`, and `sctp_proto`, and `str0m/packet/LICENSE-APACHE.txt` keeps
//! webrtc-rs's notice on `str0m::packet`:
//! - `str0m` (0.24.1+), `str0m_proto` (0.7.0), `is` (0.11.1), and the test-only
//!   `netem` (0.4.0): <https://github.com/algesten/str0m>
//!   at [`dc317173`](https://github.com/algesten/str0m/commit/dc317173b16fc6ed86633e6b08b229c9195deb87)
//!   (2026-10-26), the main-branch revision on dimpl 0.9 with str0m's pluggable
//!   crypto provider.
//! - `dimpl`: <https://github.com/algesten/dimpl> 0.9.0
//!   ([`c17b3b8`](https://github.com/algesten/dimpl/commit/c17b3b8b971b6eb2ecf7284f5dc0ecb86680ac4d)).
//! - `sctp_proto`: <https://github.com/algesten/sctp-proto> 0.10.7, as published
//!   on crates.io.
//! - `str0m_rind` and dimpl's `crypto::rind`, the stack's crypto providers:
//!   <https://github.com/kixel-ai/rind>'s `rind-str0m` (as of
//!   [`86567aad`](https://github.com/kixel-ai/rind/commit/86567aadabc22d2b2ac23ea534598a0fffc66538),
//!   which targeted the str0m revision above) and `rind-dimpl` 0.1.12, ported to
//!   the rind release the workspace pins.
//!
//! Nobody merges upstream automatically: the release-cut vendor audit reads each
//! upstream's changes since these revisions and ports the fixes MoQ needs.
//!
//! Kept close to upstream (its own tests included), so the vendored modules opt
//! out of the workspace's lints rather than churning them to satisfy ours.
#![allow(warnings, reason = "vendored upstream code")]
#![allow(clippy::all, reason = "vendored upstream code")]
#![allow(clippy::pedantic, reason = "vendored upstream code")]
#![allow(clippy::nursery, reason = "vendored upstream code")]
#![allow(rustdoc::all, reason = "vendored upstream code")]
#![allow(
	clippy::disallowed_methods,
	clippy::disallowed_types,
	reason = "the stack reads std::time::Instant until it takes kio::clock time"
)]

#[path = "dimpl/lib.rs"]
mod dimpl;
#[path = "is/lib.rs"]
mod is;
#[cfg(test)]
#[path = "netem/lib.rs"]
mod netem;
#[path = "sctp_proto/lib.rs"]
mod sctp_proto;
#[path = "str0m/lib.rs"]
pub(crate) mod str0m;
#[path = "str0m_proto/lib.rs"]
mod str0m_proto;
#[path = "str0m_rind/lib.rs"]
mod str0m_rind;
