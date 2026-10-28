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
