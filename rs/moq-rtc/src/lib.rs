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
//! internal session driver and the same per-codec adapters; the per-direction
//! split lives in the (crate-private) ingest and egress sources.
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
pub mod server;

// Implementation detail modules: these carry the WebRTC/str0m plumbing (str0m
// `Rtc`, `Mid`/`Pt`, tokio channels, raw packet buffers) and are deliberately
// crate-private, so the public surface stays `Client`, `Server`,
// `whip`/`whep::accept`, and `Response`.
mod codec;
mod egress;
mod error;
mod ingest;
mod sdp;
mod session;

/// Re-export of the underlying WebRTC stack, so consumers can name the str0m
/// types that surface through [`Error::Rtc`] / [`Error::RtcInput`] without adding
/// their own str0m dependency (and risking a version mismatch). A major str0m
/// bump is therefore a breaking change for this crate.
pub use str0m;

pub use client::Client;
pub use error::*;
pub use server::{Response, Server, whep, whip};
