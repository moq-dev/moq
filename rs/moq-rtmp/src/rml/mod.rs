//! Networking-agnostic RTMP: handshake, the chunk/message codec, and the
//! high-level `ServerSession`/`ClientSession` state machines. RTMP carries FLV,
//! which moq-mux demuxes into MoQ.
//!
//! Vendored from the unmaintained `rml_rtmp` 0.8.0
//! (github.com/KallDrexx/rust-media-libs, MIT, Copyright (c) Matthew Shapiro;
//! see LICENSE in this directory), with a small set of local patches:
//! - `sessions::ServerSession::set_connect_response_properties`, so the gateway
//!   can advertise enhanced-RTMP capabilities in the connect `_result`.
//! - guards against two reachable panics on malformed untrusted input (short
//!   AMF0 command in `messages::types::amf0_command`, empty `onMetaData` array in
//!   `sessions::server`).
//!
//! Kept close to upstream (its own tests included), so the whole module opts out
//! of the workspace's `-D warnings` clippy/rustdoc gate rather than churning the
//! vendor to satisfy our lints.
#![allow(warnings)]
#![allow(clippy::all)]
#![allow(clippy::pedantic)]
#![allow(clippy::nursery)]
#![allow(rustdoc::all)]

pub use rml_amf0;

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
