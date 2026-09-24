//! axum handlers for the HLS endpoints.

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::extract::{RawQuery, State};
use axum::http::{StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use bytes::Bytes;
use percent_encoding::percent_decode_str;

use super::Server;
use crate::export::{Kind, Rendition};

const M3U8: &str = "application/vnd.apple.mpegurl";
const MPD: &str = "application/dash+xml";
const MP4: &str = "video/mp4";

/// How long a rendition lookup waits for the catalog (and its first timeline records) to
/// populate.
const READY_TIMEOUT: Duration = Duration::from_secs(5);

pub fn router(server: Server) -> Router {
	Router::new().route("/{*path}", get(request)).with_state(server)
}

/// A parsed request path: the broadcast, and the resource under it.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Route {
	/// The broadcast path, percent-decoded. An embedder that scopes its origin rewrites
	/// this relative to that scope before calling [`Server::respond`].
	pub broadcast: String,
	/// The resource requested under the broadcast.
	pub resource: Resource,
}

/// A resource under a broadcast.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Resource {
	/// `master.m3u8`: the HLS multivariant playlist.
	Master,
	/// `manifest.mpd`: the DASH manifest.
	Manifest,
	/// `{kind}/{rendition}/media.m3u8`: a rendition's HLS media playlist.
	#[non_exhaustive]
	Media {
		/// The rendition's kind.
		kind: Kind,
		/// The rendition's name, percent-decoded.
		rendition: String,
	},
	/// `{kind}/{rendition}/init.{hash}.mp4`: a rendition's CMAF init segment.
	#[non_exhaustive]
	Init {
		/// The rendition's kind.
		kind: Kind,
		/// The rendition's name, percent-decoded.
		rendition: String,
		/// The hash of the init bytes the URL names.
		hash: String,
	},
	/// `{kind}/{rendition}/seg/[{generation}.]{sequence}.m4s`: a segment by its HLS number.
	#[non_exhaustive]
	Segment {
		/// The rendition's kind.
		kind: Kind,
		/// The rendition's name, percent-decoded.
		rendition: String,
		/// The publisher run the URL names, if the broadcaster carries one.
		generation: Option<String>,
		/// The segment's aligned number.
		sequence: u64,
	},
	/// `{kind}/{rendition}/seg/[{generation}.]t{pts}.m4s`: the same bytes, addressed by the
	/// DASH timeline pts (`$Time$`).
	#[non_exhaustive]
	SegmentAt {
		/// The rendition's kind.
		kind: Kind,
		/// The rendition's name, percent-decoded.
		rendition: String,
		/// The publisher run the URL names, if the broadcaster carries one.
		generation: Option<String>,
		/// The segment's timeline pts.
		pts: u64,
	},
}

impl Route {
	/// Parse a request path such as `/project/live/video/hd/media.m3u8`, or `None` when it
	/// addresses nothing.
	///
	/// The broadcast occupies every segment before the resource suffix. Each segment is
	/// percent-decoded, and one that decodes to a `/` is refused, so an encoded separator
	/// cannot smuggle a different broadcast past a policy check on [`broadcast`](Self::broadcast).
	pub fn parse(path: &str) -> Option<Self> {
		let parts = path
			.strip_prefix('/')?
			.split('/')
			.map(|part| {
				percent_decode_str(part)
					.decode_utf8()
					.ok()
					.map(|part| part.into_owned())
			})
			.collect::<Option<Vec<_>>>()?;
		if parts.iter().any(String::is_empty) {
			return None;
		}

		let (broadcast, resource) = match parts.as_slice() {
			[broadcast @ .., file] if file == "master.m3u8" => (broadcast, Resource::Master),
			[broadcast @ .., file] if file == "manifest.mpd" => (broadcast, Resource::Manifest),
			[broadcast @ .., kind, rendition, file] if file == "media.m3u8" => (
				broadcast,
				Resource::Media {
					kind: Kind::parse(kind)?,
					rendition: rendition.clone(),
				},
			),
			[broadcast @ .., kind, rendition, file] if init_hash(file).is_some() => (
				broadcast,
				Resource::Init {
					kind: Kind::parse(kind)?,
					rendition: rendition.clone(),
					hash: init_hash(file)?.to_string(),
				},
			),
			[broadcast @ .., kind, rendition, directory, file] if directory == "seg" => {
				let kind = Kind::parse(kind)?;
				let rendition = rendition.clone();
				let stem = file.strip_suffix(".m4s")?;
				// `{generation}.{segment}` when the export carries a generation, `{segment}` otherwise.
				let (generation, stem) = match stem.split_once('.') {
					Some((generation, stem)) => (Some(generation.to_string()), stem),
					None => (None, stem),
				};
				let resource = match stem.strip_prefix('t') {
					Some(pts) => Resource::SegmentAt {
						kind,
						rendition,
						generation,
						pts: pts.parse().ok()?,
					},
					None => Resource::Segment {
						kind,
						rendition,
						generation,
						sequence: stem.parse().ok()?,
					},
				};
				(broadcast, resource)
			}
			_ => return None,
		};

		if broadcast.is_empty() || broadcast.iter().any(|part| part.contains('/')) {
			return None;
		}
		Some(Self {
			broadcast: broadcast.join("/"),
			resource,
		})
	}
}

