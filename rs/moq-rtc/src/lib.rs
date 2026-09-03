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
//! [`OriginProducer`](moq_net::origin::Producer) /
//! [`OriginConsumer`](moq_net::origin::Consumer) and merge
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
//! The WebRTC ↔ MoQ shape conversion for H.264 and H.265 is handled by
//! `moq-mux` importers: str0m hands us Annex-B (start-code NALs with inline
//! parameter sets) and that's exactly what the importers want. AV1 uses the
//! shared OBU splitter/importer. Opus, VP8, and VP9 pass through.

#![warn(missing_docs)]

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
mod net;
mod sdp;
mod session;

/// Re-export of the HTTP router stack, so consumers can merge the [`axum::Router`]
/// returned by [`Server::publish_router`] / [`Server::subscribe_router`] (and by
/// [`whip::router`] / [`whep::router`]) into their own app without adding their own
/// axum dependency (and risking a version mismatch). A major axum bump is therefore
/// a breaking change for this crate.
pub use axum;

/// Re-export of the URL type, so consumers can build the [`url::Url`] that
/// [`Client::subscribe`] / [`Client::publish`] dial without adding their own url
/// dependency (and risking a version mismatch). A major url bump is therefore a
/// breaking change for this crate.
pub use url;

pub use client::Client;
pub use error::*;
pub use server::{Response, Server, whep, whip};

#[cfg(test)]
mod tests {
	use std::time::Duration;

	use axum::Router;
	use bytes::Bytes;

	use crate::codec::{Bridge, Frame, Track};
	use crate::{Client, Server, client, server};

	const TIMEOUT: Duration = Duration::from_secs(10);
	const OPUS_PACKET: &[u8] = &[0xfc, 0xff, 0xfe];

	#[tokio::test]
	async fn whip_and_whep_round_trip_opus() {
		let source_origin = moq_tokio::origin::spawn(moq_net::Hop::random());
		let source_consumer = source_origin.consume();
		let mut announcements = source_consumer.announced();
		let mut source = source_origin
			.create_broadcast("source")
			.expect("create source broadcast");
		let _source_announcement = source_origin
			.announce("source", moq_net::origin::Route::default())
			.expect("announce source broadcast");
		let catalog = moq_mux::catalog::Producer::new(&mut source).expect("create source catalog");
		let mut opus = crate::codec::opus::Bridge::new(source, catalog, 48_000, 2).expect("create Opus bridge");
		Bridge::push(
			&mut opus,
			Frame {
				timestamp_us: 20_000,
				payload: Bytes::from_static(OPUS_PACKET),
			},
		)
		.expect("publish source packet");
		let announcement = tokio::time::timeout(TIMEOUT, announcements.next())
			.await
			.expect("source announcement timed out")
			.expect("source origin closed");
		assert_eq!(announcement.prefix.as_path().as_str(), "source");
		assert!(announcement.active, "source was unannounced");
		drop(announcements);

		let server_origin = moq_tokio::origin::spawn(moq_net::Hop::random());
		let server = Server::new(
			server::Config::default(),
			server_origin.clone(),
			server_origin.consume(),
		);
		let app = Router::new()
			.nest("/whip", server.publish_router())
			.nest("/whep", server.subscribe_router());
		let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
			.await
			.expect("bind HTTP listener");
		let address = listener.local_addr().expect("HTTP listener address");
		let http = tokio::spawn(async move { axum::serve(listener, app).await.expect("serve HTTP") });

		let client = Client::new(client::Config::default());
		let whip = format!("http://{address}/whip/ingested").parse().expect("WHIP URL");
		tokio::time::timeout(TIMEOUT, client.publish(whip, source_consumer, "source"))
			.await
			.expect("WHIP negotiation timed out")
			.expect("WHIP negotiation failed");

		let output_origin = moq_tokio::origin::spawn(moq_net::Hop::random());
		let output = output_origin
			.create_broadcast("output")
			.expect("create output broadcast");
		let output_consumer = output.consume();
		let whep = format!("http://{address}/whep/ingested").parse().expect("WHEP URL");
		tokio::time::timeout(TIMEOUT, client.subscribe(whep, output))
			.await
			.expect("WHEP negotiation timed out")
			.expect("WHEP negotiation failed");

		let catalog_track = output_consumer
			.track(hang::Catalog::DEFAULT_NAME)
			.expect("output catalog track")
			.subscribe(hang::Catalog::default_subscription())
			.await
			.expect("subscribe to output catalog");
		let mut catalogs = moq_mux::catalog::hang::Consumer::<()>::new(catalog_track);
		let catalog = tokio::time::timeout(TIMEOUT, catalogs.next())
			.await
			.expect("output catalog timed out")
			.expect("read output catalog")
			.expect("output catalog ended");
		let track_name = catalog.audio.renditions.keys().next().expect("output Opus rendition");
		let track = output_consumer
			.track(track_name)
			.expect("output Opus track")
			.subscribe(None)
			.await
			.expect("subscribe to output Opus track");
		let mut opus = Track::opus(track);
		let frame = tokio::time::timeout(TIMEOUT, opus.next())
			.await
			.expect("output Opus packet timed out")
			.expect("read output Opus packet")
			.expect("output Opus track ended");
		assert_eq!(frame.payload.as_ref(), OPUS_PACKET);

		http.abort();
	}
}
