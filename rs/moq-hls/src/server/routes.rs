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
	/// `{kind}/{rendition}/seg/[{generation}.]{reference}.{sequence}.m4s`: a segment by its HLS
	/// number.
	#[non_exhaustive]
	Segment {
		/// The rendition's kind.
		kind: Kind,
		/// The rendition's name, percent-decoded.
		rendition: String,
		/// The publisher run the URL names, if the broadcaster carries one.
		generation: Option<String>,
		/// The tag of the reference rendition whose records number the segment.
		reference: String,
		/// The segment's aligned number.
		sequence: u64,
	},
	/// `{kind}/{rendition}/seg/[{generation}.]{reference}.t{pts}.m4s`: the same bytes, addressed
	/// by the DASH timeline pts (`$Time$`).
	#[non_exhaustive]
	SegmentAt {
		/// The rendition's kind.
		kind: Kind,
		/// The rendition's name, percent-decoded.
		rendition: String,
		/// The publisher run the URL names, if the broadcaster carries one.
		generation: Option<String>,
		/// The tag of the reference rendition whose records number the segment.
		reference: String,
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
				// `{generation}.{reference}.{segment}` when the export carries a generation,
				// `{reference}.{segment}` otherwise.
				let (generation, reference, stem) = match stem.split('.').collect::<Vec<_>>().as_slice() {
					[reference, stem] => (None, reference.to_string(), *stem),
					[generation, reference, stem] => (Some(generation.to_string()), reference.to_string(), *stem),
					_ => return None,
				};
				let resource = match stem.strip_prefix('t') {
					Some(pts) => Resource::SegmentAt {
						kind,
						rendition,
						generation,
						reference,
						pts: pts.parse().ok()?,
					},
					None => Resource::Segment {
						kind,
						rendition,
						generation,
						reference,
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
				reference,
				sequence,
			} => {
				let at = SegmentAt::Sequence(reference, *sequence);
				segment(self, broadcast, *kind, rendition, generation.as_deref(), at).await
			}
			Resource::SegmentAt {
				kind,
				rendition,
				generation,
				reference,
				pts,
			} => {
				let at = SegmentAt::Pts(reference, *pts);
				segment(self, broadcast, *kind, rendition, generation.as_deref(), at).await
			}
		}
	}
}

async fn master(server: &Server, broadcast: &str, query: Option<&str>) -> Response {
	let Some(broadcaster) = server.broadcaster(broadcast).await else {
		return not_found();
	};
	// A master listing no variant is unplayable: wait for one that can start, else answer
	// unavailable.
	let _ = moq_clock::timeout(READY_TIMEOUT, broadcaster.advertised()).await;
	let (video, audio) = broadcaster.variants(query);
	if video.is_empty() && audio.is_empty() {
		return not_found();
	}
	m3u8(crate::export::master::render(&video, &audio))
}

async fn manifest(server: &Server, broadcast: &str, query: Option<&str>) -> Response {
	let Some(broadcaster) = server.broadcaster(broadcast).await else {
		return not_found();
	};
	let _ = moq_clock::timeout(READY_TIMEOUT, broadcaster.ready()).await;
	if broadcaster.is_empty() {
		return not_found();
	}
	// A manifest whose timelines are all empty confuses players; give the broadcast a moment
	// to index its first complete segment before answering.
	let _ = moq_clock::timeout(READY_TIMEOUT, broadcaster.playable()).await;
	// Each representation names its init by hash, which an inline codec only learns from media.
	broadcaster.build_inits().await;
	match broadcaster.manifest(query) {
		Ok(Some(manifest)) => mpd(manifest),
		Ok(None) => not_found(),
		Err(err) => server_error(err),
	}
}

async fn media(server: &Server, broadcast: &str, kind: Kind, rendition: &str, query: Option<&str>) -> Response {
	let (range, child_query) = match playlist_range(query) {
		Ok(parsed) => parsed,
		Err(()) => return bad_request(),
	};
	let Some(rendition) = rendition_for(server, broadcast, kind, rendition).await else {
		return not_found();
	};
	match moq_clock::timeout(READY_TIMEOUT, rendition.playlist(range, child_query.as_deref())).await {
		Ok(Ok(Some(playlist))) => m3u8(playlist),
		Ok(Ok(None)) | Err(_) => not_found(),
		Ok(Err(crate::Error::RangeNotDurable | crate::Error::RangeUnavailable(_))) => range_not_satisfiable(),
		Ok(Err(err)) => server_error(err),
	}
}

