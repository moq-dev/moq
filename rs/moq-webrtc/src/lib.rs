//! WebRTC for MoQ: a WHIP/WHEP gateway.
//!
//! The gateway bridges WHIP (RFC 9725) and WHEP between WebRTC peers and
//! [`moq_net`] broadcasts. The crate is split along two orthogonal axes
//! so all four combinations can land independently:
//!
//! | | RTP-in (ingest into MoQ) | RTP-out (egress from MoQ) |
//! |---|---|---|
//! | HTTP server | [`Server::publish_router`] (WHIP server) | [`Server::subscribe_router`] (WHEP server) |
//! | HTTP client | [`Client::pull`] (WHEP client) | [`Client::publish`] (WHIP client) |
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
//! ## Runtime
//!
//! The crate is a tokio edge, built on `moq-tokio`, axum, and tokio sockets. The bundled
//! routers spawn session drivers on the current tokio runtime; `accept` callers run
//! [`Response::run`] themselves.
//!
//! ## Bitstream gotcha
//!
//! The WebRTC ↔ MoQ shape conversion for H.264 and H.265 is handled by
//! `moq-mux` importers: str0m hands us Annex-B (start-code NALs with inline
//! parameter sets) and that's exactly what the importers want. AV1 uses the
//! shared OBU splitter/importer. Opus, VP8, and VP9 pass through.

#![warn(missing_docs)]

pub mod client;
mod rate;
pub mod server;

// Implementation detail modules: these carry the WebRTC/str0m plumbing (str0m
// `Rtc`, `Mid`/`Pt`, tokio channels, raw packet buffers) and are deliberately
// crate-private, so the public surface stays `Client`, `Server`,
// `whip`/`whep::accept`, and `Response`.
mod codec;
mod egress;
mod error;
mod ice;
mod ingest;
mod sdp;
mod session;

pub use client::Client;
pub use error::*;
pub use server::{Response, Server, whep, whip};

#[cfg(all(test, feature = "server"))]
mod tests {
	use std::time::Duration;

