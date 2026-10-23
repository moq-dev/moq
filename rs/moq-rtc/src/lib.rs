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
//! Build a [`Server`] and pass your own origin handles when merging
//! [`Server::publish_router`] / [`Server::subscribe_router`] into your own axum
//! app, or dial out with [`Client`]. A command-line interface is provided by the
//! `moq-cli` binary, on top of this library.
//!
//! The bundled routers are unauthenticated: they derive the broadcast name from
//! the request path. To own the HTTP route and authorize requests yourself
//! (resolving the broadcast name from a verified token), skip the routers and
//! call [`whip::accept`] (ingest) / [`whep::accept`] (egress) from your own
//! handler. Return the [`Response::answer`] in your HTTP response, then run
//! [`Response::run`] to drive the media session for its lifetime. Clone
//! [`Response::wire`] before running it to sample the peer's cumulative UDP
//! payload bytes, including ICE, DTLS, RTP, RTCP, and retransmissions. Client
//! dials return the same [`wire::Source`]; its counters remain readable after
//! the session ends. [`wire::Source::stats`] can back an embedder's wire-source
//! registry without retaining the session or its socket. The routers and
//! the `axum` re-export sit behind the default `server` feature, so such an
//! embedder can drop them with `default-features = false`.
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
pub mod wire;

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

/// Re-export of the HTTP router stack, so consumers can merge the [`axum::Router`]
/// returned by [`Server::publish_router`] / [`Server::subscribe_router`] into their
/// own app without adding their own axum dependency (and risking a version mismatch). A major axum bump is therefore
/// a breaking change for this crate. Only with the `server` feature.
#[cfg(feature = "server")]
pub use axum;

/// Re-export of the URL type, so consumers can build the [`url::Url`] that
/// [`Client::subscribe`] / [`Client::publish`] dial without adding their own url
/// dependency (and risking a version mismatch). A major url bump is therefore a
/// breaking change for this crate.
pub use url;

pub use client::Client;
pub use error::*;
pub use server::{Response, Server, whep, whip};

#[cfg(all(test, feature = "server"))]
#[allow(clippy::disallowed_methods, clippy::disallowed_types)]
mod tests {
	use std::time::Duration;

	/// The next route and whether it is active.
	async fn next_update(announced: &mut moq_net::announce::Consumer) -> Option<(moq_net::announce::Announce, bool)> {
		match announced.next().await? {
			moq_net::announce::Event::Start(route)
			| moq_net::announce::Event::Update(route)
			| moq_net::announce::Event::Restart(route) => Some((route, true)),
			moq_net::announce::Event::End(route) => Some((route, false)),
		}
	}

	use axum::Router;
	use bytes::Bytes;

	use crate::codec::{Bridge, Frame, Track};
	use crate::{Client, Server, client, server};

	const TIMEOUT: Duration = Duration::from_secs(10);
	const OPUS_PACKET: &[u8] = &[0xfc, 0xff, 0xfe];

