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
//! [`Response::run`] to drive the media session for its lifetime. The routers and
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
pub mod rate;
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
mod tests {
	use std::time::Duration;

	/// The next route and whether it is active.
	async fn next_update(announced: &mut moq_net::announce::Consumer) -> Option<(moq_net::announce::Announce, bool)> {
		match announced.next().await? {
			moq_net::announce::Event::Start(route)
			| moq_net::announce::Event::Update(route)
			| moq_net::announce::Event::Restart(route)
			| moq_net::announce::Event::Takeover { announce: route, .. } => Some((route, true)),
			moq_net::announce::Event::End(route) => Some((route, false)),
		}
	}

	use axum::Router;
	use bytes::Bytes;

	use crate::codec::{Bridge, Frame, Track};
	use crate::{Client, Server, client, server};

	const TIMEOUT: Duration = Duration::from_secs(10);
	const OPUS_PACKET: &[u8] = &[0xfc, 0xff, 0xfe];
	const OLD_PACKET: &[u8] = &[0xfc, 0x01, 0x02];
	const NEW_PACKET: &[u8] = &[0xfc, 0x99, 0x99];

	/// Serve `app` on a loopback port.
	async fn serve(app: Router) -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
		let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
			.await
			.expect("bind HTTP listener");
		let address = listener.local_addr().expect("HTTP listener address");
		let http = tokio::spawn(async move { axum::serve(listener, app).await.expect("serve HTTP") });
		(address, http)
	}

	/// Subscribe to the first Opus rendition in `broadcast`'s catalog.
	async fn opus_output(broadcast: &moq_net::broadcast::Consumer) -> Track {
		let catalog_track = broadcast
			.track(hang::Catalog::DEFAULT_NAME)
			.expect("output catalog track")
			.subscribe(hang::Catalog::default_subscription())
			.await
			.expect("subscribe to output catalog");
		let mut catalogs = moq_mux::catalog::hang::Consumer::<()>::new(catalog_track);
		let catalog = kio::clock::timeout(TIMEOUT, catalogs.next())
			.await
			.expect("output catalog timed out")
			.expect("read output catalog")
			.expect("output catalog ended");
		let (track_name, config) = catalog.audio.renditions.iter().next().expect("output Opus rendition");
		let track = broadcast
			.track(track_name)
			.expect("output Opus track")
			.subscribe(None)
			.await
			.expect("subscribe to output Opus track");
		Track::opus(track, config).expect("output Opus container")
	}

	async fn next_payload(track: &mut Track) -> Vec<u8> {
		let frame = kio::clock::timeout(TIMEOUT, track.next())
			.await
			.expect("output Opus packet timed out")
			.expect("read output Opus packet")
			.expect("output Opus track ended");
		frame.payload.to_vec()
	}

	fn push(bridge: &mut dyn Bridge, timestamp_us: u64, payload: &'static [u8]) {
		bridge
			.push(Frame {
				timestamp_us,
				payload: Bytes::from_static(payload),
			})
			.expect("publish source packet");
	}

	/// Publish an Opus instance of `source` under `epoch`, keeping its catalog so the test can
	/// kill the instance.
	fn publish_opus(
		origin: &moq_net::origin::Producer,
		epoch: moq_net::Epoch,
	) -> (Box<dyn Bridge>, moq_mux::catalog::Producer) {
		let broadcast = origin.create_broadcast("source").expect("create source broadcast");
		broadcast
			.announce(moq_net::origin::Route::default().with_epoch(epoch))
			.expect("announce source broadcast");
		let catalog =
			moq_mux::catalog::Producer::new(&mut broadcast.broadcast().clone(), moq_mux::catalog::Config::default())
				.expect("create source catalog");
		let bridge = crate::codec::opus::Bridge::new(broadcast.broadcast().clone(), catalog.clone(), 48_000, 2)
			.expect("create Opus bridge");
		(Box::new(bridge), catalog)
	}

	/// Publish a replacement instance of `source`, returning once the origin serves it.
	async fn take_over(origin: &moq_net::origin::Producer) -> (Box<dyn Bridge>, moq_mux::catalog::Producer) {
		let epoch = moq_net::Epoch::mint();
		let mut instances = moq_mux::follow::Instances::new(&origin.consume(), "source", Some(&epoch)).unwrap();
		let (mut new, catalog) = publish_opus(origin, epoch);
		push(&mut *new, 20_000, NEW_PACKET);
		let started = kio::clock::timeout(TIMEOUT, instances.next())
			.await
			.expect("the replacement never took over")
			.unwrap();
		assert!(matches!(started, moq_mux::follow::Event::Start(_)));
		(new, catalog)
	}

	/// Kill an instance the way a dropped publisher session does.
	fn kill(bridge: Box<dyn Bridge>, catalog: moq_mux::catalog::Producer) {
		let err = moq_net::Error::Transport("connection lost".into());
		bridge.abort(err.clone());
		catalog.abort(err);
	}

	/// A WHEP viewer stays on the instance its session resolved: a publisher taking over the
	/// path never reaches it, so the session never resets its timestamps or codec state, and it
	/// fails once that instance's route dies.
	#[tokio::test]
	async fn a_whep_viewer_stays_on_its_instance_until_the_route_dies() {
		let origin = moq_tokio::origin::spawn();
		let (mut old, old_catalog) = publish_opus(&origin, moq_net::Epoch::mint());
		push(&mut *old, 20_000, OPUS_PACKET);

		let server = Server::new(server::Config::default());
		let whep_origin = origin.consume();
		let (sessions_tx, mut sessions) = tokio::sync::mpsc::channel(1);
		let app = Router::new().route(
			"/whep/source",
			axum::routing::post(move |offer: String| {
				let server = server.clone();
				let origin = whep_origin.clone();
				let sessions = sessions_tx.clone();
				async move {
					let response = server::whep::accept(&server, &origin, moq_net::Path::from_static("source"), &offer)
						.await
						.unwrap();
					let answer = response.answer.clone();
					sessions.send(tokio::spawn(response.run())).await.unwrap();
					answer
				}
			}),
		);
		let (address, http) = serve(app).await;

		let output_origin = moq_tokio::origin::spawn();
		let output = output_origin
			.create_broadcast("output")
			.expect("create output broadcast");
		let output_consumer = output.consume();
		let client = Client::new(client::Config::default());
		let url = format!("http://{address}/whep/source").parse().expect("WHEP URL");
		let fill = output.broadcast().clone();
		let viewer = tokio::spawn(async move { client.subscribe(url, fill).await });
		let session = kio::clock::timeout(TIMEOUT, sessions.recv())
			.await
			.expect("WHEP negotiation timed out")
			.unwrap();
		let mut played = opus_output(&output_consumer).await;
		assert_eq!(next_payload(&mut played).await, OPUS_PACKET);

		let _new = take_over(&origin).await;
		push(&mut *old, 40_000, OLD_PACKET);
		assert_eq!(
			next_payload(&mut played).await,
			OLD_PACKET,
			"the viewer left its instance"
		);

		kill(old, old_catalog);
		let ended = kio::clock::timeout(TIMEOUT, session)
			.await
			.expect("the session outlived its route")
			.unwrap();
		assert!(ended.is_err(), "the session ended cleanly");
		viewer.abort();
		http.abort();
	}

	/// A WHIP push stays on the instance it resolved the same way, and
	/// [`Client::publish`] fails once that instance's route dies.
	#[tokio::test]
	async fn a_whip_push_stays_on_its_instance_until_the_route_dies() {
		let origin = moq_tokio::origin::spawn();
		let (mut old, old_catalog) = publish_opus(&origin, moq_net::Epoch::mint());
		push(&mut *old, 20_000, OPUS_PACKET);

		let server = Server::new(server::Config::default());
		let ingest_origin = moq_tokio::origin::spawn();
		let whip_origin = ingest_origin.clone();
		let app = Router::new().route(
			"/whip/ingested",
			axum::routing::post(move |offer: String| {
				let server = server.clone();
				let origin = whip_origin.clone();
				async move {
					let response = server::whip::accept(
						&server,
						server::whip::Request::new(&origin, moq_net::Path::from_static("ingested"), &offer),
					)
					.await
					.unwrap();
					let answer = response.answer.clone();
					tokio::spawn(response.run());
					answer
				}
			}),
		);
		let (address, http) = serve(app).await;

		let client = Client::new(client::Config::default());
		let url = format!("http://{address}/whip/ingested").parse().expect("WHIP URL");
		let source = origin.consume();
		let broadcast = source.request("source", None).await.unwrap();
		let source = moq_mux::Source::new(source, broadcast);
		let pushed = tokio::spawn(async move { client.publish(url, source).await });
		let ingested = ingest_origin.consume();
		let ingested = ingested.follow("ingested", None).unwrap().available().await.unwrap();
		let mut played = opus_output(&ingested).await;
		assert_eq!(next_payload(&mut played).await, OPUS_PACKET);

		let _new = take_over(&origin).await;
		push(&mut *old, 40_000, OLD_PACKET);
		assert_eq!(
			next_payload(&mut played).await,
			OLD_PACKET,
			"the push left its instance"
		);

		kill(old, old_catalog);
		let pushed = kio::clock::timeout(TIMEOUT, pushed)
			.await
			.expect("the push outlived its route")
			.unwrap();
		assert!(pushed.is_err(), "the push ended cleanly");
		http.abort();
	}

	/// Serve WHIP at `/whip/ingested` under `rate`, sending each session's task out.
	async fn serve_capped_whip(
		rate: crate::rate::Limit,
	) -> (
		std::net::SocketAddr,
		tokio::task::JoinHandle<()>,
		tokio::sync::mpsc::Receiver<tokio::task::JoinHandle<crate::Result<()>>>,
	) {
		let server = Server::new(server::Config::default());
		let origin = moq_tokio::origin::spawn();
		let (sessions_tx, sessions) = tokio::sync::mpsc::channel(1);
		let app = Router::new().route(
			"/whip/ingested",
			axum::routing::post(move |offer: String| {
				let (server, origin, rate, sessions) =
					(server.clone(), origin.clone(), rate.clone(), sessions_tx.clone());
				async move {
					let request = server::whip::Request::new(&origin, moq_net::Path::from_static("ingested"), &offer)
						.with_rate(rate);
					let response = server::whip::accept(&server, request).await.unwrap();
					let answer = response.answer.clone();
					sessions.send(tokio::spawn(response.run())).await.unwrap();
					answer
				}
			}),
		);
		let (address, http) = serve(app).await;
		(address, http, sessions)
	}

	/// Push the `source` broadcast in `origin` to the WHIP server at `address`.
	async fn push_whip(
		origin: &moq_net::origin::Producer,
		address: std::net::SocketAddr,
	) -> tokio::task::JoinHandle<crate::Result<()>> {
		let client = Client::new(client::Config::default());
		let url = format!("http://{address}/whip/ingested").parse().expect("WHIP URL");
		let source = origin.consume();
		let broadcast = source.request("source", None).await.unwrap();
		let source = moq_mux::Source::new(source, broadcast);
		tokio::spawn(async move { client.publish(url, source).await })
	}

	/// A capped WHIP session sends its publisher REMB at nine tenths of the cap, follows a
	/// live update at once, lifts the ceiling with the cap, and leaves a publisher under the
	/// cap running.
	#[tokio::test]
	#[tracing_test::traced_test]
	async fn a_capped_whip_publisher_is_steered_by_remb() {
		/// Wait for the logs to contain `line`: the REMB is only visible in the client's.
		async fn logged(line: &str) {
			kio::clock::timeout(TIMEOUT, async {
				while !logs_contain(line) {
					kio::clock::sleep(Duration::from_millis(20)).await;
				}
			})
			.await
			.unwrap_or_else(|_| panic!("never logged {line}"));
		}

		let origin = moq_tokio::origin::spawn();
		let broadcast = origin.create_broadcast("source").unwrap();
		broadcast.announce(moq_net::origin::Route::default()).unwrap();
		let catalog =
			moq_mux::catalog::Producer::new(&mut broadcast.broadcast().clone(), moq_mux::catalog::Config::default())
				.unwrap();
		let mut vp8 = crate::codec::vp8::Bridge::new(broadcast.broadcast().clone(), catalog).unwrap();
		// A VP8 keyframe header for 320x240.
		push(
			&mut vp8,
			0,
			&[0x10, 0x00, 0x00, 0x9d, 0x01, 0x2a, 0x40, 0x01, 0xf0, 0x00],
		);

		let rate = crate::rate::Limit::new(crate::rate::Cap::new(moq_net::bandwidth::Rate::from_mbps(2)));
		let (address, http, mut sessions) = serve_capped_whip(rate.clone()).await;
		let pushed = push_whip(&origin, address).await;
		let session = kio::clock::timeout(TIMEOUT, sessions.recv()).await.unwrap().unwrap();

		logged("bps=1800000").await;
		rate.set(crate::rate::Cap::new(moq_net::bandwidth::Rate::from_mbps(1)));
		logged("bps=900000").await;
		// Lifting the cap lifts the publisher's ceiling rather than leaving the last one.
		rate.set(None);
		logged(&format!("bps={}", str0m::bwe::Bitrate::MAX.as_u64())).await;
		assert!(!session.is_finished(), "a publisher under the cap was closed");
		assert!(!pushed.is_finished(), "a publisher under the cap stopped");
		session.abort();
		pushed.abort();
		http.abort();
	}

	/// A publisher that ignores the steering, sending media past the cap plus its burst, is
	/// closed with the typed error.
	#[tokio::test]
	async fn a_whip_publisher_past_its_cap_plus_burst_is_closed() {
		let origin = moq_tokio::origin::spawn();
		let (mut opus, _catalog) = publish_opus(&origin, moq_net::Epoch::mint());
		// 8 kbps with a tenth of a second of burst allows 100 bytes up front.
		let cap = crate::rate::Cap::new(moq_net::bandwidth::Rate::from_kbps(8)).with_burst(Duration::from_millis(100));
		let (address, http, mut sessions) = serve_capped_whip(crate::rate::Limit::new(cap)).await;
		push(&mut *opus, 20_000, &[0xfc; 1000]);
		let pushed = push_whip(&origin, address).await;

		let session = kio::clock::timeout(TIMEOUT, sessions.recv()).await.unwrap().unwrap();
		let ended = kio::clock::timeout(TIMEOUT, session)
			.await
			.expect("the publisher past its cap stayed open")
			.unwrap();
		assert!(
			matches!(ended, Err(crate::Error::RateLimited { cap }) if cap == moq_net::bandwidth::Rate::from_kbps(8)),
			"{ended:?}"
		);
		pushed.abort();
		http.abort();
	}

	#[tokio::test]
	#[tracing_test::traced_test]
	async fn whip_and_whep_round_trip_opus() {
		let source_origin = moq_tokio::origin::spawn();
		let source_consumer = source_origin.consume();
		let mut announcements = source_consumer.announced();
		let source = source_origin
			.create_broadcast("source")
			.expect("create source broadcast");
		source
			.announce(moq_net::origin::Route::default())
			.expect("announce source broadcast");
		let catalog =
			moq_mux::catalog::Producer::new(&mut source.broadcast().clone(), moq_mux::catalog::Config::default())
				.expect("create source catalog");
		let mut opus = crate::codec::opus::Bridge::new(source.broadcast().clone(), catalog, 48_000, 2)
			.expect("create Opus bridge");
		Bridge::push(
			&mut opus,
			Frame {
				timestamp_us: 20_000,
				payload: Bytes::from_static(OPUS_PACKET),
			},
		)
		.expect("publish source packet");
		let (announcement, announcement_active) = kio::clock::timeout(TIMEOUT, next_update(&mut announcements))
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
						let response = server::whip::accept(
							&server,
							server::whip::Request::new(&origin, moq_net::Path::from_static("ingested"), &offer),
						)
						.await
						.unwrap();
						let answer = response.answer.clone();
						let id = response.resource_id.clone();
						let task = tokio::spawn(response.run());
						sources.send((id, task)).await.unwrap();
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
						let response =
							server::whep::accept(&server, &origin, moq_net::Path::from_static("ingested"), &offer)
								.await
								.unwrap();
						let answer = response.answer.clone();
						let id = response.resource_id.clone();
						let task = tokio::spawn(response.run());
						sources.send((id, task)).await.unwrap();
						answer
					}
				}),
			);
		let (address, http) = serve(app).await;

		let client = Client::new(client::Config::default());
		// Credentials in the dialed URLs must never reach the client's connect logs.
		let whip = format!("http://user:pass@{address}/whip/ingested?jwt=secret")
			.parse()
			.expect("WHIP URL");
		// Each client call resolves once its session ends, so the test drives them aside.
		let whip_client = tokio::spawn({
			let client = client.clone();
			async move {
				let broadcast = source_consumer.request("source", None).await?;
				client
					.publish(whip, moq_mux::Source::new(source_consumer, broadcast))
					.await
			}
		});
		let (whip_id, whip_task) = kio::clock::timeout(TIMEOUT, sources_rx.recv())
			.await
			.expect("WHIP negotiation timed out")
			.unwrap();

		let output_origin = moq_tokio::origin::spawn();
		let output = output_origin
			.create_broadcast("output")
			.expect("create output broadcast");
		let output_consumer = output.consume();
		let whep = format!("http://user:pass@{address}/whep/ingested?jwt=secret")
			.parse()
			.expect("WHEP URL");
		let fill = output.broadcast().clone();
		let whep_client = tokio::spawn(async move { client.subscribe(whep, fill).await });
		let (whep_id, whep_task) = kio::clock::timeout(TIMEOUT, sources_rx.recv())
			.await
			.expect("WHEP negotiation timed out")
			.unwrap();

		let mut opus = opus_output(&output_consumer).await;
		assert_eq!(next_payload(&mut opus).await, OPUS_PACKET);

		assert!(logs_contain("whip client connected"));
		assert!(logs_contain("whep client connected"));
		for secret in ["jwt", "secret", "user:pass"] {
			assert!(!logs_contain(secret), "client logs leaked {secret}");
		}
		assert!(server.terminate(&whep_id));
		assert!(server.terminate(&whip_id));
		kio::clock::timeout(TIMEOUT, whep_task).await.unwrap().unwrap().unwrap();
		// The DELETE ended the ingest cleanly, so it serves the finished broadcast for the
		// linger; dropping the session, as an interrupt does, ends that at once.
		let ingested = server_origin.consume().request("ingested", None).await.unwrap();
		assert!(!ingested.is_closed(), "the finished ingest lingers");
		whip_task.abort();
		let stopped = kio::clock::timeout(TIMEOUT, whip_task).await.unwrap();
		assert!(stopped.unwrap_err().is_cancelled());
		assert!(ingested.is_closed(), "the interrupt closes the finished ingest");
		assert!(!server.terminate(&whip_id));
		assert!(!server.terminate(&whep_id));
		whip_client.abort();
		whep_client.abort();
		http.abort();
		http.await.unwrap_err();
	}
}
