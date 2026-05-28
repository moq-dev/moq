//! WebRTC ↔ MoQ gateway.
//!
//! Bridges WHIP (RFC 9725) and WHEP between WebRTC peers and
//! [`moq_net`] broadcasts. The crate is split along two orthogonal axes
//! so all four combinations can land independently:
//!
//! | | RTP-in (ingest into MoQ) | RTP-out (egress from MoQ) |
//! |---|---|---|
//! | HTTP server | [`server::publish_router`] (WHIP server) | [`server::subscribe_router`] (WHEP server, 501) |
//! | HTTP client | [`Client::subscribe`] (WHEP client) | [`Client::publish`] (WHIP client, 501) |
//!
//! The two HTTP-client paths and the two HTTP-server paths share a single
//! [`session::Session`] driver and the same per-codec bridges in [`codec`];
//! the per-direction trait split lives in [`session::MediaSink`] /
//! [`session::MediaSource`].
//!
//! ## Bitstream gotcha
//!
//! The WebRTC ↔ MoQ shape conversion for H.264 is handled by `moq-mux`'s
//! `Avc3` importer: str0m hands us Annex-B (start-code NALs with inline
//! SPS/PPS) and that's exactly what the importer wants, so no extra
//! transform is needed in the gateway. Opus, VP8, and VP9 pass through.

pub mod client;
pub mod codec;
pub mod egress;
mod error;
pub mod ingest;
pub mod sdp;
pub mod server;
pub mod session;

pub use client::Client;
pub use error::*;
pub use server::Server;
