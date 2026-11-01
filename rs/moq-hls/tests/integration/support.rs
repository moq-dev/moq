//! A publisher and an HLS export on either side of a moq-net session over the in-memory
//! transport.

use std::time::Duration;

use axum::http::{Response, StatusCode, header};
use bytes::Bytes;
use moq_hls::Server;
use moq_hls::export::{Kind, Rendition, segments};

/// How long a step may take on the paused clock before the test fails.
pub const TIMEOUT: Duration = Duration::from_secs(10);

/// The media track every test publishes, which is also its rendition name.
pub const TRACK: &str = "video0";

/// A subscriber connected to a publisher over one session, as an export connects to its relay.
pub struct Pair {
	/// The publisher's origin, which the server session publishes.
	pub publisher: moq_net::origin::Producer,
	/// The subscriber's origin, which the client session inserts remote broadcasts into.
	pub subscriber: moq_net::origin::Producer,
	_client: moq_net::Session,
	server: Option<moq_net::Session>,
}

impl Pair {
	/// Connect on the default moq-lite version.
	pub async fn connect() -> Self {
		let version = *moq_net::Versions::default().iter().next().expect("a default version");
		Self::new(version, Default::default()).await
	}

	/// Connect on `version`, with the publisher's origin built from `config`.
	pub async fn new(version: moq_net::Version, config: moq_net::origin::Config) -> Self {
		let publisher = origin(config);
		let subscriber = origin(Default::default());
		let mut transport = moq_net::transport::memory::Config::default();
		transport.protocol = Some(version.alpn().to_string());
		let (client_transport, server_transport) = moq_net::transport::memory::pair(transport);
		let client = moq_net::Client::new()
			.with_versions(version.into())
			.with_subscriber(subscriber.clone());
		let server = moq_net::Server::new()
			.with_versions(version.into())
			.with_publisher(publisher.consume());
		let (client, server) = tokio::join!(
			async {
				let (session, driver) = client
					.connect(kio::clock::now(), client_transport)
					.await
					.expect("client session");
				tokio::spawn(moq_net::time::run(driver));
				session
			},
			async {
				let (session, driver) = server
					.accept(kio::clock::now(), server_transport)
					.await
					.expect("server session");
				tokio::spawn(moq_net::time::run(driver));
				session
			}
		);
		Self {
			publisher,
			subscriber,
			_client: client,
			server: Some(server),
		}
	}

	/// An HLS server exporting everything the subscriber receives.
	pub fn server(&self) -> Server {
		Server::new(self.subscriber.consume(), Default::default())
	}

	/// Drop the publisher's session, as a publisher that lost its connection would.
	pub fn disconnect(&mut self) {
		drop(self.server.take().expect("connected"));
	}
}

fn origin(config: moq_net::origin::Config) -> moq_net::origin::Producer {
	let (origin, driver) = moq_net::origin::Producer::new(config);
	tokio::spawn(moq_net::time::run(driver));
	origin
}

/// One publisher run of a broadcast: a catalog and a single VP8 rendition whose HLS records the
/// catalog advertises.
pub struct Publisher {
	/// The broadcast, which announces its path while it lives.
	pub broadcast: moq_net::origin::Publication,
	/// The catalog.
	pub catalog: moq_mux::catalog::Producer,
	/// The media track, recording its HLS records as it is written.
	pub media: moq_mux::container::Producer<moq_mux::catalog::hang::Container, hang::catalog::VideoConfig>,
}

impl Publisher {
	/// Announce `path` on `origin` under `route` and publish its catalog.
	pub fn new(origin: &moq_net::origin::Producer, path: &str, route: moq_net::origin::Route) -> Self {
		let broadcast = origin.create_broadcast(path).expect("publish");
		broadcast.announce(route).expect("announce");
		let catalog =
			moq_mux::catalog::Producer::new(&mut broadcast.broadcast().clone(), catalog_config()).expect("catalog");
		let reserved = catalog.reserve();
		let track = broadcast.create_track(TRACK, timed()).expect("track");
		let media = reserved
			.video(
				track,
				moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Video),
				video_config(),
			)
			.expect("rendition");
		drop(reserved);
		Self {
			broadcast,
			catalog,
			media,
		}
	}

	/// Write one-second frames for each second in `start..end`, a keyframe on every even second.
	/// Segments are cut every four seconds, so writing up to 13 completes segments 0 to 2 and
	/// leaves the GOP at 12s open.
	pub fn write(&mut self, start: u64, end: u64, payload: u8) {
		for second in start..end {
			self.media.write(frame(second, payload)).expect("write");
		}
	}

	/// Finish the media and then the catalog, as a publisher ending cleanly does.
	pub fn finish(&mut self) {
		self.media.finish().expect("finish media");
		self.catalog.finish().expect("finish catalog");
	}
}

/// A catalog config publishing HLS records, hinted so they are advertised before any media and a
/// recorder can start first.
pub fn catalog_config() -> moq_mux::catalog::Config {
	let hls = moq_mux::hls::Config::default().with_duration_hint(Duration::from_secs(2));
	moq_mux::catalog::Config::default().with_hls(hls)
}