/// Parse `start` and `end` as decimal media-time seconds, returning the remaining query for
/// child resources. Both are required together and may occur only once. Other parameters pass
/// through byte for byte, so a signed token is never re-encoded.
fn playlist_range(query: Option<&str>) -> Result<(Option<crate::export::Range>, Option<String>), ()> {
	let Some(query) = query else {
		return Ok((None, None));
	};
	let mut start = None;
	let mut end = None;
	let mut child = Vec::new();
	for pair in query.split('&') {
		let Some((key, value)) = url::form_urlencoded::parse(pair.as_bytes()).next() else {
			continue;
		};
		match key.as_ref() {
			"start" if start.is_none() => start = Some(media_time(&value)?),
			"end" if end.is_none() => end = Some(media_time(&value)?),
			"start" | "end" => return Err(()),
			_ => child.push(pair),
		}
	}
	let range = match (start, end) {
		(None, None) => None,
		(Some(start), Some(end)) => Some(crate::export::Range::new(start, end).map_err(|_| ())?),
		_ => return Err(()),
	};
	let child = child.join("&");
	Ok((range, (!child.is_empty()).then_some(child)))
}

fn media_time(value: &str) -> Result<Duration, ()> {
	let seconds = value.parse::<f64>().map_err(|_| ())?;
	if !seconds.is_finite() || seconds < 0.0 {
		return Err(());
	}
	Duration::try_from_secs_f64(seconds).map_err(|_| ())
}

async fn init(server: &Server, broadcast: &str, kind: Kind, rendition: &str, hash: &str) -> Response {
	let Some(rendition) = rendition_for(server, broadcast, kind, rendition).await else {
		return not_found();
	};
	media_result(rendition.init_versioned(hash).await, server)
}

/// How a segment URL addresses its bytes under its reference's tag: HLS by aligned number
/// (`seg/{tag}.0.m4s`), DASH by timeline pts (`seg/{tag}.t2000.m4s`, the SegmentTemplate's
/// `$Time$`).
enum SegmentAt<'a> {
	Sequence(&'a str, u64),
	Pts(&'a str, u64),
}

async fn segment(
	server: &Server,
	broadcast: &str,
	kind: Kind,
	rendition: &str,
	generation: Option<&str>,
	at: SegmentAt<'_>,
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
		SegmentAt::Sequence(reference, sequence) => rendition.segment(reference, sequence).await,
		SegmentAt::Pts(reference, pts) => rendition.segment_at(reference, pts).await,
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
	let _ = moq_clock::timeout(READY_TIMEOUT, broadcaster.ready()).await;
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

fn bad_request() -> Response {
	(StatusCode::BAD_REQUEST, [(header::CACHE_CONTROL, "no-store")]).into_response()
}

fn range_not_satisfiable() -> Response {
	(StatusCode::RANGE_NOT_SATISFIABLE, [(header::CACHE_CONTROL, "no-store")]).into_response()
}

fn server_error(err: crate::Error) -> Response {
	tracing::warn!(%err, "hls request failed");
	let status = match err {
		crate::Error::Moq(moq_net::Error::Unauthorized)
		| crate::Error::Mux(moq_mux::Error::Moq(moq_net::Error::Unauthorized)) => StatusCode::FORBIDDEN,
		_ => StatusCode::INTERNAL_SERVER_ERROR,
	};
	(status, [(header::CACHE_CONTROL, "no-store")]).into_response()
}

#[cfg(test)]
#[expect(
	clippy::disallowed_methods,
	clippy::disallowed_types,
	reason = "tests read the host clock directly"
)]
mod tests {
	use super::*;

