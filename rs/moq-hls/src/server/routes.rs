//! axum handlers for the HLS endpoints.

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::extract::{RawQuery, State};
use axum::http::{StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use bytes::Bytes;

use super::Server;
use crate::export::{Kind, Rendition, Resource, Route, segments};

const M3U8: &str = "application/vnd.apple.mpegurl";
const MPD: &str = "application/dash+xml";
const MP4: &str = "video/mp4";

/// How long a rendition lookup waits for the catalog (and its first HLS records) to
/// populate.
const READY_TIMEOUT: Duration = Duration::from_secs(5);

pub fn router(server: Server) -> Router {
	Router::new().route("/{*path}", get(request)).with_state(server)
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
			Resource::Media {
				instance,
				kind,
				rendition,
			} => media(self, At::new(broadcast, instance, *kind, rendition), query).await,
			Resource::Init {
				instance,
				kind,
				rendition,
				hash,
			} => init(self, At::new(broadcast, instance, *kind, rendition), hash).await,
			Resource::Segment {
				instance,
				kind,
				rendition,
				segment: id,
			} => segment(self, At::new(broadcast, instance, *kind, rendition), id).await,
		}
	}
}

async fn master(server: &Server, broadcast: &str, query: Option<&str>) -> Response {
	let Some(broadcaster) = server.broadcaster(broadcast).await else {
		return not_found();
	};
	// A master listing no variant is unplayable: wait for one that can start, else answer
	// unavailable.
	let _ = kio::clock::timeout(READY_TIMEOUT, broadcaster.advertised()).await;
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
	let _ = kio::clock::timeout(READY_TIMEOUT, broadcaster.ready()).await;
	if broadcaster.is_empty() {
		return not_found();
	}
	// A manifest whose renditions are all empty confuses players; give the broadcast a moment
	// to index its first complete segment before answering.
	let _ = kio::clock::timeout(READY_TIMEOUT, broadcaster.playable()).await;
	// Each representation names its init by hash, which an inline codec only learns from media.
	// A player keeps the adaptation sets of the first manifest it loads, so wait for one video
	// init rather than every init: a stuck rendition never holds a sibling that can play.
	let _ = kio::clock::timeout(READY_TIMEOUT, broadcaster.video_init()).await;
	match broadcaster.manifest(query) {
		Ok(Some(manifest)) => mpd(manifest),
		Ok(None) => not_found(),
		Err(err) => server_error(err),
	}
}

async fn media(server: &Server, at: At<'_>, query: Option<&str>) -> Response {
	let Some(rendition) = rendition_for(server, at).await else {
		return not_found();
	};
	match kio::clock::timeout(READY_TIMEOUT, rendition.playlist(query)).await {
		Ok(Ok(Some(playlist))) => m3u8(playlist),
		Ok(Ok(None)) | Err(_) => not_found(),
		Ok(Err(err)) => server_error(err),
	}
}

async fn init(server: &Server, at: At<'_>, hash: &str) -> Response {
	let Some(rendition) = rendition_for(server, at).await else {
		return not_found();
	};
	media_result(rendition.init(hash).await, server)
}

async fn segment(server: &Server, at: At<'_>, id: &segments::Id) -> Response {
	let Some(rendition) = rendition_for(server, at).await else {
		return not_found();
	};
	media_result(rendition.segment(id).await, server)
}

/// A rendition of one publisher instance, as an instance-scoped URL names it.
#[derive(Clone, Copy)]
struct At<'a> {
	broadcast: &'a str,
	instance: &'a str,
	kind: Kind,
	rendition: &'a str,
}

impl<'a> At<'a> {
	fn new(broadcast: &'a str, instance: &'a str, kind: Kind, rendition: &'a str) -> Self {
		Self {
			broadcast,
			instance,
			kind,
			rendition,
		}
	}
}