/// A millisecond track: the frames written here carry timestamps.
pub fn timed() -> moq_net::track::Info {
	moq_net::track::Info::default().with_timescale(moq_net::Timescale::MILLI)
}

/// A 320x240 VP8 rendition, whose init its catalog config alone describes.
pub fn video_config() -> hang::catalog::VideoConfig {
	let mut config = hang::catalog::VideoConfig::new(hang::catalog::VideoCodec::VP8);
	config.coded_width = Some(320);
	config.coded_height = Some(240);
	config.framerate = Some(30.0);
	config
}

/// A VP8 frame at `second`, a keyframe on even seconds, tagged with `payload` so each run's
/// media differs byte for byte.
pub fn frame(second: u64, payload: u8) -> moq_mux::container::Frame {
	let keyframe = second.is_multiple_of(2);
	let mut bytes = if keyframe {
		vec![0x10, 0x00, 0x00, 0x9d, 0x01, 0x2a, 0x40, 0x01, 0xf0, 0x00]
	} else {
		vec![0x31, 0x00, 0x00]
	};
	bytes.extend_from_slice(&[payload; 8]);
	moq_mux::container::Frame {
		timestamp: moq_net::Timestamp::from_secs(second).expect("timestamp"),
		payload: bytes.into(),
		keyframe,
		duration: None,
	}
}

/// The rendition `server` exports for `path` once its catalog lists it.
pub async fn rendition(server: &Server, path: &str) -> std::sync::Arc<Rendition> {
	let broadcaster = kio::clock::timeout(TIMEOUT, server.broadcaster(path))
		.await
		.expect("resolve timed out")
		.expect("announced");
	let mut renditions = broadcaster.renditions();
	loop {
		match kio::clock::timeout(TIMEOUT, renditions.next()).await {
			Ok(Ok(Some(moq_hls::export::renditions::Event::Added(rendition)))) if rendition.name == TRACK => {
				return rendition;
			}
			Ok(Ok(Some(_))) => continue,
			result => panic!("the catalog never listed {TRACK}: {:?}", result.map(|r| r.map(|_| ()))),
		}
	}
}

/// Read `cursor` until it ends, returning the segment numbers read and how it ended.
pub async fn drain(cursor: &mut segments::Consumer) -> (Vec<u64>, moq_hls::Result<()>) {
	let mut read = Vec::new();
	loop {
		match kio::clock::timeout(TIMEOUT, cursor.next())
			.await
			.expect("the cursor stalled")
		{
			Ok(Some(segment)) => read.push(segment.segment),
			Ok(None) => return (read, Ok(())),
			Err(err) => return (read, Err(err)),
		}
	}
}

/// GET `uri` from `app`, with the whole body read.
pub async fn get(app: &axum::Router, uri: &str) -> Response<Bytes> {
	use tower::ServiceExt;
	let request = axum::extract::Request::builder()
		.uri(uri)
		.body(axum::body::Body::empty())
		.expect("request");
	let (parts, body) = app.clone().oneshot(request).await.expect("infallible").into_parts();
	let body = axum::body::to_bytes(body, usize::MAX).await.expect("body");
	Response::from_parts(parts, body)
}

/// Poll `uri` until it answers 200 with a body containing `needle`, returning the body.
pub async fn listed(app: &axum::Router, uri: &str, needle: &str) -> String {
	let deadline = kio::clock::now() + TIMEOUT;
	loop {
		let response = get(app, uri).await;
		let body = String::from_utf8_lossy(response.body()).into_owned();
		if response.status() == StatusCode::OK && body.contains(needle) {
			return body;
		}
		assert!(
			kio::clock::now() < deadline,
			"{uri} never listed {needle}: {} {body}",
			response.status()
		);
		kio::clock::sleep(Duration::from_millis(50)).await;
	}
}

/// Assert `response` is a 404 that a CDN won't pin, since the resource may appear later.
pub fn assert_missing(response: &Response<Bytes>) {
	assert_eq!(response.status(), StatusCode::NOT_FOUND);
	assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
}

/// The segment numbers a media playlist lists, in order.
pub fn numbers(playlist: &str) -> Vec<u64> {
	playlist
		.lines()
		.filter_map(|line| line.strip_prefix("seg/")?.strip_suffix(".m4s"))
		.map(|name| name.rsplit('.').next().expect("a number").parse().expect("numbered"))
		.collect()
}

/// The media playlist URI the master playlist of `path` lists for [`TRACK`], under its instance.
pub async fn media_uri(app: &axum::Router, path: &str) -> String {
	let master = listed(app, &format!("/{path}/master.m3u8"), TRACK).await;
	let line = master
		.lines()
		.find(|line| line.ends_with(&format!("/{}/{TRACK}/media.m3u8", Kind::Video.as_str())))
		.unwrap_or_else(|| panic!("no {TRACK} variant in {master}"));
	format!("/{path}/{line}")
}

/// The URI `playlist` lists for segment `n`.
pub fn segment(playlist: &str, n: u64) -> String {
	let suffix = format!(".{n}.m4s");
	playlist
		.lines()
		.find(|line| line.starts_with("seg/") && line.ends_with(&suffix))
		.unwrap_or_else(|| panic!("no segment {n} in {playlist}"))
		.to_string()
}