	#[test]
	fn parses_playlist_ranges_and_preserves_other_query_parameters() {
		let (range, query) = playlist_range(Some("jwt=a%2Eb&start=1.25&sig=x~y*&end=4")).unwrap();
		assert_eq!(
			range,
			Some(crate::export::Range::new(Duration::from_millis(1250), Duration::from_secs(4)).unwrap())
		);
		assert_eq!(query.as_deref(), Some("jwt=a%2Eb&sig=x~y*"));
		assert_eq!(
			playlist_range(Some("jwt=x&flag")).unwrap(),
			(None, Some("jwt=x&flag".into()))
		);

		for invalid in [
			"start=1",
			"end=2",
			"start=2&end=1",
			"start=-1&end=2",
			"start=wat&end=2",
			"start=1&start=2&end=3",
		] {
			assert!(playlist_range(Some(invalid)).is_err(), "accepted {invalid}");
		}
	}

	/// A millisecond track: the frames written here carry timestamps.
	fn timed() -> moq_net::track::Info {
		moq_net::track::Info::default().with_timescale(moq_net::Timescale::MILLI)
	}

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
	fn parses_encoded_dash_rendition_as_one_segment() {
		let parsed = |path| {
			let route = Route::parse(path)?;
			let rendition = match &route.resource {
				Resource::Media { rendition, .. }
				| Resource::Init { rendition, .. }
				| Resource::Segment { rendition, .. }
				| Resource::SegmentAt { rendition, .. } => rendition.clone(),
				Resource::Master | Resource::Manifest => return None,
			};
			Some((route.broadcast, rendition))
		};

		// The manifest for broadcast `live` and rendition `video/1080p` emits these paths.
		// Unencoded, the same name is broadcast `live/video`, rendition `1080p`.
		let cases = [
			("/live/video/video%2F1080p/init.0123abcd.mp4", "video/1080p"),
			("/live/video/video%2F1080p/seg/ab12cd34.t2000.m4s", "video/1080p"),
			("/live/video/cam%3F1/init.0123abcd.mp4", "cam?1"),
			("/live/video/cam%231/media.m3u8", "cam#1"),
			("/live/audio/100%25/init.0123abcd.mp4", "100%"),
			("/live/video/price%249/seg/ab12cd34.t0.m4s", "price$9"),
		];
		for (path, rendition) in cases {
			assert_eq!(parsed(path), Some(("live".into(), rendition.into())), "{path}");
		}
		assert_eq!(
			parsed("/live/video/video/1080p/init.0123abcd.mp4"),
			Some(("live/video".into(), "1080p".into()))
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
			resource("/project/live/video/main/seg/ab12cd34.42.m4s"),
			Some(Resource::Segment {
				kind: Kind::Video,
				rendition: "main".to_string(),
				generation: None,
				reference: "ab12cd34".to_string(),
				sequence: 42,
			})
		);
		assert_eq!(
			resource("/project/live/video/main/seg/run-1.ab12cd34.42.m4s"),
			Some(Resource::Segment {
				kind: Kind::Video,
				rendition: "main".to_string(),
				generation: Some("run-1".to_string()),
				reference: "ab12cd34".to_string(),
				sequence: 42,
			})
		);
		assert_eq!(
			resource("/project/live/video/main/seg/ab12cd34.t2000.m4s"),
			Some(Resource::SegmentAt {
				kind: Kind::Video,
				rendition: "main".to_string(),
				generation: None,
				reference: "ab12cd34".to_string(),
				pts: 2000,
			})
		);
		assert_eq!(
			resource("/project/live/video/main/seg/init.ab12cd34.t2000.m4s"),
			Some(Resource::SegmentAt {
				kind: Kind::Video,
				rendition: "main".to_string(),
				generation: Some("init".to_string()),
				reference: "ab12cd34".to_string(),
				pts: 2000,
			})
		);
		assert!(Route::parse("/master.m3u8").is_none());
		assert!(Route::parse("/project/live/video/main/unknown").is_none());
		assert!(Route::parse("/project/live/data/main/media.m3u8").is_none());
		assert!(Route::parse("/project/live/video/main/init.mp4").is_none());
		assert!(Route::parse("/project/live/video/main/init..mp4").is_none());
		assert!(Route::parse("/project/live/video/main/seg/ab12cd34.tx.m4s").is_none());
		assert!(
			Route::parse("/project/live/video/main/seg/42.m4s").is_none(),
			"a segment URL names its reference"
		);
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
		lite_pair_on("moq-lite-05", config).await
	}