/// The content hash in an `init.{hash}.mp4` file name.
fn init_hash(file: &str) -> Option<&str> {
	file.strip_prefix("init.")?
		.strip_suffix(".mp4")
		.filter(|hash| !hash.is_empty())
}

async fn request(State(server): State<Server>, uri: Uri, RawQuery(query): RawQuery) -> Response {
	match Route::parse(uri.path()) {
		Some(route) => server.respond(&route, query.as_deref()).await,
		None => not_found(),
	}
}

impl Server {
	/// Answer a parsed [`Route`] from this server's origin: the handler behind
	/// [`router`](Self::router), for an embedder that parses and authorizes the request
	/// itself.
	///
	/// `query` is the raw request query (without the leading `?`), propagated to every
	/// child URL a playlist or manifest lists, so a credential carried there reaches the
	/// player's follow-up requests.
	pub async fn respond(&self, route: &Route, query: Option<&str>) -> Response {
		let broadcast = route.broadcast.as_str();
		match &route.resource {
			Resource::Master => master(self, broadcast, query).await,
			Resource::Manifest => manifest(self, broadcast, query).await,
			Resource::Media { kind, rendition } => media(self, broadcast, *kind, rendition, query).await,
			Resource::Init { kind, rendition, hash } => init(self, broadcast, *kind, rendition, hash).await,
			Resource::Segment {
				kind,
				rendition,
				generation,
				sequence,
			} => {
				let at = SegmentAt::Sequence(*sequence);
				segment(self, broadcast, *kind, rendition, generation.as_deref(), at).await
			}
			Resource::SegmentAt {
				kind,
				rendition,
				generation,
				pts,
			} => {
				let at = SegmentAt::Pts(*pts);
				segment(self, broadcast, *kind, rendition, generation.as_deref(), at).await
			}
		}
	}
}

async fn master(server: &Server, broadcast: &str, query: Option<&str>) -> Response {
	let Some(broadcaster) = server.broadcaster(broadcast).await else {
		return not_found();
	};
	let _ = tokio::time::timeout(READY_TIMEOUT, broadcaster.ready()).await;
	if broadcaster.is_empty() {
		return not_found();
	}
	m3u8(broadcaster.master_playlist(query))
}

async fn manifest(server: &Server, broadcast: &str, query: Option<&str>) -> Response {
	let Some(broadcaster) = server.broadcaster(broadcast).await else {
		return not_found();
	};
	let _ = tokio::time::timeout(READY_TIMEOUT, broadcaster.ready()).await;
	if broadcaster.is_empty() {
		return not_found();
	}
	// A manifest whose timelines are all empty confuses players; give the broadcast a moment
	// to index its first complete segment before answering.
	let _ = tokio::time::timeout(READY_TIMEOUT, broadcaster.playable()).await;
	// Each representation names its init by hash, which an inline codec only learns from media.
	broadcaster.build_inits().await;
	match broadcaster.manifest(query) {
		Some(manifest) => mpd(manifest),
		None => not_found(),
	}
}

