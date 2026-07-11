//! axum handlers for the HLS endpoints.

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::extract::{Path, RawQuery, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use bytes::Bytes;

use super::Server;
use crate::export::{Kind, Rendition};

const M3U8: &str = "application/vnd.apple.mpegurl";
const MP4: &str = "video/mp4";

/// How long a rendition lookup waits for the catalog (and its first timeline records) to
/// populate.
const READY_TIMEOUT: Duration = Duration::from_secs(5);

pub fn router(server: Server) -> Router {
	Router::new()
		.route("/{broadcast}/master.m3u8", get(master))
		.route("/{broadcast}/{kind}/{rendition}/media.m3u8", get(media))
		.route("/{broadcast}/{kind}/{rendition}/init.mp4", get(init))
		.route("/{broadcast}/{kind}/{rendition}/seg/{file}", get(segment))
		.with_state(server)
}

async fn master(State(server): State<Server>, Path(broadcast): Path<String>, headers: HeaderMap) -> Response {
	if let Err(status) = server.authorize(&broadcast, &headers, None) {
		return status.into_response();
	}
	let Some(broadcaster) = server.broadcaster(&broadcast).await else {
		return not_found();
	};
	broadcaster.wait_ready(READY_TIMEOUT).await;
	m3u8(broadcaster.master_playlist())
}

async fn media(
	State(server): State<Server>,
	Path((broadcast, kind, rendition)): Path<(String, String, String)>,
	RawQuery(query): RawQuery,
	headers: HeaderMap,
) -> Response {
	if let Err(status) = server.authorize(&broadcast, &headers, query.as_deref()) {
		return status.into_response();
	}
	let Some(rendition) = rendition_for(&server, &broadcast, &kind, &rendition).await else {
		return not_found();
	};

	// A playlist with no segments confuses players; give the timeline a moment to index the
	// first complete segment before answering.
	wait_playable(&rendition).await;
	if !rendition.playable() {
		return not_found();
	}

	// The playlist references init.mp4 via EXT-X-MAP. Make sure it's actually buildable before
	// advertising it (an inline-codec init needs a keyframe group fetched first), so a player
	// never loads a map segment that 404s. init() caches, so the follow-up GET is free.
	match rendition.init().await {
		Ok(Some(_)) => {}
		Ok(None) => return not_found(),
		Err(err) => return server_error(err),
	}

	m3u8(crate::export::render_media(&rendition.playlist()))
}

async fn init(
	State(server): State<Server>,
	Path((broadcast, kind, rendition)): Path<(String, String, String)>,
	RawQuery(query): RawQuery,
	headers: HeaderMap,
) -> Response {
	if let Err(status) = server.authorize(&broadcast, &headers, query.as_deref()) {
		return status.into_response();
	}
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
	RawQuery(query): RawQuery,
	headers: HeaderMap,
) -> Response {
	if let Err(status) = server.authorize(&broadcast, &headers, query.as_deref()) {
		return status.into_response();
	}
	let Some(sequence) = file.strip_suffix(".m4s").and_then(|s| s.parse::<u64>().ok()) else {
		return not_found();
	};
	let Some(rendition) = rendition_for(&server, &broadcast, &kind, &rendition).await else {
		return not_found();
	};
	match rendition.segment(sequence).await {
		Ok(Some(bytes)) => media_bytes(bytes),
		Ok(None) => not_found(),
		Err(err) => server_error(err),
	}
}

/// Resolve a rendition, waiting for the catalog to populate.
async fn rendition_for(server: &Server, broadcast: &str, kind: &str, rendition: &str) -> Option<Arc<Rendition>> {
	let kind = kind.parse::<Kind>().ok()?;
	let broadcaster = server.broadcaster(broadcast).await?;
	broadcaster.wait_ready(READY_TIMEOUT).await;
	broadcaster.rendition(kind, rendition)
}

/// Wait until the rendition has at least one complete segment (or ended), bounded by
/// [`READY_TIMEOUT`].
async fn wait_playable(rendition: &Rendition) {
	if rendition.playable() {
		return;
	}
	let mut rx = rendition.updated();
	let _ = tokio::time::timeout(READY_TIMEOUT, async {
		loop {
			if rendition.playable() {
				break;
			}
			if rx.changed().await.is_err() {
				break;
			}
		}
	})
	.await;
}

fn m3u8(body: String) -> Response {
	// Playlists mutate as the live edge advances, so they must not be cached.
	(
		[(header::CONTENT_TYPE, M3U8), (header::CACHE_CONTROL, "no-cache")],
		body,
	)
		.into_response()
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