	/// The next route and whether it is active.
	async fn next_update(announced: &mut moq_net::announce::Consumer) -> Option<(moq_net::announce::Announce, bool)> {
		match announced.next().await? {
			moq_net::announce::Event::Present(route) => Some((route, true)),
			moq_net::announce::Event::Absent(route) => Some((route, false)),
			event => panic!("unexpected announce event {event:?}"),
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
			.expect("output catalog track");
		let mut catalogs = moq_mux::catalog::hang::Consumer::<()>::new(catalog_track);
		let catalog = kio::clock::timeout(TIMEOUT, catalogs.next())
			.await
			.expect("output catalog timed out")
			.expect("read output catalog")
			.expect("output catalog ended")
			.value;
		let (track_name, config) = catalog.audio.renditions.iter().next().expect("output Opus rendition");
		let track = broadcast
			.track(track_name)
			.expect("output Opus track")
			.subscribe(None)
			.await
			.expect("subscribe to output Opus track");
		Track::opus(track, config).expect("output Opus container")
	}

	fn push(bridge: &mut dyn Bridge, timestamp_us: u64, payload: &'static [u8]) {
		bridge
			.push(Frame {
				timestamp_us,
				payload: Bytes::from_static(payload),
			})
			.expect("publish source packet");
	}

	/// An Opus identity of `source`: its route, its bridge, and its catalog, kept so the test
	/// can kill the identity.
	type Opus = (moq_net::origin::Route, Box<dyn Bridge>, moq_mux::catalog::Producer);

	/// Publish an Opus identity of `source` under `identity`.
	fn publish_opus(origin: &moq_net::origin::Producer, identity: moq_net::Identity) -> Opus {
		let route = origin
			.route(
				"source",
				moq_net::origin::RouteOptions::default().with_identity(identity, 0),
			)
			.expect("publish source broadcast");
		let catalog =
			moq_mux::catalog::Producer::new(&mut route.broadcast().clone(), moq_mux::catalog::Config::default())
				.expect("create source catalog");
		let bridge = crate::codec::opus::Bridge::new(route.broadcast().clone(), catalog.clone(), 48_000, 2)
			.expect("create Opus bridge");
		(route, Box::new(bridge), catalog)
	}

	/// Publish a replacement identity of `source`, returning once the origin serves it.
	async fn take_over(origin: &moq_net::origin::Producer) -> Opus {
		let identity = moq_net::Identity::random();
		let mut identities = origin.consume().follow("source", identity.clone()).unwrap();
		let (route, mut new, catalog) = publish_opus(origin, identity);
		push(&mut *new, 20_000, NEW_PACKET);
		let started = kio::clock::timeout(TIMEOUT, identities.available())
			.await
			.expect("the replacement never took over")
			.unwrap();
		assert!(started.is_some());
		(route, new, catalog)
	}

	/// Kill an identity the way a dropped publisher session does.
	fn kill((route, bridge, catalog): Opus) {
		let err = moq_net::Error::Transport("connection lost".into());
		bridge.abort(err.clone());
		catalog.abort(err);
		drop(route);
	}

	/// A WHEP endpoint for `source` on `origin`, with each accepted session's task.
	type Sessions = tokio::sync::mpsc::Receiver<tokio::task::JoinHandle<crate::Result<()>>>;

	/// Serve WHEP plays of `source` from `origin`, returning the endpoint's URL, its HTTP task,
	/// and the task running each session it accepts.
	async fn serve_whep(origin: &moq_net::origin::Producer) -> (url::Url, tokio::task::JoinHandle<()>, Sessions) {
		let server = Server::bind("127.0.0.1:0".parse().unwrap(), server::Config::default())
			.await
			.unwrap();
		let whep_origin = origin.consume();
		let (sessions_tx, sessions) = tokio::sync::mpsc::channel(1);
		let app = Router::new().route(
			"/whep/source",
			axum::routing::post(move |offer: String| {
				let server = server.clone();
				let origin = whep_origin.clone();
				let sessions = sessions_tx.clone();
				async move {
					let response = server::whep::accept(
						&server,
						server::whep::Request::new(&origin, moq_net::Path::from_static("source"), &offer),
					)
					.await
					.unwrap();
					let answer = response.answer.clone();
					sessions.send(tokio::spawn(response.run())).await.unwrap();
					answer
				}
			}),
		);
		let (address, http) = serve(app).await;
		let url = format!("http://{address}/whep/source").parse().expect("WHEP URL");
		(url, http, sessions)
	}

	/// A WHEP viewer pulling `url` into a fresh broadcast: its task, and its first Opus rendition.
	async fn view(url: url::Url) -> (tokio::task::JoinHandle<crate::Result<()>>, Track) {
		let output_origin = moq_tokio::origin::spawn();
		let output = output_origin
			.route("output", moq_net::origin::RouteOptions::hidden())
			.expect("create output broadcast");
		let output_consumer = output.broadcast().consume();
		let client = Client::new(client::Config::default()).unwrap();
		let mut fill = output.broadcast().clone();
		let catalog = moq_mux::catalog::Producer::new(&mut fill, Default::default()).unwrap();
		let viewer = tokio::spawn(async move {
			let _output = output;
			client.pull(url, &fill, &catalog).await
		});
		(viewer, opus_output(&output_consumer).await)
	}

	/// A bare str0m WHEP viewer of `url`, an audio line alone: the task driving it, which ends
	/// with the server's close, and each payload it receives, until the channel closes with that
	/// task. Unlike [`Client::pull`], which aborts its output when the session ends, it keeps
	/// everything that arrived.
	async fn raw_view(
		url: url::Url,
	) -> (
		tokio::task::JoinHandle<()>,
		tokio::sync::mpsc::UnboundedReceiver<Vec<u8>>,
	) {
		let udp = std::sync::Arc::new(tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap());
		raw_view_on(url, udp).await
	}

	/// [`raw_view`] from `udp`.
	async fn raw_view_on(
		url: url::Url,
		udp: std::sync::Arc<tokio::net::UdpSocket>,
	) -> (
		tokio::task::JoinHandle<()>,
		tokio::sync::mpsc::UnboundedReceiver<Vec<u8>>,
	) {
		use str0m::media::{Direction, MediaKind};

		let local = udp.local_addr().unwrap();
		let socket = crate::ice::Socket::new(udp.clone(), vec![local], crate::ice::reader(udp)).unwrap();
		let mut rtc = crate::session::rtc_config()
			.unwrap()
			.build(kio::clock::now().into_std());
		for addr in socket.locals() {
			rtc.add_local_candidate(str0m::Candidate::host(*addr, "udp").unwrap());
		}
		let mut api = rtc.sdp_api();
		api.add_media(MediaKind::Audio, Direction::RecvOnly, None, None, None);
		let (offer, pending) = api.apply().unwrap();
		let client = Client::new(client::Config::default()).unwrap();
		let answer = (client.http().post(url))
			.header(reqwest::header::CONTENT_TYPE, "application/sdp")
			.body(offer.to_sdp_string())
			.send()
			.await
			.unwrap()
			.text()
			.await
			.unwrap();
		let answer = str0m::change::SdpAnswer::from_sdp_string(&answer).unwrap();
		rtc.sdp_api().accept_answer(pending, answer).unwrap();
		let mut agent = crate::ice::Agent::new(rtc, socket).unwrap();

		let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
		let task = tokio::spawn(async move {
			loop {
				match agent.next().await {
					Ok(str0m::Event::MediaData(data)) => tx.send(data.data.to_vec()).unwrap(),
					Ok(_) => {}
					Err(crate::ice::Error::Closed) => return,
					Err(err) => panic!("the viewer failed: {err:?}"),
				}
			}
		});
		(task, rx)
	}

	/// The next payload `viewer` receives, or `None` once the server closed.
	async fn received(viewer: &mut tokio::sync::mpsc::UnboundedReceiver<Vec<u8>>) -> Option<Vec<u8>> {
		kio::clock::timeout(TIMEOUT, viewer.recv())
			.await
			.expect("the viewer neither received nor closed")
	}

	/// What a test watches play: a raw WHEP viewer's payloads, or a track's.
	trait Viewer {
		/// The next payload, or `None` once the viewer closed.
		async fn played(&mut self) -> Option<Vec<u8>>;
	}

	impl Viewer for tokio::sync::mpsc::UnboundedReceiver<Vec<u8>> {
		async fn played(&mut self) -> Option<Vec<u8>> {
			received(self).await
		}
	}

	impl Viewer for Track {
		async fn played(&mut self) -> Option<Vec<u8>> {
			let frame = kio::clock::timeout(TIMEOUT, self.next())
				.await
				.expect("output Opus packet timed out")
				.expect("read output Opus packet")?;
			Some(frame.payload.to_vec())
		}
	}

	/// A source that keeps sending while a test waits on its viewer, as a live one does.
	/// Each packet's group ends whole where the packet does, so a paused source's last one
	/// ages out before a session that connects past its delay budget reads it, which a loaded
	/// host does to the first packet as readily as a later one.
	struct Live<'a> {
		bridge: &'a mut dyn Bridge,
		/// The next packet's timestamp.
		at: Duration,
		/// What it last sent live, which may still be in flight after its viewer played it.
		sent: Option<&'static [u8]>,
	}