async fn media(server: &Server, broadcast: &str, kind: Kind, rendition: &str, query: Option<&str>) -> Response {
	let Some(rendition) = rendition_for(server, broadcast, kind, rendition).await else {
		return not_found();
	};
	match tokio::time::timeout(READY_TIMEOUT, rendition.playlist(query)).await {
		Ok(Ok(Some(playlist))) => m3u8(playlist),
		Ok(Ok(None)) | Err(_) => not_found(),
		Ok(Err(err)) => server_error(err),
	}
}

async fn init(server: &Server, broadcast: &str, kind: Kind, rendition: &str, hash: &str) -> Response {
	let Some(rendition) = rendition_for(server, broadcast, kind, rendition).await else {
		return not_found();
	};
	media_result(rendition.init_versioned(hash).await, server)
}

/// How a segment URL addresses its bytes: HLS by aligned number (`seg/0.m4s`), DASH by
/// timeline pts (`seg/t2000.m4s`, the SegmentTemplate's `$Time$`).
enum SegmentAt {
	Sequence(u64),
	Pts(u64),
}

async fn segment(
	server: &Server,
	broadcast: &str,
	kind: Kind,
	rendition: &str,
	generation: Option<&str>,
	at: SegmentAt,
) -> Response {
	let Some(rendition) = rendition_for(server, broadcast, kind, rendition).await else {
		return not_found();
	};
	// Only the current run's URLs are served: a restart reuses segment numbers, so another
	// generation's URL would name different bytes than the ones it once served.
	let current = || rendition.generation().as_deref() == generation;
	if !current() {
		return not_found();
	}
	let result = match at {
		SegmentAt::Sequence(sequence) => rendition.segment(sequence).await,
		SegmentAt::Pts(pts) => rendition.segment_at(pts).await,
	};
	// A generation change while fetching may have swapped the rows under the lookup.
	if !current() {
		return not_found();
	}
	media_result(result, server)
}

/// Resolve a rendition, waiting for the catalog to populate.
async fn rendition_for(server: &Server, broadcast: &str, kind: Kind, rendition: &str) -> Option<Arc<Rendition>> {
	let broadcaster = server.broadcaster(broadcast).await?;
	let _ = tokio::time::timeout(READY_TIMEOUT, broadcaster.ready()).await;
	broadcaster.rendition(kind, rendition)
}

fn m3u8(body: String) -> Response {
	// Playlists mutate as the live edge advances, so they must not be cached.
	(
		[(header::CONTENT_TYPE, M3U8), (header::CACHE_CONTROL, "no-cache")],
		body,
	)
		.into_response()
}

fn mpd(body: String) -> Response {
	// Like the playlists: the manifest mutates as the live edge advances.
	([(header::CONTENT_TYPE, MPD), (header::CACHE_CONTROL, "no-cache")], body).into_response()
}

fn media_result(result: crate::Result<Option<Bytes>>, server: &Server) -> Response {
	match result {
		Ok(Some(bytes)) => media_bytes(bytes, server),
		Ok(None) => not_found(),
		Err(err) => server_error(err),
	}
}

fn media_bytes(body: Bytes, server: &Server) -> Response {
	// Init/segment bytes never change while their URL is listed, but a segment URL without a
	// generation is not globally unique: a restarted publisher starts a new timeline whose
	// segment numbers and pts can repeat the old ones with different media. Cap shared caching
	// at the playlist window - every concurrent viewer of the live window still hits the cache,
	// while a stale run's bytes age out as fast as the window that stopped listing them.
	let max_age = server.inner.config.window.as_secs().max(1);
	(
		[
			(header::CONTENT_TYPE, MP4.to_string()),
			(header::CACHE_CONTROL, format!("public, max-age={max_age}")),
		],
		body,
	)
		.into_response()
}

fn not_found() -> Response {
	// The resource may appear later (a segment not yet produced), so don't let a
	// CDN pin the 404.
	(StatusCode::NOT_FOUND, [(header::CACHE_CONTROL, "no-store")]).into_response()
}