/// Resolve a rendition, waiting for the catalog to populate.
async fn rendition_for(server: &Server, at: At<'_>) -> Option<Arc<Rendition>> {
	let broadcaster = server.instance(at.broadcast, at.instance).await?;
	let _ = kio::clock::timeout(READY_TIMEOUT, broadcaster.ready()).await;
	broadcaster.rendition(at.kind, at.rendition)
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
	// Init/segment bytes never change while their URL is listed, but a run spliced into one
	// instance without a generation (see `Broadcaster::set_generation`) can repeat the old run's
	// segment numbers and pts with different media. Cap shared caching at the playlist window -
	// every concurrent viewer of the live window still hits the cache, while a stale run's bytes
	// age out as fast as the window that stopped listing them.
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
	// A catalog denied mid-stream still answers as a denial.
	let denied = match &err {
		crate::Error::CatalogFailed { source } => source.as_ref(),
		err => err,
	};
	let status = match denied {
		crate::Error::Moq(moq_net::Error::Unauthorized)
		| crate::Error::Mux(moq_mux::Error::Moq(moq_net::Error::Unauthorized)) => StatusCode::FORBIDDEN,
		_ => StatusCode::INTERNAL_SERVER_ERROR,
	};
	(status, [(header::CACHE_CONTROL, "no-store")]).into_response()
}

#[cfg(test)]
mod tests {
	use super::*;

	/// A millisecond track: the frames written here carry timestamps.
	fn timed() -> moq_net::track::Info {
		moq_net::track::Info::default().with_timescale(moq_net::Timescale::MILLI)
	}

	const TIMEOUT: Duration = Duration::from_secs(10);

	/// The epoch each test's catalog broadcast announces, and so the instance its URLs name.
	const RUN: &str = "01920000-0000-7000-8000-000000000000";

	fn run() -> moq_net::origin::Route {
		moq_net::origin::Route::default().with_epoch(RUN.parse().unwrap())
	}

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

	/// A catalog config recording two-second HLS segments, so its renditions are listed.
	fn hls_config() -> moq_mux::catalog::Config {
		let duration = Duration::from_secs(2);
		moq_mux::catalog::Config::default().with_hls(
			moq_mux::hls::Config::default()
				.with_duration(duration)
				.with_duration_hint(duration),
		)
	}

	fn video_config() -> hang::catalog::VideoConfig {
		let mut config = hang::catalog::VideoConfig::new(hang::catalog::VideoCodec::VP8);
		config.coded_width = Some(320);
		config.coded_height = Some(240);
		config.framerate = Some(30.0);
		config
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
		let deadline = kio::clock::now() + TIMEOUT;
		let mut last = String::new();
		loop {
			let response = oneshot(app.clone(), playlist).await;
			if response.status() == StatusCode::OK {
				let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
				last = String::from_utf8_lossy(&body).into_owned();
				if last.contains(segment) {
					return;
				}
			}
			assert!(
				kio::clock::now() < deadline,
				"{playlist} never listed {segment}: {last}"
			);
			kio::clock::sleep(Duration::from_millis(50)).await;
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
		let catalog = moq_mux::catalog::Producer::new(broadcast, hls_config()).unwrap();
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

	async fn status(app: &axum::Router, uri: &str) -> StatusCode {
		oneshot(app.clone(), uri).await.status()
	}

	/// A master listing no variant would be unplayable, so it answers 404 until a rendition can
	/// start.
	#[tokio::test(start_paused = true)]
	async fn master_answers_404_until_a_rendition_can_start() {
		let origin = moq_tokio::origin::spawn();
		let broadcast = origin.create_broadcast("live").expect("publish");
		broadcast.announce(run()).expect("announce");
		let (_catalog, _registration, _track, mut media) =
			publish_video(&mut broadcast.broadcast().clone(), video_config(), timed());

		let app = Server::new(origin.consume(), crate::export::Config::default()).router();
		assert_eq!(status(&app, "/live/master.m3u8").await, StatusCode::NOT_FOUND);

		write_three_gops(&mut media);
		wait_listed(&app, "/live/master.m3u8", "video/video0/media.m3u8").await;
	}

	/// A player keeps the adaptation sets of the first manifest it loads, so a fresh edge's first
	/// manifest already lists an inline-codec video rendition, whose init is only known from media.
	#[tokio::test(start_paused = true)]
	async fn the_first_manifest_lists_inline_video() {
		let origin = moq_tokio::origin::spawn();
		let broadcast = origin.create_broadcast("live").expect("publish");
		broadcast.announce(run()).expect("announce");
		let inline = hang::catalog::VideoConfig::new(hang::catalog::VideoCodec::VP8);
		let (_catalog, _registration, _track, mut media) =
			publish_video(&mut broadcast.broadcast().clone(), inline, timed());
		write_three_gops(&mut media);

		let app = Server::new(origin.consume(), crate::export::Config::default()).router();
		let response = oneshot(app, "/live/manifest.mpd").await;
		assert_eq!(response.status(), StatusCode::OK);
		let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
		let body = String::from_utf8(body.to_vec()).unwrap();
		assert!(body.contains("id=\"video/video0\""), "{body}");
	}

	/// A denied video can never be listed, so it never holds the manifest until the first-video
	/// wait times out: the playable audio is served at once.
	#[tokio::test(start_paused = true)]
	async fn a_denied_video_never_holds_the_manifest() {
		let origin = moq_tokio::origin::spawn();
		let broadcast = origin.create_broadcast("room/live").unwrap();
		broadcast.announce(run()).unwrap();
		let secret = origin.create_broadcast("room/secret").unwrap();
		secret.announce(Default::default()).unwrap();
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast.broadcast().clone(), hls_config()).unwrap();
		let reserved = catalog.reserve();
		let mut config = video_config();
		config.broadcast = Some(moq_net::path::Relative::new("./secret").unwrap());
		let mut video = reserved
			.video(
				secret.create_track("video0", timed()).unwrap(),
				moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Video),
				config,
			)
			.unwrap();
		let mut audio = reserved
			.audio(
				broadcast.create_track("audio0", timed()).unwrap(),
				moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Data),
				hang::catalog::AudioConfig::new(hang::catalog::AudioCodec::Opus, 48_000, 2),
			)
			.unwrap();
		drop(reserved);
		write_three_gops(&mut video);
		for micros in [0, 2_000_000, 4_000_000] {
			let keyframe = audio.needs_keyframe();
			audio.write(vp8_frame(micros, keyframe)).unwrap();
			audio.cut(None).unwrap();
		}

