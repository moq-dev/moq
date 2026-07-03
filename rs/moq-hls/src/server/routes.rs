//! axum handlers for the HLS / LL-HLS endpoints.

use std::time::Duration;

use axum::Router;
use axum::extract::{Path, RawQuery, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use bytes::Bytes;

use super::Server;
use crate::export::store::SegmentStore;

const M3U8: &str = "application/vnd.apple.mpegurl";
const MP4: &str = "video/mp4";

/// How long a rendition lookup waits for the catalog to populate.
const READY_TIMEOUT: Duration = Duration::from_secs(5);
/// Upper bound on an LL-HLS blocking-reload / preload wait.
const BLOCK_TIMEOUT: Duration = Duration::from_secs(10);

pub fn router(server: Server) -> Router {
	Router::new()
		.route("/{broadcast}/master.m3u8", get(master))
		.route("/{broadcast}/{rendition}/media.m3u8", get(media))
		.route("/{broadcast}/{rendition}/init.mp4", get(init))
		.route("/{broadcast}/{rendition}/seg/{file}", get(segment))
		.route("/{broadcast}/{rendition}/part/{seq}/{file}", get(part))
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
	Path((broadcast, rendition)): Path<(String, String)>,
	RawQuery(query): RawQuery,
	headers: HeaderMap,
) -> Response {
	if let Err(status) = server.authorize(&broadcast, &headers, query.as_deref()) {
		return status.into_response();
	}
	let Some(store) = store(&server, &broadcast, &rendition).await else {
		return not_found();
	};

	// LL-HLS blocking reload: wait until the requested (msn, part) lands.
	if let Some(msn) = query_param(query.as_deref(), "_HLS_msn").and_then(|v| v.parse::<u64>().ok()) {
		let part = query_param(query.as_deref(), "_HLS_part").and_then(|v| v.parse::<usize>().ok());

		// RFC 8216bis: a request more than the last media sequence + 2 in the future
		// (or, when a part is given, beyond the last part of that segment + the
		// PART-TARGET headroom of one extra part) is a spec 400, not something to
		// block on for the full timeout.
		let version = store.version();
		if !version.finished {
			let too_far = if msn > version.last_sequence.saturating_add(2) {
				true
			} else if msn == version.last_sequence.saturating_add(1) {
				// A part index on the not-yet-started next segment may only reach one
				// past its expected first part.
				part.is_some_and(|part| part > 0)
			} else if msn == version.last_sequence {
				part.is_some_and(|part| part > version.last_parts)
			} else {
				false
			};
			if too_far {
				return bad_request();
			}
		}

		block_until(&store, msn, part.unwrap_or(0)).await;
	}

	let snapshot = store.snapshot();

	// Don't advertise a rendition the player can't bootstrap yet: the playlist
	// references init.mp4, which 404s until the first (init) fragment lands.
	if !snapshot.init_ready {
		return not_found();
	}

	m3u8(crate::export::render_media(&snapshot))
}

async fn init(
	State(server): State<Server>,
	Path((broadcast, rendition)): Path<(String, String)>,
	RawQuery(query): RawQuery,
	headers: HeaderMap,
) -> Response {
	if let Err(status) = server.authorize(&broadcast, &headers, query.as_deref()) {
		return status.into_response();
	}
	let Some(store) = store(&server, &broadcast, &rendition).await else {
		return not_found();
	};
	match store.init() {
		Some(bytes) => media_bytes(bytes),
		None => not_found(),
	}
}

async fn segment(
	State(server): State<Server>,
	Path((broadcast, rendition, file)): Path<(String, String, String)>,
	RawQuery(query): RawQuery,
	headers: HeaderMap,
) -> Response {
	if let Err(status) = server.authorize(&broadcast, &headers, query.as_deref()) {
		return status.into_response();
	}
	let Some(sequence) = strip_m4s(&file).and_then(|s| s.parse::<u64>().ok()) else {
		return not_found();
	};
	let Some(store) = store(&server, &broadcast, &rendition).await else {
		return not_found();
	};
	match store.segment(sequence) {
		Some(bytes) => media_bytes(bytes),
		None => not_found(),
	}
}

async fn part(
	State(server): State<Server>,
	Path((broadcast, rendition, sequence, file)): Path<(String, String, u64, String)>,
	RawQuery(query): RawQuery,
	headers: HeaderMap,
) -> Response {
	if let Err(status) = server.authorize(&broadcast, &headers, query.as_deref()) {
		return status.into_response();
	}
	let Some(index) = strip_m4s(&file).and_then(|s| s.parse::<usize>().ok()) else {
		return not_found();
	};
	let Some(store) = store(&server, &broadcast, &rendition).await else {
		return not_found();
	};

	// A legit preload-hint part is at most one sequence past the current last segment.
	// Reject anything further ahead immediately rather than holding the connection for
	// the full block timeout on a bogus/scanning request.
	let version = store.version();
	if !version.finished && sequence > version.last_sequence + 1 {
		return not_found();
	}

	// The part may be a preload hint that hasn't been produced yet; block briefly.
	block_until(&store, sequence, index).await;

	match store.part(sequence, index) {
		Some(bytes) => media_bytes(bytes),
		None => not_found(),
	}
}

/// Resolve a rendition's store, waiting for the catalog to populate.
async fn store(server: &Server, broadcast: &str, rendition: &str) -> Option<std::sync::Arc<SegmentStore>> {
	let broadcaster = server.broadcaster(broadcast).await?;
	broadcaster.wait_ready(READY_TIMEOUT).await;
	broadcaster.rendition(rendition).map(|r| r.store.clone())
}

/// Block until the store holds `(msn, part)`, the window passed it, or the track
/// ended; bounded by [`BLOCK_TIMEOUT`].
async fn block_until(store: &SegmentStore, msn: u64, part: usize) {
	if store.satisfies(msn, part) {
		return;
	}
	let mut rx = store.subscribe();
	let _ = tokio::time::timeout(BLOCK_TIMEOUT, async {
		loop {
			if store.satisfies(msn, part) {
				break;
			}
			if rx.changed().await.is_err() {
				break;
			}
		}
	})
	.await;
}

/// Find a query parameter value in a raw `a=b&c=d` query string.
fn query_param<'a>(query: Option<&'a str>, key: &str) -> Option<&'a str> {
	query?.split('&').find_map(|pair| {
		let (k, v) = pair.split_once('=')?;
		(k == key).then_some(v)
	})
}

fn strip_m4s(file: &str) -> Option<&str> {
	file.strip_suffix(".m4s")
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
	// Init/segment/part bytes are content-addressed and immutable once produced.
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

fn bad_request() -> Response {
	(StatusCode::BAD_REQUEST, [(header::CACHE_CONTROL, "no-store")]).into_response()
}