fn server_error(err: crate::Error) -> Response {
	tracing::warn!(%err, "hls request failed");
	(StatusCode::INTERNAL_SERVER_ERROR, [(header::CACHE_CONTROL, "no-store")]).into_response()
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn parses_multisegment_broadcast_and_encoded_rendition() {
		assert_eq!(
			Route::parse("/project/live/video/cam%231%2Fmain%3Falt/media.m3u8"),
			Some(Route {
				broadcast: "project/live".to_string(),
				resource: Resource::Media {
					kind: Kind::Video,
					rendition: "cam#1/main?alt".to_string(),
				},
			})
		);
	}

	#[test]
	fn parses_all_resource_routes() {
		let resource = |path| Route::parse(path).map(|route| route.resource);
		assert_eq!(resource("/project/live/master.m3u8"), Some(Resource::Master));
		assert_eq!(resource("/project/live/manifest.mpd"), Some(Resource::Manifest));
		assert_eq!(
			resource("/project/live/audio/main/init.0123abcd.mp4"),
			Some(Resource::Init {
				kind: Kind::Audio,
				rendition: "main".to_string(),
				hash: "0123abcd".to_string(),
			})
		);
		assert_eq!(
			resource("/project/live/video/main/seg/42.m4s"),
			Some(Resource::Segment {
				kind: Kind::Video,
				rendition: "main".to_string(),
				generation: None,
				sequence: 42,
			})
		);
		assert_eq!(
			resource("/project/live/video/main/seg/run-1.42.m4s"),
			Some(Resource::Segment {
				kind: Kind::Video,
				rendition: "main".to_string(),
				generation: Some("run-1".to_string()),
				sequence: 42,
			})
		);
		assert_eq!(
			resource("/project/live/video/main/seg/t2000.m4s"),
			Some(Resource::SegmentAt {
				kind: Kind::Video,
				rendition: "main".to_string(),
				generation: None,
				pts: 2000,
			})
		);
		assert_eq!(
			resource("/project/live/video/main/seg/init.t2000.m4s"),
			Some(Resource::SegmentAt {
				kind: Kind::Video,
				rendition: "main".to_string(),
				generation: Some("init".to_string()),
				pts: 2000,
			})
		);
		assert!(Route::parse("/master.m3u8").is_none());
		assert!(Route::parse("/project/live/video/main/unknown").is_none());
		assert!(Route::parse("/project/live/data/main/media.m3u8").is_none());
		assert!(Route::parse("/project/live/video/main/init.mp4").is_none());
		assert!(Route::parse("/project/live/video/main/init..mp4").is_none());
		assert!(Route::parse("/project/live/video/main/seg/tx.m4s").is_none());
		assert!(Route::parse("/project/live/video/main/seg/1.ts").is_none());
	}

	#[test]
	fn rejects_empty_path_segments() {
		assert!(Route::parse("/project//live/master.m3u8").is_none());
	}

	#[test]
	fn rejects_encoded_broadcast_separators() {
		assert!(Route::parse("/project/private%2F/master.m3u8").is_none());
		assert!(Route::parse("/project%2Fprivate/master.m3u8").is_none());
	}

	const TIMEOUT: Duration = Duration::from_secs(10);

	fn vp8_frame(micros: u64, keyframe: bool) -> moq_mux::container::Frame {
		let payload = if keyframe {
			&[0x10, 0x00, 0x00, 0x9d, 0x01, 0x2a, 0x40, 0x01, 0xf0, 0x00][..]
		} else {
			&[0x31, 0x00, 0x00][..]
		};
		moq_mux::container::Frame {
			timestamp: moq_net::Timestamp::from_micros(micros).unwrap(),
			payload: bytes::Bytes::copy_from_slice(payload),
			keyframe,
			duration: None,
		}
	}

	fn video_config() -> hang::catalog::VideoConfig {
		let mut config = hang::catalog::VideoConfig::new(hang::catalog::VideoCodec::VP8);
		config.coded_width = Some(320);
		config.coded_height = Some(240);
		config.framerate = Some(30.0);
		config
	}

	/// A moq-lite publisher↔subscriber pair over loopback TCP, so a FETCH actually
	/// crosses a session.
	struct LitePair {
		pub_origin: moq_net::origin::Producer,
		sub_origin: moq_net::origin::Producer,
		_connection: moq_tokio::Connection,
		accept: tokio::task::JoinHandle<()>,
	}

	async fn lite_pair() -> LitePair {
		lite_pair_pub(moq_net::Hop::random()).await
	}

	async fn lite_pair_pub(config: impl Into<moq_net::origin::Config>) -> LitePair {
		use std::net::TcpListener;

		let lite: moq_net::Version = "moq-lite-05".parse().expect("lite version");
		let pub_origin = moq_tokio::origin::spawn_config(config.into());
		let sub_origin = moq_tokio::origin::spawn();

		for _ in 0..20 {
			let probe = TcpListener::bind("127.0.0.1:0").expect("bind probe");
			let port = probe.local_addr().expect("local addr").port();
			drop(probe);

			let mut listen = moq_tokio::listen::Config::default();
			listen.tcp.bind = Some(format!("127.0.0.1:{port}").parse().expect("addr"));
			listen.version = vec![lite];
			let Ok(server) = listen.init(Default::default()) else {
				continue;
			};
			let Ok(mut server) = server.listen().await else {
				continue;
			};

			let pub_for_accept = pub_origin.clone();
			let accept = tokio::spawn(async move {
				let request = server.accept().await.expect("accept");
				let session = request
					.with_publisher(&pub_for_accept)
					.ok()
					.await
					.expect("publisher session");
				let _ = session.closed().await;
			});

			let mut connect = moq_tokio::connect::Config::default();
			connect.version = vec![lite];
			let client = connect
				.init(Default::default())
				.expect("client")
				.with_subscriber(sub_origin.clone())
				.with_reconnect(false);
			let url: url::Url = format!("tcp://127.0.0.1:{port}/").parse().expect("url");
			let connection = tokio::time::timeout(TIMEOUT, client.connect(url).established())
				.await
				.expect("connect timed out")
				.expect("connect");

			return LitePair {
				pub_origin,
				sub_origin,
				_connection: connection,
				accept,
			};
		}
		panic!("could not bind a free TCP port after 20 attempts");
	}

	async fn oneshot(app: axum::Router, uri: &str) -> axum::http::Response<axum::body::Body> {
		use tower::ServiceExt;
		app.oneshot(
			axum::extract::Request::builder()
				.uri(uri)
				.body(axum::body::Body::empty())
				.unwrap(),
		)
		.await
		.unwrap()
	}

	async fn wait_listed(app: &axum::Router, playlist: &str, segment: &str) {
		let deadline = tokio::time::Instant::now() + TIMEOUT;
		loop {
			let response = oneshot(app.clone(), playlist).await;
			if response.status() == StatusCode::OK {
				let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
				if String::from_utf8_lossy(&body).contains(segment) {
					return;
				}
			}
			assert!(
				tokio::time::Instant::now() < deadline,
				"{playlist} never listed {segment}"
			);
			tokio::time::sleep(Duration::from_millis(50)).await;
		}
	}

	fn publish_video(
		broadcast: &mut moq_net::broadcast::Producer,
		config: hang::catalog::VideoConfig,
		track: impl Into<Option<moq_net::track::Info>>,
	) -> (
		moq_mux::catalog::Producer,
		(),
		moq_net::track::Producer,
		moq_mux::container::Producer<moq_mux::catalog::hang::Container, hang::catalog::VideoConfig>,
	) {
		let catalog = moq_mux::catalog::Producer::new(broadcast, moq_mux::catalog::Config::default()).unwrap();
		let reserved = catalog.reserve();
		let track = broadcast.create_track("video0", track).unwrap();
		let media = reserved
			.video(
				track.clone(),
				moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Video),
				config,
			)
			.unwrap();
		drop(reserved);
		(catalog, (), track, media)
	}

	fn write_three_gops(
		media: &mut moq_mux::container::Producer<moq_mux::catalog::hang::Container, hang::catalog::VideoConfig>,
	) {
		media.write(vp8_frame(0, true)).unwrap();
		media.write(vp8_frame(1_000_000, false)).unwrap();
		media.write(vp8_frame(2_000_000, true)).unwrap();
		media.write(vp8_frame(3_000_000, false)).unwrap();
		media.write(vp8_frame(4_000_000, true)).unwrap();
	}

	/// An embedder parses the path, rewrites the broadcast into its own scope, and answers
	/// through `respond` with the query propagated to every child URL.
	#[tokio::test]
	async fn respond_serves_a_route_rewritten_into_scope() {
		let pair = lite_pair().await;
		let mut broadcast = pair.pub_origin.create_broadcast("live").expect("publish");
		broadcast.announce(Default::default()).expect("announce");
		let (_catalog, _registration, _track, mut media) = publish_video(&mut broadcast, video_config(), None);
		write_three_gops(&mut media);

		let server = Server::new(pair.sub_origin.consume(), crate::export::Config::default());
		let mut route = Route::parse("/tenant/live/video/video0/media.m3u8").expect("media route");
		route.broadcast = route.broadcast.strip_prefix("tenant/").expect("scoped").to_string();

		let deadline = tokio::time::Instant::now() + TIMEOUT;
		let body = loop {
			let response = server.respond(&route, Some("jwt=abc")).await;
			if response.status() == StatusCode::OK {
				let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
				let body = String::from_utf8(body.to_vec()).unwrap();
				if body.contains("seg/0.m4s") {
					break body;
				}
			}
			assert!(
				tokio::time::Instant::now() < deadline,
				"media playlist never listed a segment"
			);
			tokio::time::sleep(Duration::from_millis(50)).await;
		};
		assert!(body.contains(".mp4?jwt=abc\""), "{body}");
		assert!(body.contains("seg/0.m4s?jwt=abc"), "{body}");

		pair.accept.abort();
	}

	async fn status(app: &axum::Router, uri: &str) -> StatusCode {
		oneshot(app.clone(), uri).await.status()
	}

	/// Only the URLs the renderers emit are served: the init under its hash, segments under the
	/// current generation.
	#[tokio::test]
	async fn serves_only_versioned_media_urls() {
		let origin = moq_tokio::origin::spawn();
		let mut broadcast = origin.create_broadcast("live").expect("publish");
		broadcast.announce(Default::default()).expect("announce");
		let (_catalog, _registration, _track, mut media) = publish_video(&mut broadcast, video_config(), None);
		write_three_gops(&mut media);

		let server = Server::new(origin.consume(), crate::export::Config::default());
		let app = server.router();
		wait_listed(&app, "/live/video/video0/media.m3u8", "seg/0.m4s").await;
		let broadcaster = server.broadcaster("live").await.expect("broadcaster");
		broadcaster.set_generation(Some("run-1")).unwrap();

		let response = oneshot(app.clone(), "/live/video/video0/media.m3u8").await;
		let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
		let playlist = String::from_utf8(body.to_vec()).unwrap();
		assert!(playlist.contains("\nseg/run-1.0.m4s\n"), "{playlist}");
		let map = playlist.lines().find(|line| line.starts_with("#EXT-X-MAP:")).unwrap();
		let init = map
			.strip_prefix("#EXT-X-MAP:URI=\"")
			.unwrap()
			.strip_suffix('"')
			.unwrap();

		assert_eq!(
			status(&app, &format!("/live/video/video0/{init}")).await,
			StatusCode::OK
		);
		assert_eq!(status(&app, "/live/video/video0/init.mp4").await, StatusCode::NOT_FOUND);
		assert_eq!(
			status(&app, "/live/video/video0/init.0000000000000000.mp4").await,
			StatusCode::NOT_FOUND
		);

		assert_eq!(status(&app, "/live/video/video0/seg/run-1.0.m4s").await, StatusCode::OK);
		assert_eq!(
			status(&app, "/live/video/video0/seg/run-1.t0.m4s").await,
			StatusCode::OK
		);
		assert_eq!(
			status(&app, "/live/video/video0/seg/0.m4s").await,
			StatusCode::NOT_FOUND
		);
		assert_eq!(
			status(&app, "/live/video/video0/seg/run-0.0.m4s").await,
			StatusCode::NOT_FOUND
		);

		// A new run: the previous generation's URLs are refused even for numbers it reuses.
		broadcaster.set_generation(Some("run-2")).unwrap();
		assert_eq!(
			status(&app, "/live/video/video0/seg/run-1.0.m4s").await,
			StatusCode::NOT_FOUND
		);
	}

	/// A miss that crossed a moq-lite session answers 404, not 500.
	#[tokio::test]
	async fn a_session_crossed_cache_miss_answers_404() {
		let pool = moq_net::cache::Pool::new(moq_net::cache::Config::default().with_capacity(1));
		let pair = lite_pair_pub({
			let mut config = moq_net::origin::Config::default();
			config.pool = pool;
			config
		})
		.await;
		let mut broadcast = pair.pub_origin.create_broadcast("live").expect("publish");
		broadcast.announce(Default::default()).expect("announce");
		let (_catalog, _registration, _track, mut media) = publish_video(&mut broadcast, video_config(), None);
		write_three_gops(&mut media);

		let app = Server::new(pair.sub_origin.consume(), crate::export::Config::default()).router();
		wait_listed(&app, "/live/video/video0/media.m3u8", "seg/0.m4s").await;

		let response = oneshot(app, "/live/video/video0/seg/0.m4s").await;
		assert_eq!(response.status(), StatusCode::NOT_FOUND);
		assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");

		pair.accept.abort();
	}

	#[tokio::test]
	async fn a_disconnected_publisher_on_the_catalog_broadcast_answers_404() {
		let pair = lite_pair().await;
		let mut broadcast = pair.pub_origin.create_broadcast("live").expect("publish");
		broadcast.announce(Default::default()).expect("announce");
		let (catalog, registration, track, mut media) = publish_video(&mut broadcast, video_config(), None);
		write_three_gops(&mut media);

		let app = Server::new(pair.sub_origin.consume(), crate::export::Config::default()).router();
		wait_listed(&app, "/live/video/video0/media.m3u8", "seg/0.m4s").await;

		let remote = tokio::time::timeout(TIMEOUT, pair.sub_origin.consume().request_broadcast("live"))
			.await
			.expect("remote resolve timed out")
			.expect("remote broadcast");
		drop((catalog, registration, track, media, broadcast));
		tokio::time::timeout(TIMEOUT, remote.closed())
			.await
			.expect("publisher close timed out");

		let response = oneshot(app, "/live/video/video0/seg/0.m4s").await;
		assert_eq!(response.status(), StatusCode::NOT_FOUND);
		assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");

		pair.accept.abort();
	}

	#[tokio::test]
	async fn a_disconnected_sibling_publisher_answers_404() {
		let pair = lite_pair().await;
		let mut catalog_broadcast = pair.pub_origin.create_broadcast("room/live").expect("catalog");
		catalog_broadcast
			.announce(Default::default())
			.expect("announce catalog");
		let media_broadcast = pair.pub_origin.create_broadcast("room/source").expect("media");
		media_broadcast.announce(Default::default()).expect("announce media");

		let catalog =
			moq_mux::catalog::Producer::new(&mut catalog_broadcast, moq_mux::catalog::Config::default()).unwrap();
		let reserved = catalog.reserve();
		let mut config = video_config();
		config.broadcast = Some(moq_net::path::Relative::new("../source").to_owned());
		let track = media_broadcast.create_track("video0", None).unwrap();
		let mut media = reserved
			.video(
				track,
				moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Video),
				config,
			)
			.unwrap();
		drop(reserved);
		write_three_gops(&mut media);

		let app = Server::new(pair.sub_origin.consume(), crate::export::Config::default()).router();
		wait_listed(&app, "/room/live/video/video0/media.m3u8", "seg/0.m4s").await;

		let remote_media = tokio::time::timeout(TIMEOUT, pair.sub_origin.consume().request_broadcast("room/source"))
			.await
			.expect("sibling resolve timed out")
			.expect("sibling broadcast");
		drop(media);
		drop(media_broadcast);
		tokio::time::timeout(TIMEOUT, remote_media.closed())
			.await
			.expect("sibling close timed out");

		let response = oneshot(app, "/room/live/video/video0/seg/0.m4s").await;
		assert_eq!(response.status(), StatusCode::NOT_FOUND);
		assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");

		drop((catalog, catalog_broadcast));
		pair.accept.abort();
	}
}