	async fn lite_pair_on(version: &str, config: impl Into<moq_net::origin::Config>) -> LitePair {
		let lite: moq_net::Version = version.parse().expect("lite version");
		let pub_origin = moq_tokio::origin::spawn_config(config.into());
		let sub_origin = moq_tokio::origin::spawn();

		let mut listen = moq_tokio::listen::Config::default();
		listen.tcp.bind = Some("127.0.0.1:0".parse().expect("addr"));
		listen.version = vec![lite];
		let mut server = listen
			.init(Default::default())
			.expect("server")
			.listen()
			.await
			.expect("listen");
		let addr = server.tcp_local_addr().expect("tcp listener bound");

		let pub_for_accept = pub_origin.clone();
		let accept = tokio::spawn(async move {
			let request = server.accept().await.expect("accept");
			let (session, driver) = request
				.with_publisher(&pub_for_accept)
				.ok()
				.await
				.expect("publisher session");
			let _ = driver.await;
			let _ = session.closed().await;
		});

		let mut connect = moq_tokio::connect::Config::default();
		connect.version = vec![lite];
		let client = connect
			.init(Default::default())
			.expect("client")
			.with_subscriber(sub_origin.clone())
			.with_reconnect(false);
		let url: url::Url = format!("tcp://{addr}/").parse().expect("url");
		let connection = tokio::time::timeout(TIMEOUT, client.connect(url).established())
			.await
			.expect("connect timed out")
			.expect("connect");

		LitePair {
			pub_origin,
			sub_origin,
			_connection: connection,
			accept,
		}
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
		let (_catalog, _registration, _track, mut media) = publish_video(&mut broadcast, video_config(), timed());
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
				if body.contains("seg/656e3d1b.0.m4s") {
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
		assert!(body.contains("seg/656e3d1b.0.m4s?jwt=abc"), "{body}");

		pair.accept.abort();
	}

	/// A recorder's cursor gets the media of a publisher whose moq-lite has no FETCH: the
	/// cursor's own subscription carries every group to the subscriber's cache.
	#[tokio::test]
	async fn a_cursor_records_a_publisher_without_fetch() {
		for version in [
			"moq-lite-01",
			"moq-lite-02",
			"moq-lite-03",
			"moq-lite-04",
			"moq-lite-05",
		] {
			let pair = lite_pair_on(version, moq_net::Hop::random()).await;
			let mut broadcast = pair.pub_origin.create_broadcast("live").expect("publish");
			broadcast.announce(Default::default()).expect("announce");
			// A hinted timeline is declared before any media, so the rendition is listed early.
			let hinted = moq_mux::timeline::Config::default().with_duration_hint(Duration::from_secs(2));
			let catalog = moq_mux::catalog::Producer::new(
				&mut broadcast,
				moq_mux::catalog::Config::default().with_timeline(hinted),
			)
			.unwrap();
			let reserved = catalog.reserve();
			let track = broadcast.create_track("video0", timed()).unwrap();
			let mut media = reserved
				.video(
					track.clone(),
					moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Video),
					video_config(),
				)
				.unwrap();
			drop(reserved);

			let origin = pair.sub_origin.consume();
			tokio::time::timeout(TIMEOUT, origin.routed("live"))
				.await
				.expect("announce timed out")
				.expect("routed");
			let remote = tokio::time::timeout(TIMEOUT, origin.request_broadcast("live", None))
				.await
				.expect("request timed out")
				.expect("remote broadcast");
			let source = moq_mux::Source::new(origin, remote);
			let broadcaster = crate::export::Broadcaster::new(source, crate::export::Config::default());
			// Recorded as soon as the catalog lists it with its timeline, as a recorder does.
			let mut renditions = broadcaster.renditions();
			let rendition = match tokio::time::timeout(TIMEOUT, renditions.next()).await {
				Ok(Ok(Some(crate::export::renditions::Event::Added(rendition)))) => rendition,
				_ => panic!("{version}: the catalog never listed the rendition"),
			};
			let mut cursor = rendition.segments();
			let mut next = std::pin::pin!(cursor.next());
			// Waiting for its first segment, the cursor subscribes before any media exists.
			let mut demand = track.demand();
			let wanted = async {
				while demand.wanted_changed().await?.is_none() {}
				moq_net::Result::Ok(())
			};
			tokio::time::timeout(TIMEOUT, async {
				tokio::select! {
					_ = next.as_mut() => panic!("{version}: a segment before any media"),
					wanted = wanted => wanted.expect("track open"),
				}
			})
			.await
			.unwrap_or_else(|_| panic!("{version}: the cursor never subscribed"));

			write_three_gops(&mut media);
			let segment = tokio::time::timeout(TIMEOUT, next)
				.await
				.unwrap_or_else(|_| panic!("{version}: no segment recorded"))
				.unwrap_or_else(|err| panic!("{version}: {err}"))
				.expect("a segment");
			assert_eq!(segment.segment, 0, "{version}");
			assert!(!segment.media.is_empty(), "{version}");

			pair.accept.abort();
		}
	}