	impl<'a> Live<'a> {
		/// How far apart it sends packets, in both media time and wall time.
		const STEP: Duration = Duration::from_millis(20);

		fn new(bridge: &'a mut dyn Bridge) -> Self {
			Self {
				bridge,
				at: Self::STEP,
				sent: None,
			}
		}

		/// Send `payload` once.
		fn push(&mut self, payload: &'static [u8]) {
			push(self.bridge, self.at.as_micros() as u64, payload);
			self.at += Self::STEP;
		}

		/// Send `payload` every [`Self::STEP`] until `viewer` plays something other than an earlier
		/// live payload, and return that. Repeating the last payload skips nothing.
		async fn play(&mut self, payload: &'static [u8], viewer: &mut impl Viewer) -> Vec<u8> {
			let stale = self.sent.filter(|sent| *sent != payload);
			self.until(payload, fresh(viewer, stale))
				.await
				.expect("the viewer was closed")
		}

		/// Send `payload` every [`Self::STEP`] until `done` resolves, and return its output.
		async fn until<T>(&mut self, payload: &'static [u8], done: impl Future<Output = T>) -> T {
			self.sent = Some(payload);
			tokio::pin!(done);
			loop {
				self.push(payload);
				tokio::select! {
					output = &mut done => return output,
					_ = kio::clock::sleep(Self::STEP) => {}
				}
			}
		}
	}

	/// The next payload `viewer` plays other than `stale`, or `None` once it closed.
	async fn fresh(viewer: &mut impl Viewer, stale: Option<&[u8]>) -> Option<Vec<u8>> {
		loop {
			let played = viewer.played().await?;
			if Some(played.as_slice()) != stale {
				return Some(played);
			}
		}
	}

