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

enum Route {
	Master {
		broadcast: String,
	},
	Manifest {
		broadcast: String,
	},
	Media {
		broadcast: String,
		kind: String,
		rendition: String,
	},
	Init {
		broadcast: String,
		kind: String,
		rendition: String,
	},
	Segment {
		broadcast: String,
		kind: String,
		rendition: String,
		file: String,
	},
}

fn broadcast_path(parts: &[String]) -> Option<String> {
	(!parts.is_empty() && parts.iter().all(|part| !part.contains('/'))).then(|| parts.join("/"))
}

fn parse_route(path: &str) -> Option<Route> {
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

	match parts.as_slice() {
		[broadcast @ .., file] if file == "master.m3u8" => Some(Route::Master {
			broadcast: broadcast_path(broadcast)?,
		}),
		[broadcast @ .., file] if file == "manifest.mpd" => Some(Route::Manifest {
			broadcast: broadcast_path(broadcast)?,
		}),
		[broadcast @ .., kind, rendition, file] if file == "media.m3u8" => Some(Route::Media {
			broadcast: broadcast_path(broadcast)?,
			kind: kind.clone(),
			rendition: rendition.clone(),
		}),
		[broadcast @ .., kind, rendition, file] if file == "init.mp4" => Some(Route::Init {
			broadcast: broadcast_path(broadcast)?,
			kind: kind.clone(),
			rendition: rendition.clone(),
		}),
		[broadcast @ .., kind, rendition, directory, file] if directory == "seg" => Some(Route::Segment {
			broadcast: broadcast_path(broadcast)?,
			kind: kind.clone(),
			rendition: rendition.clone(),
			file: file.clone(),
		}),
		_ => None,
	}
}

async fn request(State(server): State<Server>, uri: Uri, RawQuery(query): RawQuery) -> Response {
	match parse_route(uri.path()) {
		Some(Route::Master { broadcast }) => master(&server, &broadcast, query.as_deref()).await,
		Some(Route::Manifest { broadcast }) => manifest(&server, &broadcast, query.as_deref()).await,
		Some(Route::Media {
			broadcast,
			kind,
			rendition,
		}) => media(&server, &broadcast, &kind, &rendition, query.as_deref()).await,
		Some(Route::Init {
			broadcast,
			kind,
			rendition,
		}) => init(&server, &broadcast, &kind, &rendition).await,
		Some(Route::Segment {
			broadcast,
			kind,
			rendition,
			file,
		}) => segment(&server, &broadcast, &kind, &rendition, &file).await,
		None => not_found(),
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
	// Propagate whatever query reached the master (e.g. a credential a wrapping
	// middleware required) down to the child media-playlist URLs.
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
	match broadcaster.manifest(query) {
		Some(manifest) => mpd(manifest),
		None => not_found(),
	}
}

async fn media(server: &Server, broadcast: &str, kind: &str, rendition: &str, query: Option<&str>) -> Response {
	let Some(rendition) = rendition_for(server, broadcast, kind, rendition).await else {
		return not_found();
	};

	// A playlist with no segments confuses players; give the timeline a moment to index the
	// first complete segment before answering.
	let _ = tokio::time::timeout(READY_TIMEOUT, rendition.playable()).await;

	// The playlist references init.mp4 via EXT-X-MAP. Make sure it's actually buildable before
	// advertising it (an inline-codec init needs a keyframe group fetched first), so a player
	// never loads a map segment that 404s. init() caches, so the follow-up GET is free.
	match rendition.init().await {
		Ok(Some(_)) => {}
		Ok(None) => return not_found(),
		Err(err) => return server_error(err),
	}

	match rendition.media_playlist(query) {
		Some(playlist) => m3u8(playlist),
		None => not_found(),
	}
}

async fn init(server: &Server, broadcast: &str, kind: &str, rendition: &str) -> Response {
	let Some(rendition) = rendition_for(server, broadcast, kind, rendition).await else {
		return not_found();
	};
	match rendition.init().await {
		Ok(Some(bytes)) => media_bytes(bytes, server),
		Ok(None) => not_found(),
		Err(err) => server_error(err),
	}
}

async fn segment(server: &Server, broadcast: &str, kind: &str, rendition: &str, file: &str) -> Response {
	let Some(stem) = file.strip_suffix(".m4s") else {
		return not_found();
	};
	let Some(rendition) = rendition_for(server, broadcast, kind, rendition).await else {
		return not_found();
	};
	// HLS addresses a segment by its aligned number (`seg/0.m4s`); DASH by its timeline pts
	// (`seg/t2000.m4s`, the SegmentTemplate's `$Time$`). Same bytes either way.
	let result = match stem.strip_prefix('t') {
		Some(time) => match time.parse::<u64>() {
			Ok(time) => rendition.segment_at(time).await,
			Err(_) => return not_found(),
		},
		None => match stem.parse::<u64>() {
			Ok(sequence) => rendition.segment(sequence).await,
			Err(_) => return not_found(),
		},
	};
	match result {
		Ok(Some(bytes)) => media_bytes(bytes, server),
		Ok(None) => not_found(),
		Err(err) => server_error(err),
	}
}

/// Resolve a rendition, waiting for the catalog to populate.
async fn rendition_for(server: &Server, broadcast: &str, kind: &str, rendition: &str) -> Option<Arc<Rendition>> {
	let kind = kind.parse::<Kind>().ok()?;
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

fn media_bytes(body: Bytes, server: &Server) -> Response {
	// Init/segment bytes never change while their URL is listed, but the URL itself is not
	// globally unique: a restarted publisher starts a new timeline whose segment numbers and
	// pts can repeat the old ones with different media (and a reconfigured rendition rebuilds
	// its init under the same init.mp4). Cap shared caching at the playlist window - every
	// concurrent viewer of the live window still hits the cache, while a stale generation's
	// bytes age out as fast as the window that stopped listing them.
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
		let Some(Route::Media {
			broadcast,
			kind,
			rendition,
		}) = parse_route("/project/live/video/cam%231%2Fmain%3Falt/media.m3u8")
		else {
			panic!("media route should parse");
		};

		assert_eq!(broadcast, "project/live");
		assert_eq!(kind, "video");
		assert_eq!(rendition, "cam#1/main?alt");
	}

	#[test]
	fn parses_all_resource_routes() {
		assert!(matches!(
			parse_route("/project/live/master.m3u8"),
			Some(Route::Master { .. })
		));
		assert!(matches!(
			parse_route("/project/live/manifest.mpd"),
			Some(Route::Manifest { .. })
		));
		assert!(matches!(
			parse_route("/project/live/video/main/init.mp4"),
			Some(Route::Init { .. })
		));
		assert!(matches!(
			parse_route("/project/live/video/main/seg/42.m4s"),
			Some(Route::Segment { .. })
		));
		assert!(matches!(
			parse_route("/project/live/video/main/seg/t2000.m4s"),
			Some(Route::Segment { .. })
		));
		assert!(parse_route("/master.m3u8").is_none());
		assert!(parse_route("/project/live/video/main/unknown").is_none());
	}

	#[test]
	fn rejects_empty_path_segments() {
		assert!(parse_route("/project//live/master.m3u8").is_none());
	}

	#[test]
	fn rejects_encoded_broadcast_separators() {
		assert!(parse_route("/project/private%2F/master.m3u8").is_none());
		assert!(parse_route("/project%2Fprivate/master.m3u8").is_none());
	}
}