	async fn status(app: &axum::Router, uri: &str) -> StatusCode {
		oneshot(app.clone(), uri).await.status()
	}

	/// A master listing no variant would be unplayable, so it answers 404 until a rendition can
	/// start.
	#[tokio::test(start_paused = true)]
	async fn master_answers_404_until_a_rendition_can_start() {
		let origin = moq_tokio::origin::spawn();
		let mut broadcast = origin.create_broadcast("live").expect("publish");
		broadcast.announce(Default::default()).expect("announce");
		let (_catalog, _registration, _track, mut media) = publish_video(&mut broadcast, video_config(), timed());

		let app = Server::new(origin.consume(), crate::export::Config::default()).router();
		assert_eq!(status(&app, "/live/master.m3u8").await, StatusCode::NOT_FOUND);

		write_three_gops(&mut media);
		wait_listed(&app, "/live/master.m3u8", "video/video0/media.m3u8").await;
	}

	/// A failed timeline answers every listing, a clip included, with an uncacheable error
	/// rather than ending it as a complete recording or refusing the range.
	#[tokio::test(start_paused = true)]
	async fn a_failed_timeline_answers_an_uncacheable_error() {
		let origin = moq_tokio::origin::spawn();
		let mut broadcast = origin.create_broadcast("live").expect("publish");
		broadcast.announce(Default::default()).expect("announce");
		let mut catalog = moq_mux::catalog::Producer::new(&mut broadcast, Default::default()).unwrap();
		let name = hang::timeline::default_name("video0");
		catalog
			.mutate(|catalog| {
				catalog.video.renditions.insert("video0".into(), video_config());
				let mut archive = hang::catalog::Archive::new();
				archive
					.timelines
					.insert("video0".into(), hang::catalog::Timeline::new(name.clone(), 1_000));
				archive.store = Some("https://example.com/recording/".parse().unwrap());
				catalog.archive = Some(archive);
			})
			.unwrap();
		let track = broadcast
			.create_track(name, moq_mux::timeline::Producer::info())
			.unwrap();
		let mut timeline = moq_mux::timeline::Producer::new(track.clone());
		for sequence in 0..3 {
			let start = hang::timeline::Position::group(sequence);
			let end = hang::timeline::Position::group(sequence + 1);
			let record = hang::timeline::Record::new(sequence, sequence * 2_000, 2_000, start, end);
			timeline.push(&record).unwrap();
		}

		let config = crate::export::Config {
			history: true,
			..Default::default()
		};
		let app = Server::new(origin.consume(), config).router();
		wait_listed(&app, "/live/video/video0/media.m3u8", ".2.m4s").await;
		assert_eq!(status(&app, "/live/manifest.mpd").await, StatusCode::OK);

		drop(timeline);
		track
			.abort(moq_net::Error::Transport("connection lost".into()))
			.unwrap();
		for uri in [
			"/live/video/video0/media.m3u8",
			"/live/video/video0/media.m3u8?start=0&end=2",
			"/live/manifest.mpd",
		] {
			let deadline = tokio::time::Instant::now() + TIMEOUT;
			let response = loop {
				let response = oneshot(app.clone(), uri).await;
				if response.status() != StatusCode::OK {
					break response;
				}
				assert!(tokio::time::Instant::now() < deadline, "{uri} never failed");
				tokio::time::sleep(Duration::from_millis(50)).await;
			};
			assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR, "{uri}");
			assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store", "{uri}");
		}
	}

	/// Only the URLs the renderers emit are served: the init under its hash, segments under the
	/// current generation. Paused, since a refused lookup waits out [`READY_TIMEOUT`].
	#[tokio::test(start_paused = true)]
	async fn serves_only_versioned_media_urls() {
		let origin = moq_tokio::origin::spawn();
		let mut broadcast = origin.create_broadcast("live").expect("publish");
		broadcast.announce(Default::default()).expect("announce");
		let (_catalog, _registration, _track, mut media) = publish_video(&mut broadcast, video_config(), timed());
		write_three_gops(&mut media);

		let server = Server::new(origin.consume(), crate::export::Config::default());
		let app = server.router();
		wait_listed(&app, "/live/video/video0/media.m3u8", "seg/656e3d1b.0.m4s").await;
		let broadcaster = server.broadcaster("live").await.expect("broadcaster");
		broadcaster.set_generation(Some("run-1")).unwrap();

		let response = oneshot(app.clone(), "/live/video/video0/media.m3u8").await;
		let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
		let playlist = String::from_utf8(body.to_vec()).unwrap();
		assert!(playlist.contains("\nseg/run-1.656e3d1b.0.m4s\n"), "{playlist}");
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

		assert_eq!(
			status(&app, "/live/video/video0/seg/run-1.656e3d1b.0.m4s").await,
			StatusCode::OK
		);
		assert_eq!(
			status(&app, "/live/video/video0/seg/run-1.656e3d1b.t0.m4s").await,
			StatusCode::OK
		);
		assert_eq!(
			status(&app, "/live/video/video0/seg/656e3d1b.0.m4s").await,
			StatusCode::NOT_FOUND
		);
		assert_eq!(
			status(&app, "/live/video/video0/seg/run-0.656e3d1b.0.m4s").await,
			StatusCode::NOT_FOUND
		);

		// A new run: the previous generation's URLs are refused even for numbers it reuses.
		broadcaster.set_generation(Some("run-2")).unwrap();
		assert_eq!(
			status(&app, "/live/video/video0/seg/run-1.656e3d1b.0.m4s").await,
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
		let (_catalog, _registration, _track, mut media) = publish_video(&mut broadcast, video_config(), timed());
		write_three_gops(&mut media);

		let app = Server::new(pair.sub_origin.consume(), crate::export::Config::default()).router();
		wait_listed(&app, "/live/video/video0/media.m3u8", "seg/656e3d1b.0.m4s").await;

		let response = oneshot(app, "/live/video/video0/seg/656e3d1b.0.m4s").await;
		assert_eq!(response.status(), StatusCode::NOT_FOUND);
		assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");

		pair.accept.abort();
	}

	#[tokio::test]
	async fn a_disconnected_publisher_on_the_catalog_broadcast_answers_404() {
		let pair = lite_pair().await;
		let mut broadcast = pair.pub_origin.create_broadcast("live").expect("publish");
		broadcast.announce(Default::default()).expect("announce");
		let (catalog, registration, track, mut media) = publish_video(&mut broadcast, video_config(), timed());
		write_three_gops(&mut media);

		let app = Server::new(pair.sub_origin.consume(), crate::export::Config::default()).router();
		wait_listed(&app, "/live/video/video0/media.m3u8", "seg/656e3d1b.0.m4s").await;

		let remote = tokio::time::timeout(TIMEOUT, pair.sub_origin.consume().request_broadcast("live", None))
			.await
			.expect("remote resolve timed out")
			.expect("remote broadcast");
		drop((catalog, registration, track, media, broadcast));
		tokio::time::timeout(TIMEOUT, remote.closed())
			.await
			.expect("publisher close timed out");

		let response = oneshot(app, "/live/video/video0/seg/656e3d1b.0.m4s").await;
		assert_eq!(response.status(), StatusCode::NOT_FOUND);
		assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");

		pair.accept.abort();
	}

	/// The server watches its whole scope, so a lookup rides one request to the peer
	/// rather than opening its own for every path an HTTP client names.
	#[tokio::test]
	async fn lookups_ride_the_servers_scope_watch() {
		let pair = lite_pair().await;
		let other = pair.pub_origin.create_broadcast("room/other").expect("other");
		other.announce(Default::default()).expect("announce other");

		let server = Server::new(pair.sub_origin.consume(), crate::export::Config::default());
		let missing = tokio::time::timeout(TIMEOUT, server.broadcaster("room/missing"))
			.await
			.expect("lookup timed out");
		assert!(missing.is_none());

		// Nothing watches `room/other` on its own, yet the synced lookup already discovered it.
		pair.sub_origin
			.consume()
			.request_broadcast("room/other", None)
			.await
			.expect("discovered through the scope watch");

		drop(other);
		pair.accept.abort();
	}

	/// A client asks its peer only about watched paths, so a sibling no listing covers is
	/// asked about before it is requested, on every lite version, including those whose
	/// announce stream marks no initial set.
	#[tokio::test]
	async fn a_sibling_resolves_through_a_client_session() {
		for version in [
			"moq-lite-01",
			"moq-lite-02",
			"moq-lite-03",
			"moq-lite-04",
			"moq-lite-05",
			"moq-lite-06",
			"moq-lite-07-wip",
		] {
			let pair = lite_pair_on(version, moq_net::origin::Config::default()).await;
			let catalog = pair.pub_origin.create_broadcast("room/live").expect("catalog");
			catalog.announce(Default::default()).expect("announce catalog");
			let media = pair.pub_origin.create_broadcast("room/source").expect("media");
			media.announce(Default::default()).expect("announce media");

			let origin = pair.sub_origin.consume();
			let broadcast = tokio::time::timeout(TIMEOUT, origin.routed_broadcast("room/live"))
				.await
				.expect("catalog resolve timed out")
				.expect("catalog broadcast");
			let source = moq_mux::Source::new(origin, broadcast);
			let sibling = tokio::time::timeout(
				TIMEOUT,
				source.resolve(Some(&moq_net::path::Relative::new("./source")), None),
			)
			.await
			.expect("sibling resolve timed out")
			.unwrap_or_else(|err| panic!("{version}: sibling broadcast: {err}"));
			assert!(!sibling.is_closed(), "{version}");

			drop((catalog, media));
			pair.accept.abort();
		}
	}

	#[tokio::test(start_paused = true)]
	async fn denied_sibling_media_answers_403_without_poisoning_allowed_siblings() {
		let origin = moq_tokio::origin::spawn();
		let mut broadcast = origin.create_broadcast("room/live").unwrap();
		broadcast.announce(Default::default()).unwrap();
		let source = origin.create_broadcast("room/source").unwrap();
		source.announce(Default::default()).unwrap();
		let secret = origin.create_broadcast("room/secret").unwrap();
		secret.announce(Default::default()).unwrap();
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast, Default::default()).unwrap();
		let reserved = catalog.reserve();
		let mut config = video_config();
		config.broadcast = Some(moq_net::path::Relative::new("./source").to_owned());
		let mut allowed = reserved
			.video(
				source.create_track("video0", timed()).unwrap(),
				moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Video),
				config,
			)
			.unwrap();
		let mut config = video_config();
		config.broadcast = Some(moq_net::path::Relative::new("./secret").to_owned());
		let mut denied = reserved
			.video(
				secret.create_track("video1", timed()).unwrap(),
				moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Video),
				config,
			)
			.unwrap();
		drop(reserved);
		write_three_gops(&mut allowed);
		write_three_gops(&mut denied);

		let patterns = ["room/live", "room/source"]
			.into_iter()
			.map(|path| path.parse::<moq_net::Pattern>().unwrap())
			.collect();
		let restricted = origin.consume().scope("", &patterns).unwrap();
		let server = Server::new(restricted, Default::default());
		let broadcaster = server.broadcaster("room/live").await.unwrap();
		broadcaster.ready().await;
		let allowed_uri = "/room/live/video/video0/seg/656e3d1b.0.m4s";
		let denied_uri = "/room/live/video/video1/seg/656e3d1b.0.m4s";
		let app = server.router();
		assert_eq!(oneshot(app.clone(), allowed_uri).await.status(), StatusCode::OK);
		for uri in [
			denied_uri,
			"/room/live/video/video1/media.m3u8",
			"/room/live/video/video1/media.m3u8?start=0&end=2",
			"/room/live/video/video1/init.unknown.mp4",
		] {
			let response = oneshot(app.clone(), uri).await;
			assert_eq!(response.status(), StatusCode::FORBIDDEN, "{uri}");
			assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
			assert!(
				axum::body::to_bytes(response.into_body(), usize::MAX)
					.await
					.unwrap()
					.is_empty()
			);
		}
		let master = oneshot(app.clone(), "/room/live/master.m3u8").await;
		assert_eq!(master.status(), StatusCode::OK);
		let master = axum::body::to_bytes(master.into_body(), usize::MAX).await.unwrap();
		let master = String::from_utf8(master.to_vec()).unwrap();
		assert!(master.contains("video/video0/"));
		assert!(!master.contains("video/video1/"));
		let rendition = broadcaster.rendition(Kind::Video, "video1").unwrap();
		assert!(matches!(
			rendition.segment("656e3d1b", 0).await,
			Err(crate::Error::Mux(moq_mux::Error::Moq(moq_net::Error::Unauthorized)))
		));
		// Broader authority can consume the referenced bytes, but cannot warm the
		// restricted server's export into using them.
		let broad = moq_mux::Source::new(origin.consume(), broadcast.consume());
		let referenced = broad
			.resolve(Some(&moq_net::path::Relative::new("./secret")), None)
			.await
			.unwrap();
		let mut group = referenced.track("video1").unwrap().fetch_group(0, None).await.unwrap();
		assert!(group.read_frame().await.unwrap().is_some());
		assert_eq!(oneshot(app.clone(), denied_uri).await.status(), StatusCode::FORBIDDEN);
		assert_eq!(oneshot(app.clone(), allowed_uri).await.status(), StatusCode::OK);
		assert_eq!(
			oneshot(app, "/room/live/video/video0/seg/656e3d1b.9999.m4s")
				.await
				.status(),
			StatusCode::NOT_FOUND
		);
		// Readiness settles even when no rendition is authorized; it must not need
		// an allowed sibling to wake the server into reporting the typed refusal.
		let only_catalog = origin
			.consume()
			.scope(
				"",
				&moq_net::Patterns::from("room/live".parse::<moq_net::Pattern>().unwrap()),
			)
			.unwrap();
		let all_denied = Server::new(only_catalog, Default::default());
		all_denied.broadcaster("room/live").await.unwrap().ready().await;
		assert_eq!(
			oneshot(all_denied.router(), denied_uri).await.status(),
			StatusCode::FORBIDDEN
		);

		// A timeline failure refuses listings, but its already-retained media
		// still drains. Authorization checks must not broaden that failure.
		let retained = broadcaster.rendition(Kind::Video, "video0").unwrap();
		retained.fail(crate::Error::Moq(moq_net::Error::Transport("connection lost".into())));
		assert!(retained.media_playlist(None).is_err());
		assert!(retained.init().await.unwrap().is_some());
		assert!(retained.segment("656e3d1b", 0).await.unwrap().is_some());
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
		config.broadcast = Some(moq_net::path::Relative::new("./source").to_owned());
		let track = media_broadcast.create_track("video0", timed()).unwrap();
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
		wait_listed(&app, "/room/live/video/video0/media.m3u8", "seg/656e3d1b.0.m4s").await;

		// Waited on rather than requested once: a client only asks about what it watches.
		let remote_media = tokio::time::timeout(TIMEOUT, pair.sub_origin.consume().routed_broadcast("room/source"))
			.await
			.expect("sibling resolve timed out")
			.expect("sibling broadcast");
		// Only the sibling goes: the catalog keeps listing the rendition, so the request
		// reaches the media fetch rather than missing the rendition.
		media_broadcast.close();
		tokio::time::timeout(TIMEOUT, remote_media.closed())
			.await
			.expect("sibling close timed out");

		let response = oneshot(app, "/room/live/video/video0/seg/656e3d1b.0.m4s").await;
		assert_eq!(response.status(), StatusCode::NOT_FOUND);
		assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");

		drop((catalog, catalog_broadcast, media));
		pair.accept.abort();
	}
}
