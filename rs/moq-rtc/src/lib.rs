//! WebRTC ↔ MoQ gateway.
//!
//! Bridges WHIP (RFC 9725) and WHEP between WebRTC peers and
//! [`moq_net`] broadcasts. The crate is split along two orthogonal axes
//! so all four combinations can land independently:
//!
//! | | RTP-in (ingest into MoQ) | RTP-out (egress from MoQ) |
//! |---|---|---|
//! | HTTP server | [`Server::publish_router`] (WHIP server) | [`Server::subscribe_router`] (WHEP server) |
//! | HTTP client | [`Client::subscribe`] (WHEP client) | [`Client::publish`] (WHIP client) |
//!
//! The two HTTP-client paths and the two HTTP-server paths share a single
//! [`session::Session`] driver and the same per-codec adapters in [`codec`];
//! the per-direction split lives in [`session::MediaSink`] (ingest) /
//! [`egress::EgressSource`] (egress).
//!
//! ## Embedding
//!
//! Build a [`Server`] over your own
//! [`OriginProducer`](moq_net::OriginProducer) /
//! [`OriginConsumer`](moq_net::OriginConsumer) and merge
//! [`Server::publish_router`] / [`Server::subscribe_router`] into your own axum
//! app, or dial out with [`Client`]. A command-line interface is provided by the
//! `moq-cli` binary, on top of this library.
//!
//! The bundled routers are unauthenticated: they derive the broadcast name from
//! the request path. To own the HTTP route and authorize requests yourself
//! (resolving the broadcast name from a verified token), skip the routers and
//! call [`whip::accept`] (ingest) / [`whep::accept`] (egress) from your own
//! handler. Return the [`Response::answer`] in your HTTP response, then run
//! [`Response::run`] to drive the media session for its lifetime.
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
pub use server::{Response, Server, whep, whip};