	#[tokio::test]
	#[tracing_test::traced_test]
	async fn whip_and_whep_round_trip_opus() {
		let source_origin = moq_tokio::origin::spawn();
		let source_consumer = source_origin.consume();
		let mut announcements = source_consumer.announced();
		let mut source = source_origin
			.create_broadcast("source")
			.expect("create source broadcast");
		source
			.announce(moq_net::origin::Route::default())
			.expect("announce source broadcast");
		let catalog = moq_mux::catalog::Producer::new(&mut source, moq_mux::catalog::Config::default())
			.expect("create source catalog");
		let mut opus = crate::codec::opus::Bridge::new(source, catalog, 48_000, 2).expect("create Opus bridge");
		Bridge::push(
			&mut opus,
			Frame {
				timestamp_us: 20_000,
				payload: Bytes::from_static(OPUS_PACKET),
			},
		)
		.expect("publish source packet");
		let (announcement, announcement_active) = tokio::time::timeout(TIMEOUT, next_update(&mut announcements))
			.await
			.expect("source announcement timed out")
			.expect("source origin closed");
		assert_eq!(announcement.prefix.as_str(), "source");
		assert!(announcement_active, "source was unannounced");
		drop(announcements);

		let server_origin = moq_tokio::origin::spawn();
		let server = Server::new(server::Config::default());
		let (sources_tx, mut sources_rx) = tokio::sync::mpsc::channel(2);
		let whip_server = server.clone();
		let whip_origin = server_origin.clone();
		let whip_sources = sources_tx.clone();
		let whep_server = server.clone();
		let whep_origin = server_origin.consume();
		let app = Router::new()
			.route(
				"/whip/ingested",
				axum::routing::post(move |offer: String| {
					let server = whip_server.clone();
					let origin = whip_origin.clone();
					let sources = whip_sources.clone();
					async move {
						let response = server::whip::accept(&server, &origin, "ingested", &offer)
							.await
							.unwrap();
						let answer = response.answer.clone();
						let id = response.resource_id.clone();
						let wire = response.wire.clone();
						let task = tokio::spawn(response.run());
						sources.send((id, wire, task)).await.unwrap();
						answer
					}
				}),
			)
			.route(
				"/whep/ingested",
				axum::routing::post(move |offer: String| {
					let server = whep_server.clone();
					let origin = whep_origin.clone();
					let sources = sources_tx.clone();
					async move {
						let response = server::whep::accept(&server, &origin, "ingested", &offer)
							.await
							.unwrap();
						let answer = response.answer.clone();
						let id = response.resource_id.clone();
						let wire = response.wire.clone();
						let task = tokio::spawn(response.run());
						sources.send((id, wire, task)).await.unwrap();
						answer
					}
				}),
			);
		let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
			.await
			.expect("bind HTTP listener");
		let address = listener.local_addr().expect("HTTP listener address");
		let http = tokio::spawn(async move { axum::serve(listener, app).await.expect("serve HTTP") });

		let client = Client::new(client::Config::default());
		// Credentials in the dialed URLs must never reach the client's connect logs.
		let whip = format!("http://user:pass@{address}/whip/ingested?jwt=secret")
			.parse()
			.expect("WHIP URL");
		let whip_wire = tokio::time::timeout(TIMEOUT, client.publish(whip, source_consumer, "source"))
			.await
			.expect("WHIP negotiation timed out")
			.expect("WHIP negotiation failed");

		let output_origin = moq_tokio::origin::spawn();
		let output = output_origin
			.create_broadcast("output")
			.expect("create output broadcast");
		let output_consumer = output.consume();
		let whep = format!("http://user:pass@{address}/whep/ingested?jwt=secret")
			.parse()
			.expect("WHEP URL");
		let whep_wire = tokio::time::timeout(TIMEOUT, client.subscribe(whep, output))
			.await
			.expect("WHEP negotiation timed out")
			.expect("WHEP negotiation failed");

		assert!(logs_contain("whip client connected"));
		assert!(logs_contain("whep client connected"));
		for secret in ["jwt", "secret", "user:pass"] {
			assert!(!logs_contain(secret), "client logs leaked {secret}");
		}

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
		let (track_name, config) = catalog.audio.renditions.iter().next().expect("output Opus rendition");
		let track = output_consumer
			.track(track_name)
			.expect("output Opus track")
			.subscribe(None)
			.await
			.expect("subscribe to output Opus track");
		let mut opus = Track::opus(track, config).expect("output Opus container");
		let frame = tokio::time::timeout(TIMEOUT, opus.next())
			.await
			.expect("output Opus packet timed out")
			.expect("read output Opus packet")
			.expect("output Opus track ended");
		assert_eq!(frame.payload.as_ref(), OPUS_PACKET);
		let (whip_id, whip_server_wire, whip_task) = sources_rx.recv().await.unwrap();
		let (whep_id, whep_server_wire, whep_task) = sources_rx.recv().await.unwrap();
		for source in [&whip_wire, &whep_wire, &whip_server_wire, &whep_server_wire] {
			let stats = source.stats();
			assert!(
				stats.tx > OPUS_PACKET.len() as u64,
				"includes outgoing ICE/DTLS/RTP/RTCP: {stats:?}"
			);
			assert!(
				stats.rx > OPUS_PACKET.len() as u64,
				"includes incoming ICE/DTLS/RTP/RTCP: {stats:?}"
			);
			assert!(source.stats().tx >= stats.tx);
			assert!(source.stats().rx >= stats.rx);
		}
		assert!(server.terminate(&whep_id));
		assert!(server.terminate(&whip_id));
		tokio::time::timeout(TIMEOUT, whep_task)
			.await
			.unwrap()
			.unwrap()
			.unwrap();
		// The DELETE ended the ingest cleanly, so it serves the finished broadcast until its
		// media is stale; a newer publish at the path ends that linger at once.
		let ingested = server_origin
			.consume()
			.request_broadcast("ingested", None)
			.await
			.unwrap();
		assert!(!ingested.is_closed(), "the finished ingest lingers");
		let _replacement = server_origin
			.publish(
				"ingested",
				moq_net::origin::Route::default().with_epoch(moq_net::Epoch::mint()),
			)
			.unwrap();
		tokio::time::timeout(TIMEOUT, whip_task)
			.await
			.unwrap()
			.unwrap()
			.unwrap();
		let final_whip = whip_server_wire.stats();
		let final_whep = whep_server_wire.stats();
		assert!(!server.terminate(&whip_id));
		assert!(!server.terminate(&whep_id));
		assert_eq!(whip_server_wire.stats(), final_whip);
		assert_eq!(whep_server_wire.stats(), final_whep);
		http.abort();
		http.await.unwrap_err();
	}
}