		let restricted = origin
			.consume()
			.scope(
				"",
				&moq_net::Patterns::from("room/live".parse::<moq_net::Pattern>().unwrap()),
			)
			.unwrap();
		let app = Server::new(restricted, Default::default()).router();
		let start = kio::clock::now();
		let response = oneshot(app, "/room/live/manifest.mpd").await;
		assert!(start.elapsed() < READY_TIMEOUT, "waited {:?}", start.elapsed());
		assert_eq!(response.status(), StatusCode::OK);
		let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
		let body = String::from_utf8(body.to_vec()).unwrap();
		assert!(body.contains("id=\"audio/audio0\""), "{body}");
		assert!(!body.contains("id=\"video/video0\""), "{body}");
	}

	/// A restarted publisher gets URLs of its own, whose numbering starts over: the master lists
	/// the replacement, while a player already on the old instance keeps its playlist until that
	/// instance's broadcast closes.
	#[tokio::test(start_paused = true)]
	async fn a_restart_lists_the_replacement_under_its_own_urls() {
		let origin = moq_tokio::origin::spawn();
		let old = origin.create_broadcast("live").expect("publish");
		old.announce(run()).expect("announce");
		let (old_catalog, _, old_track, mut old_media) =
			publish_video(&mut old.broadcast().clone(), video_config(), timed());
		write_three_gops(&mut old_media);

		let app = Server::new(origin.consume(), crate::export::Config::default()).router();
		let old_playlist = format!("/live/@{RUN}/video/video0/media.m3u8");
		let old_segment = format!("/live/@{RUN}/video/video0/seg/53e0aafd.0.m4s");
		wait_listed(&app, "/live/master.m3u8", &format!("@{RUN}/video/video0/media.m3u8")).await;
		wait_listed(&app, &old_playlist, "seg/53e0aafd.0.m4s").await;

		let next = moq_net::Epoch::mint();
		let new = origin.create_broadcast("live").expect("publish");
		new.announce(moq_net::origin::Route::default().with_epoch(next.clone()))
			.expect("announce");
		let (_new_catalog, _, _new_track, mut new_media) =
			publish_video(&mut new.broadcast().clone(), video_config(), timed());
		write_three_gops(&mut new_media);
		let new_playlist = format!("/live/@{}/video/video0/media.m3u8", next.as_str());
		wait_listed(
			&app,
			"/live/master.m3u8",
			&format!("@{}/video/video0/media.m3u8", next.as_str()),
		)
		.await;
		wait_listed(&app, &new_playlist, "seg/53e0aafd.0.m4s").await;
		assert_eq!(status(&app, &old_playlist).await, StatusCode::OK);
		assert_eq!(status(&app, &old_segment).await, StatusCode::OK);

		drop((old_catalog, old_track, old_media, old));
		let deadline = kio::clock::now() + TIMEOUT;
		while status(&app, &old_playlist).await != StatusCode::NOT_FOUND {
			assert!(kio::clock::now() < deadline, "the closed instance kept serving");
			kio::clock::sleep(Duration::from_millis(50)).await;
		}
		assert_eq!(status(&app, &old_segment).await, StatusCode::NOT_FOUND);
		assert_eq!(status(&app, &new_playlist).await, StatusCode::OK);
	}

	/// Failed records answer every listing with an uncacheable error rather than ending it as a
	/// complete recording.
	#[tokio::test(start_paused = true)]
	async fn failed_records_answer_an_uncacheable_error() {
		let origin = moq_tokio::origin::spawn();
		let broadcast = origin.create_broadcast("live").expect("publish");
		broadcast.announce(run()).expect("announce");
		let mut catalog =
			moq_mux::catalog::Producer::new(&mut broadcast.broadcast().clone(), Default::default()).unwrap();
		let name = hang::hls::default_name("video0");
		catalog
			.mutate(|catalog| {
				let mut config = video_config();
				config.hls = Some(hang::catalog::Hls::new(name.clone(), Duration::from_secs(2)));
				catalog.video.renditions.insert("video0".into(), config);
			})
			.unwrap();
		let track = broadcast.create_track(name, moq_mux::hls::Producer::info()).unwrap();
		let mut records = moq_mux::hls::Producer::new(track.clone());
		for sequence in 0..4 {
			let record = hang::hls::Record::new(sequence, sequence, sequence * 2_000);
			records.push(&record).unwrap();
		}

		let app = Server::new(origin.consume(), crate::export::Config::default()).router();
		wait_listed(&app, &format!("/live/@{RUN}/video/video0/media.m3u8"), ".2.m4s").await;
		assert_eq!(status(&app, "/live/manifest.mpd").await, StatusCode::OK);

		drop(records);
		track
			.abort(moq_net::Error::Transport("connection lost".into()))
			.unwrap();
		for uri in [&format!("/live/@{RUN}/video/video0/media.m3u8"), "/live/manifest.mpd"] {
			let deadline = kio::clock::now() + TIMEOUT;
			let response = loop {
				let response = oneshot(app.clone(), uri).await;
				if response.status() != StatusCode::OK {
					break response;
				}
				assert!(kio::clock::now() < deadline, "{uri} never failed");
				kio::clock::sleep(Duration::from_millis(50)).await;
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
		let broadcast = origin.create_broadcast("live").expect("publish");
		broadcast.announce(run()).expect("announce");
		let (_catalog, _registration, _track, mut media) =
			publish_video(&mut broadcast.broadcast().clone(), video_config(), timed());
		write_three_gops(&mut media);

		let server = Server::new(origin.consume(), crate::export::Config::default());
		let app = server.router();
		wait_listed(
			&app,
			&format!("/live/@{RUN}/video/video0/media.m3u8"),
			"seg/53e0aafd.0.m4s",
		)
		.await;
		let broadcaster = server.broadcaster("live").await.expect("broadcaster");
		broadcaster.set_generation(Some("run-1")).unwrap();

		let response = oneshot(app.clone(), &format!("/live/@{RUN}/video/video0/media.m3u8")).await;
		let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
		let playlist = String::from_utf8(body.to_vec()).unwrap();
		assert!(playlist.contains("\nseg/run-1.53e0aafd.0.m4s\n"), "{playlist}");
		let map = playlist.lines().find(|line| line.starts_with("#EXT-X-MAP:")).unwrap();
		let init = map
			.strip_prefix("#EXT-X-MAP:URI=\"")
			.unwrap()
			.strip_suffix('"')
			.unwrap();

		assert_eq!(
			status(&app, &format!("/live/@{RUN}/video/video0/{init}")).await,
			StatusCode::OK
		);
		assert_eq!(
			status(&app, &format!("/live/@{RUN}/video/video0/init.mp4")).await,
			StatusCode::NOT_FOUND
		);
		assert_eq!(
			status(&app, &format!("/live/@{RUN}/video/video0/init.0000000000000000.mp4")).await,
			StatusCode::NOT_FOUND
		);

		assert_eq!(
			status(&app, &format!("/live/@{RUN}/video/video0/seg/run-1.53e0aafd.0.m4s")).await,
			StatusCode::OK
		);
		assert_eq!(
			status(&app, &format!("/live/@{RUN}/video/video0/seg/run-1.53e0aafd.t0.m4s")).await,
			StatusCode::OK
		);
		assert_eq!(
			status(&app, &format!("/live/@{RUN}/video/video0/seg/53e0aafd.0.m4s")).await,
			StatusCode::NOT_FOUND
		);
		assert_eq!(
			status(&app, &format!("/live/@{RUN}/video/video0/seg/run-0.53e0aafd.0.m4s")).await,
			StatusCode::NOT_FOUND
		);

		// A new run: the previous generation's URLs are refused even for numbers it reuses.
		broadcaster.set_generation(Some("run-2")).unwrap();
		assert_eq!(
			status(&app, &format!("/live/@{RUN}/video/video0/seg/run-1.53e0aafd.0.m4s")).await,
			StatusCode::NOT_FOUND
		);
	}

	#[tokio::test(start_paused = true)]
	async fn denied_sibling_media_answers_403_without_poisoning_allowed_siblings() {
		let origin = moq_tokio::origin::spawn();
		let broadcast = origin.create_broadcast("room/live").unwrap();
		broadcast.announce(run()).unwrap();
		let source = origin.create_broadcast("room/source").unwrap();
		source.announce(Default::default()).unwrap();
		let secret = origin.create_broadcast("room/secret").unwrap();
		secret.announce(Default::default()).unwrap();
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast.broadcast().clone(), hls_config()).unwrap();
		let reserved = catalog.reserve();
		let mut config = video_config();
		config.broadcast = Some(moq_net::path::Relative::new("./source").unwrap());
		let mut allowed = reserved
			.video(
				source.create_track("video0", timed()).unwrap(),
				moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Video),
				config,
			)
			.unwrap();
		let mut config = video_config();
		config.broadcast = Some(moq_net::path::Relative::new("./secret").unwrap());
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
		let tag = |name| broadcaster.rendition(Kind::Video, name).unwrap().tag().to_string();
		let allowed_uri = &format!("/room/live/@{RUN}/video/video0/seg/{}.0.m4s", tag("video0"));
		let denied_uri = &format!("/room/live/@{RUN}/video/video1/seg/{}.0.m4s", tag("video1"));
		let app = server.router();
		assert_eq!(oneshot(app.clone(), allowed_uri).await.status(), StatusCode::OK);
		for uri in [
			denied_uri,
			&format!("/room/live/@{RUN}/video/video1/media.m3u8"),
			&format!("/room/live/@{RUN}/video/video1/init.unknown.mp4"),
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
		let first = |rendition: &Rendition| segments::Id {
			generation: rendition.generation().map(|generation| generation.to_string()),
			tag: rendition.tag().into(),
			at: segments::At::Sequence(0),
		};
		let rendition = broadcaster.rendition(Kind::Video, "video1").unwrap();
		assert!(matches!(
			rendition.segment(&first(&rendition)).await,
			Err(crate::Error::Mux(moq_mux::Error::Moq(moq_net::Error::Unauthorized)))
		));
		// Broader authority can consume the referenced bytes, but cannot warm the
		// restricted server's export into using them.
		let broad = moq_mux::Source::new(origin.consume(), broadcast.consume());
		let referenced = broad
			.resolve(Some(&moq_net::path::Relative::new("./secret").unwrap()), None)
			.await
			.unwrap();
		let mut group = referenced.track("video1").unwrap().fetch_group(0, None).await.unwrap();
		assert!(group.read_frame().await.unwrap().is_some());
		assert_eq!(oneshot(app.clone(), denied_uri).await.status(), StatusCode::FORBIDDEN);
		assert_eq!(oneshot(app.clone(), allowed_uri).await.status(), StatusCode::OK);
		assert_eq!(
			oneshot(
				app,
				&format!("/room/live/@{RUN}/video/video0/seg/{}.9999.m4s", tag("video0"))
			)
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

		// A records failure refuses listings, but its already-retained media
		// still drains. Authorization checks must not broaden that failure.
		let retained = broadcaster.rendition(Kind::Video, "video0").unwrap();
		retained.fail(crate::Error::Moq(moq_net::Error::Transport("connection lost".into())));
		assert!(retained.media_playlist(None).is_err());
		assert!(retained.latest_init().await.unwrap().is_some());
		assert!(retained.segment(&first(&retained)).await.unwrap().is_some());
	}
}