	/// A WHEP viewer stays on the identity its session resolved: a publisher taking over the
	/// path never reaches it, so the session never resets its timestamps or codec state, and it
	/// fails once that identity's route dies.
	#[tokio::test]
	async fn a_whep_viewer_stays_on_its_identity_until_the_route_dies() {
		let origin = moq_tokio::origin::spawn();
		let (old_route, mut old, old_catalog) = publish_opus(&origin, moq_net::Identity::random());
		let mut live = Live::new(&mut *old);
		live.push(OPUS_PACKET);

		let (url, http, mut sessions) = serve_whep(&origin).await;
		let (viewer, mut played) = view(url).await;
		let session = kio::clock::timeout(TIMEOUT, sessions.recv())
			.await
			.expect("WHEP negotiation timed out")
			.unwrap();
		assert_eq!(live.play(OPUS_PACKET, &mut played).await, OPUS_PACKET);

		let _new = take_over(&origin).await;
		assert_eq!(
			live.play(OLD_PACKET, &mut played).await,
			OLD_PACKET,
			"the viewer left its identity"
		);

		kill((old_route, old, old_catalog));
		let ended = kio::clock::timeout(TIMEOUT, session)
			.await
			.expect("the session outlived its route")
			.unwrap();
		assert!(ended.is_err(), "the session ended cleanly");
		viewer.abort();
		http.abort();
	}

	/// A broadcast that ends cleanly ends its WHEP sessions once they have sent what they hold,
	/// closing the peer connection so the viewer learns to reconnect rather than freeze.
	#[tokio::test]
	async fn a_whep_viewer_ends_with_its_broadcast() {
		let origin = moq_tokio::origin::spawn();
		let (_route, mut bridge, mut catalog) = publish_opus(&origin, moq_net::Identity::random());
		let mut live = Live::new(&mut *bridge);
		live.push(OPUS_PACKET);

		let (url, http, mut sessions) = serve_whep(&origin).await;
		let (task, mut viewer) = raw_view(url).await;
		let session = kio::clock::timeout(TIMEOUT, sessions.recv())
			.await
			.expect("WHEP negotiation timed out")
			.unwrap();
		assert_eq!(live.play(OPUS_PACKET, &mut viewer).await, OPUS_PACKET);

		// The publisher's final packet, then its clean end, as an RTMP `deleteStream` does.
		live.push(NEW_PACKET);
		let _finished = bridge.finish().unwrap();
		catalog.finish().unwrap();
		assert_eq!(
			fresh(&mut viewer, Some(OPUS_PACKET)).await.as_deref(),
			Some(NEW_PACKET),
			"the final packet was lost"
		);
		assert_eq!(
			fresh(&mut viewer, Some(OPUS_PACKET)).await,
			None,
			"the viewer was not closed"
		);
		// The channel also closes if the viewer panicked, so only its task tells a clean close.
		kio::clock::timeout(TIMEOUT, task)
			.await
			.expect("the viewer outlived its channel")
			.expect("the viewer failed");
		kio::clock::timeout(TIMEOUT, session)
			.await
			.expect("the session outlived its broadcast")
			.unwrap()
			.expect("the session ended cleanly");
		http.abort();
	}

	/// A viewer joining once the catalog ended, while a track still sends, plays that track to
	/// its end before its session closes.
	#[tokio::test]
	async fn a_whep_viewer_joining_after_the_catalog_ends_plays_the_rest() {
		let origin = moq_tokio::origin::spawn();
		let (_route, mut bridge, mut catalog) = publish_opus(&origin, moq_net::Identity::random());
		let mut live = Live::new(&mut *bridge);
		live.push(OPUS_PACKET);
		catalog.finish().unwrap();

		let (url, http, mut sessions) = serve_whep(&origin).await;
		let (task, mut viewer) = raw_view(url).await;
		let session = kio::clock::timeout(TIMEOUT, sessions.recv())
			.await
			.expect("WHEP negotiation timed out")
			.unwrap();
		// The line subscribed at a group it found, so it plays the track.
		assert_eq!(live.play(OPUS_PACKET, &mut viewer).await, OPUS_PACKET);
		live.push(NEW_PACKET);
		let _finished = bridge.finish().unwrap();
		assert_eq!(fresh(&mut viewer, Some(OPUS_PACKET)).await.as_deref(), Some(NEW_PACKET));
		assert_eq!(
			fresh(&mut viewer, Some(OPUS_PACKET)).await,
			None,
			"the viewer was not closed"
		);
		// The channel also closes if the viewer panicked, so only its task tells a clean close.
		kio::clock::timeout(TIMEOUT, task)
			.await
			.expect("the viewer outlived its channel")
			.expect("the viewer failed");
		kio::clock::timeout(TIMEOUT, session)
			.await
			.expect("the session outlived its broadcast")
			.unwrap()
			.expect("the session ended cleanly");
		http.abort();
	}

