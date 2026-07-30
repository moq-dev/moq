//! axum handlers for the HLS endpoints.

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::extract::{Path, RawQuery, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use bytes::Bytes;

use super::Server;
use crate::export::{Kind, Rendition};

const M3U8: &str = "application/vnd.apple.mpegurl";
const MPD: &str = "application/dash+xml";
const MP4: &str = "video/mp4";

/// How long a rendition lookup waits for the catalog (and its first timeline records) to
/// populate.
const READY_TIMEOUT: Duration = Duration::from_secs(5);

pub fn router(server: Server) -> Router {
	Router::new()
		.route("/{broadcast}/master.m3u8", get(master))
		.route("/{broadcast}/manifest.mpd", get(manifest))
		.route("/{broadcast}/{kind}/{rendition}/media.m3u8", get(media))
		.route("/{broadcast}/{kind}/{rendition}/init.mp4", get(init))
		.route("/{broadcast}/{kind}/{rendition}/seg/{file}", get(segment))
		.with_state(server)
}

async fn master(State(server): State<Server>, Path(broadcast): Path<String>, RawQuery(query): RawQuery) -> Response {
	let Some(broadcaster) = server.broadcaster(&broadcast).await else {
		return not_found();
	};
	let _ = tokio::time::timeout(READY_TIMEOUT, broadcaster.ready()).await;
	if broadcaster.is_empty() {
		return not_found();
	}
	// Propagate whatever query reached the master (e.g. a credential a wrapping
	// middleware required) down to the child media-playlist URLs.
	m3u8(broadcaster.master_playlist(query.as_deref()))
}

async fn manifest(State(server): State<Server>, Path(broadcast): Path<String>, RawQuery(query): RawQuery) -> Response {
	let Some(broadcaster) = server.broadcaster(&broadcast).await else {
		return not_found();
	};
	let _ = tokio::time::timeout(READY_TIMEOUT, broadcaster.ready()).await;
	if broadcaster.is_empty() {
		return not_found();
	}
	// A manifest whose timelines are all empty confuses players; give the broadcast a moment
	// to index its first complete segment before answering.
	let _ = tokio::time::timeout(READY_TIMEOUT, broadcaster.playable()).await;
	match broadcaster.manifest(query.as_deref()) {
		Some(manifest) => mpd(manifest),
		None => not_found(),
	}
}

async fn media(
	State(server): State<Server>,
	Path((broadcast, kind, rendition)): Path<(String, String, String)>,
	RawQuery(query): RawQuery,
) -> Response {
	let Some(rendition) = rendition_for(&server, &broadcast, &kind, &rendition).await else {
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

	match rendition.media_playlist(query.as_deref()) {
		Some(playlist) => m3u8(playlist),
		None => not_found(),
	}
}

async fn init(
	State(server): State<Server>,
	Path((broadcast, kind, rendition)): Path<(String, String, String)>,
) -> Response {
	let Some(rendition) = rendition_for(&server, &broadcast, &kind, &rendition).await else {
		return not_found();
	};
	match rendition.init().await {
		Ok(Some(bytes)) => media_bytes(bytes),
		Ok(None) => not_found(),
		Err(err) => server_error(err),
	}
}

async fn segment(
	State(server): State<Server>,
	Path((broadcast, kind, rendition, file)): Path<(String, String, String, String)>,
) -> Response {
	let Some(stem) = file.strip_suffix(".m4s") else {
		return not_found();
	};
	let Some(rendition) = rendition_for(&server, &broadcast, &kind, &rendition).await else {
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
		Ok(Some(bytes)) => media_bytes(bytes),
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

fn media_bytes(body: Bytes) -> Response {
	// Init/segment bytes are content-addressed and immutable once produced.
	(
		[
			(header::CONTENT_TYPE, MP4),
			(header::CACHE_CONTROL, "public, max-age=31536000, immutable"),
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
