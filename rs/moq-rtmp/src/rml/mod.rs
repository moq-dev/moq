//! Networking-agnostic RTMP: handshake, the chunk/message codec, and the
//! high-level `ServerSession`/`ClientSession` state machines. RTMP carries FLV,
//! which moq-mux demuxes into MoQ.
//!
//! Vendored from the unmaintained `rml_rtmp` 0.8.0
//! (github.com/KallDrexx/rust-media-libs, MIT, Copyright (c) Matthew Shapiro;
//! see LICENSE in this directory). The `amf0` module vendors rml_amf0 0.3.0
//! from the same project and license. Local patches include:
//! - borrowed AMF0 receive views, bounded nesting, and a nonzero incoming chunk size.
//! - independent per-CSID message assembly with bounded connection state.
//! - `sessions::ServerSession::set_connect_response_properties`, so the gateway
//!   can advertise enhanced-RTMP capabilities in the connect `_result`.
//! - guards against two reachable panics on malformed untrusted input (short
//!   AMF0 command in `messages::types::amf0_command`, empty `onMetaData` array in
//!   `sessions::server`).
//!
//! # Upstream
//!
//! Checked upstream `master` through
//! [`953fc41df2369d64e9c677369c706da797592e49`](https://github.com/KallDrexx/rust-media-libs/commit/953fc41df2369d64e9c677369c706da797592e49)
//! (2023-05-31) on 2026-10-31, starting from rml_rtmp 0.8.0
//! ([`a25df6bc`](https://github.com/KallDrexx/rust-media-libs/commit/a25df6bc35cdc33f21f7a35efc97e700dcc8fa17))
//! and rml_amf0 0.3.0. All four subsequent commits were checked:
//! - [`ec5935a9`](https://github.com/KallDrexx/rust-media-libs/commit/ec5935a97005c7f8a4e75590b95445758a57cccd)
//!   changes connection IDs only in an example we do not vendor.
//! - [`4508e466`](https://github.com/KallDrexx/rust-media-libs/commit/4508e4663798a470fcb9f087983c72199c8ca475)
//!   fixes short reads/writes. `chunk_io::serializer::add_message_payload`
//!   already uses `write_all`; AMF0's borrowed decoder requires the entire
//!   string/property name via checked slice splitting, with truncation tests.
//!   Its remaining changes are reference cleanup or unvendored examples/tools.
//! - [`bac6a385`](https://github.com/KallDrexx/rust-media-libs/commit/bac6a3857f056671113169bcb4db0aeff4177951)
//!   simplifies an unvendored example's socket writer.
//! - [`953fc41d`](https://github.com/KallDrexx/rust-media-libs/commit/953fc41df2369d64e9c677369c706da797592e49)
//!   makes publish modes case-insensitive, already present in `sessions::server`.
//!
//! No code ports were needed. The upstream repository advisories, GitHub
//! Advisory Database (both crate names), and RustSec advisory database had no
//! published advisories for rml_rtmp or rml_amf0 on that date. Upstream has had
//! no commits since the checked head.
//!
//! Kept close to upstream (its own tests included), so the whole module opts out
//! of the workspace's `-D warnings` clippy/rustdoc gate rather than churning the
//! vendor to satisfy our lints.
#![allow(warnings, reason = "vendored rml code")]
#![allow(clippy::all, reason = "vendored rml code")]
#![allow(clippy::pedantic, reason = "vendored rml code")]
#![allow(clippy::nursery, reason = "vendored rml code")]
#![allow(rustdoc::all, reason = "vendored rml code")]
#![deny(clippy::disallowed_methods, clippy::disallowed_types)]

pub mod amf0;

#[cfg(test)]
#[macro_use]
mod test_utils {
	#[macro_use]
	pub mod assert_vec_match_macro;
	#[macro_use]
	pub mod assert_vec_contains_macro;
}

pub mod chunk_io;
pub mod handshake;
pub mod messages;
pub mod sessions;
pub mod time;