	/// A viewer re-offering from the address its abandoned session still holds plays on its
	/// new session: the new session's ICE confirms the path, so the address moves to it.
	#[tokio::test]
	async fn a_reoffer_from_the_same_address_plays() {
		let origin = moq_tokio::origin::spawn();
		let (_route, mut bridge, _catalog) = publish_opus(&origin, moq_net::Identity::random());
		let mut live = Live::new(&mut *bridge);

		let (url, http, mut sessions) = serve_whep(&origin).await;
		let udp = std::sync::Arc::new(tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap());
		let (old, mut viewer) = raw_view_on(url.clone(), udp.clone()).await;
		let _old_session = sessions.recv().await.unwrap();
		assert_eq!(live.play(NEW_PACKET, &mut viewer).await, NEW_PACKET);

		// The peer drops its connection without a word, then offers again from the same address.
		old.abort();
		let _ = old.await;
		let (_new, mut viewer) = raw_view_on(url, udp).await;
		let _new_session = sessions.recv().await.unwrap();
		assert_eq!(live.play(NEW_PACKET, &mut viewer).await, NEW_PACKET);
		http.abort();
	}

	/// A WHIP push stays on the identity it resolved the same way, and
	/// [`Client::publish`] fails once that identity's route dies.
	#[tokio::test]
	async fn a_whip_push_stays_on_its_identity_until_the_route_dies() {
		let origin = moq_tokio::origin::spawn();
		let (old_route, mut old, old_catalog) = publish_opus(&origin, moq_net::Identity::random());
		let mut live = Live::new(&mut *old);
		live.push(OPUS_PACKET);

		let server = Server::bind("127.0.0.1:0".parse().unwrap(), server::Config::default())
			.await
			.unwrap();
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

		let client = Client::new(client::Config::default()).unwrap();
		let url = format!("http://{address}/whip/ingested").parse().expect("WHIP URL");
		let source = origin.consume();
		let broadcast = source.request("source", None).await.unwrap();
		let source = moq_mux::Source::new(source, broadcast);
		let pushed = tokio::spawn(async move { client.publish(url, source).await });
		let ingested = ingest_origin.consume();
		let ingested = ingested
			.follow("ingested", None)
			.unwrap()
			.available()
			.await
			.unwrap()
			.unwrap();
		let mut played = opus_output(&ingested).await;
		assert_eq!(live.play(OPUS_PACKET, &mut played).await, OPUS_PACKET);

		let _new = take_over(&origin).await;
		assert_eq!(
			live.play(OLD_PACKET, &mut played).await,
			OLD_PACKET,
			"the push left its identity"
		);

		kill((old_route, old, old_catalog));
		let pushed = kio::clock::timeout(TIMEOUT, pushed)
			.await
			.expect("the push outlived its route")
			.unwrap();
		assert!(pushed.is_err(), "the push ended cleanly");
		http.abort();
	}

	/// Serve WHIP at `/whip/ingested` under `rate`, sending each session's task out.
	async fn serve_capped_whip(
		rate: moq_net::bandwidth::Limit,
	) -> (
		std::net::SocketAddr,
		tokio::task::JoinHandle<()>,
		tokio::sync::mpsc::Receiver<tokio::task::JoinHandle<crate::Result<()>>>,
	) {
		let server = Server::bind("127.0.0.1:0".parse().unwrap(), server::Config::default())
			.await
			.unwrap();
		let origin = moq_tokio::origin::spawn();
		let (sessions_tx, sessions) = tokio::sync::mpsc::channel(1);
		let app = Router::new().route(
			"/whip/ingested",
			axum::routing::post(move |offer: String| {
				let (server, origin, rate, sessions) =
					(server.clone(), origin.clone(), rate.clone(), sessions_tx.clone());
				async move {
					let mut request =
						server::whip::Request::new(&origin, moq_net::Path::from_static("ingested"), &offer);
					request.ingest.rate = Some(rate);
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
		let client = Client::new(client::Config::default()).unwrap();
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
		let broadcast = origin
			.route("source", moq_net::origin::RouteOptions::default())
			.unwrap();
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

		let rate = moq_net::bandwidth::Limit::new(moq_net::bandwidth::Cap::new(moq_net::bandwidth::Rate::from_mbps(2)));
		let (address, http, mut sessions) = serve_capped_whip(rate.clone()).await;
		let pushed = push_whip(&origin, address).await;
		let session = kio::clock::timeout(TIMEOUT, sessions.recv()).await.unwrap().unwrap();

		logged("bps=1800000").await;
		rate.set(moq_net::bandwidth::Cap::new(moq_net::bandwidth::Rate::from_mbps(1)));
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
		let (_route, mut opus, _catalog) = publish_opus(&origin, moq_net::Identity::random());
		// 8 kbps with a tenth of a second of burst allows 100 bytes up front.
		let cap =
			moq_net::bandwidth::Cap::new(moq_net::bandwidth::Rate::from_kbps(8)).with_burst(Duration::from_millis(100));
		let (address, http, mut sessions) = serve_capped_whip(moq_net::bandwidth::Limit::new(cap)).await;
		const LOUD: &[u8] = &[0xfc; 1000];
		let mut live = Live::new(&mut *opus);
		live.push(LOUD);
		let pushed = push_whip(&origin, address).await;

		let session = kio::clock::timeout(TIMEOUT, sessions.recv()).await.unwrap().unwrap();
		let ended = kio::clock::timeout(TIMEOUT, live.until(LOUD, session))
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
			.route("source", moq_net::origin::RouteOptions::default())
			.expect("create source broadcast");
		let catalog =
			moq_mux::catalog::Producer::new(&mut source.broadcast().clone(), moq_mux::catalog::Config::default())
				.expect("create source catalog");
		let mut opus = crate::codec::opus::Bridge::new(source.broadcast().clone(), catalog, 48_000, 2)
			.expect("create Opus bridge");
		let mut live = Live::new(&mut opus);
		live.push(OPUS_PACKET);
		let (announcement, announcement_active) = kio::clock::timeout(TIMEOUT, next_update(&mut announcements))
			.await
			.expect("source announcement timed out")
			.expect("source origin closed");
		assert_eq!(announcement.prefix.as_str(), "source");
		assert!(announcement_active, "source was unannounced");
		drop(announcements);

		let server_origin = moq_tokio::origin::spawn();
		let server = Server::bind("127.0.0.1:0".parse().unwrap(), server::Config::default())
			.await
			.unwrap();
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
						let response = server::whep::accept(
							&server,
							server::whep::Request::new(&origin, moq_net::Path::from_static("ingested"), &offer),
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
			);
		let (address, http) = serve(app).await;

		let client = Client::new(client::Config::default()).unwrap();
		// Credentials in the dialed URLs must never reach the client's connect logs.
		let whip = format!("http://user:pass@{address}/whip/ingested?jwt=canary-7f3e")
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
			.route("output", moq_net::origin::RouteOptions::hidden())
			.expect("create output broadcast");
		let output_consumer = output.broadcast().consume();
		let whep = format!("http://user:pass@{address}/whep/ingested?jwt=canary-7f3e")
			.parse()
			.expect("WHEP URL");
		let mut fill = output.broadcast().clone();
		let catalog = moq_mux::catalog::Producer::new(&mut fill, Default::default()).unwrap();
		let whep_client = tokio::spawn(async move { client.pull(whep, &fill, &catalog).await });
		let (whep_id, whep_task) = kio::clock::timeout(TIMEOUT, sources_rx.recv())
			.await
			.expect("WHEP negotiation timed out")
			.unwrap();

		let mut played = opus_output(&output_consumer).await;
		assert_eq!(live.play(OPUS_PACKET, &mut played).await, OPUS_PACKET);

		assert!(logs_contain("whip client connected"));
		assert!(logs_contain("whep client connected"));
		for secret in ["jwt", "canary-7f3e", "user:pass"] {
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
