//! axum handlers for the HLS / LL-HLS endpoints.

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::extract::{Path, RawQuery, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use bytes::Bytes;

use super::Server;
use crate::export::store::SegmentStore;

const M3U8: &str = "application/vnd.apple.mpegurl";
const MP4: &str = "video/mp4";

/// Playlists change at the live edge, and a blocking-reload response is specific to the
/// request that produced it, so they must never be served from a shared cache. Without
/// this a proxy is free to apply heuristic caching and pin a stale live playlist.
const PLAYLIST_CACHE: &str = "no-cache";

/// Segments and parts never change at a given URL while a broadcast is live, so they
/// cache well. Bounded rather than `immutable`: republishing a broadcast restarts its
/// sequence numbering, so a stale entry has to age out instead of pinning the previous
/// session's media at the same URL.
const MEDIA_CACHE: &str = "public, max-age=60";

/// How long a rendition lookup waits for the catalog to populate.
const READY_TIMEOUT: Duration = Duration::from_secs(5);
/// Upper bound on an LL-HLS blocking-reload / preload wait.
const BLOCK_TIMEOUT: Duration = Duration::from_secs(10);

/// An endpoint served underneath a broadcast name.
enum Endpoint {
	Master,
	Media {
		rendition: String,
	},
	Init {
		rendition: String,
	},
	Segment {
		rendition: String,
		sequence: u64,
	},
	Part {
		rendition: String,
		sequence: u64,
		index: usize,
	},
}

/// A parsed request path: which broadcast, and what under it.
struct Route {
	broadcast: String,
	endpoint: Endpoint,
}

impl Route {
	/// Split a request path into its broadcast name and endpoint.
	///
	/// Matched from the END, because a MoQ broadcast name is a hierarchical path: `room/user`
	/// is a single name spanning two components, so only the trailing components are fixed.
	/// A broadcast whose name ends in `seg` or `part` is ambiguous and loses to the endpoint;
	/// disambiguating that isn't worth an escaping scheme.
	fn parse(path: &str) -> Option<Self> {
		let parts: Vec<&str> = path.split('/').collect();
		let count = parts.len();
		// Index from the end: `at(0)` is the last component.
		let at = |i: usize| -> Option<&str> { parts.get(count.checked_sub(1 + i)?).copied() };

		let (trailing, endpoint) = match at(0)? {
			"master.m3u8" => (1, Endpoint::Master),
			"media.m3u8" => (
				2,
				Endpoint::Media {
					rendition: at(1)?.to_string(),
				},
			),
			"init.mp4" => (
				2,
				Endpoint::Init {
					rendition: at(1)?.to_string(),
				},
			),
			file => {
				let file = file.strip_suffix(".m4s")?;
				if at(1)? == "seg" {
					(
						3,
						Endpoint::Segment {
							rendition: at(2)?.to_string(),
							sequence: file.parse().ok()?,
						},
					)
				} else if at(2)? == "part" {
					(
						4,
						Endpoint::Part {
							rendition: at(3)?.to_string(),
							sequence: at(1)?.parse().ok()?,
							index: file.parse().ok()?,
						},
					)
				} else {
					return None;
				}
			}
		};

		// Whatever precedes the endpoint is the broadcast name, which must be non-empty.
		let split = count.checked_sub(trailing)?;
		if split == 0 {
			return None;
		}

		Some(Self {
			broadcast: parts[..split].join("/"),
			endpoint,
		})
	}
}

pub fn router(server: Server) -> Router {
	// A single catch-all rather than a route per endpoint: the broadcast name can span
	// any number of path components, so the shape isn't fixed. See [`Route::parse`].
	Router::new().route("/{*path}", get(dispatch)).with_state(server)
}

async fn dispatch(State(server): State<Server>, Path(path): Path<String>, RawQuery(query): RawQuery) -> Response {
	let Some(route) = Route::parse(&path) else {
		return not_found();
	};
	let broadcast = route.broadcast;

	match route.endpoint {
		Endpoint::Master => master(&server, &broadcast).await,
		Endpoint::Media { rendition } => media(&server, &broadcast, &rendition, query.as_deref()).await,
		Endpoint::Init { rendition } => init(&server, &broadcast, &rendition).await,
		Endpoint::Segment { rendition, sequence } => segment(&server, &broadcast, &rendition, sequence).await,
		Endpoint::Part {
			rendition,
			sequence,
			index,
		} => part(&server, &broadcast, &rendition, sequence, index).await,
	}
}

async fn master(server: &Server, broadcast: &str) -> Response {
	let Some(handle) = server.handle(broadcast).await else {
		return not_found();
	};
	handle.wait_ready(READY_TIMEOUT).await;
	let snapshot = handle.snapshot();
	// Don't serve an empty master (a 200 with no variants): if no rendition showed up
	// within the timeout, the broadcast isn't playable yet, so 404.
	if snapshot.is_empty() {
		return not_found();
	}
	m3u8(snapshot.master_playlist())
}

async fn media(server: &Server, broadcast: &str, rendition: &str, query: Option<&str>) -> Response {
	let msn = query_param(query, "_HLS_msn").and_then(|v| v.parse::<u64>().ok());
	let part = query_param(query, "_HLS_part").and_then(|v| v.parse::<usize>().ok());

	// `_HLS_part` only names a part *within* `_HLS_msn`, so it is meaningless alone.
	// Checked before resolving the store, which would otherwise spend the ready timeout
	// on a request we already know is malformed.
	if msn.is_none() && part.is_some() {
		return bad_request();
	}

	let Some(store) = store(server, broadcast, rendition).await else {
		return not_found();
	};

	// LL-HLS blocking reload: wait until the requested (msn, part) lands.
	if let Some(msn) = msn {
		// A blocking reload may only ask for a segment the playlist is about to produce.
		// Anything further ahead is a client bug or a scan, so answer it now instead of
		// pinning the connection for the whole block timeout.
		let version = store.version();
		if !version.finished && msn > version.last_sequence + 2 {
			return bad_request();
		}
		block_until(&store, msn, part.unwrap_or(0)).await;
	}

	let snapshot = store.snapshot();

	// Don't advertise a rendition the player can't bootstrap yet: the playlist references
	// init.mp4, which 404s until the first (init) fragment lands, and carries nothing to
	// play until a part exists.
	if !snapshot.init_ready || snapshot.segments.is_empty() {
		return not_found();
	}

	m3u8(crate::export::render_media(&snapshot))
}

async fn init(server: &Server, broadcast: &str, rendition: &str) -> Response {
	let Some(store) = store(server, broadcast, rendition).await else {
		return not_found();
	};
	match store.init() {
		Some(bytes) => media_bytes(bytes),
		None => not_found(),
	}
}

async fn segment(server: &Server, broadcast: &str, rendition: &str, sequence: u64) -> Response {
	let Some(store) = store(server, broadcast, rendition).await else {
		return not_found();
	};
	match store.segment(sequence) {
		Some(bytes) => media_bytes(bytes),
		None => not_found(),
	}
}

async fn part(server: &Server, broadcast: &str, rendition: &str, sequence: u64, index: usize) -> Response {
	let Some(store) = store(server, broadcast, rendition).await else {
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
async fn store(server: &Server, broadcast: &str, rendition: &str) -> Option<Arc<SegmentStore>> {
	let handle = server.handle(broadcast).await?;
	handle.wait_ready(READY_TIMEOUT).await;
	handle.rendition(rendition).map(|r| r.store.clone())
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

fn m3u8(body: String) -> Response {
	(
		[(header::CONTENT_TYPE, M3U8), (header::CACHE_CONTROL, PLAYLIST_CACHE)],
		body,
	)
		.into_response()
}

fn media_bytes(body: Bytes) -> Response {
	(
		[(header::CONTENT_TYPE, MP4), (header::CACHE_CONTROL, MEDIA_CACHE)],
		body,
	)
		.into_response()
}

fn not_found() -> Response {
	StatusCode::NOT_FOUND.into_response()
}

fn bad_request() -> Response {
	StatusCode::BAD_REQUEST.into_response()
}

#[cfg(test)]
mod tests {
	use axum::body::Body;
	use axum::http::Request;
	use tower::ServiceExt as _;

	use super::*;

	/// `Route::parse` matches the endpoint from the end of the path, which assumes axum
	/// hands the wildcard capture over WITHOUT a leading slash. Every route silently 404s
	/// if that ever changes, so pin it.
	#[tokio::test]
	async fn wildcard_capture_has_no_leading_slash() {
		async fn echo(Path(path): Path<String>) -> String {
			path
		}

		let response = Router::new()
			.route("/{*path}", get(echo))
			.oneshot(
				Request::builder()
					.uri("/live/video/media.m3u8")
					.body(Body::empty())
					.unwrap(),
			)
			.await
			.unwrap();

		let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
		assert_eq!(&body[..], b"live/video/media.m3u8");
	}

	fn parse(path: &str) -> (String, Endpoint) {
		let route = Route::parse(path).expect("route should parse");
		(route.broadcast, route.endpoint)
	}

	#[test]
	fn parses_each_endpoint() {
		assert!(matches!(parse("live/master.m3u8"), (b, Endpoint::Master) if b == "live"));
		assert!(matches!(parse("live/video/media.m3u8"), (_, Endpoint::Media { rendition }) if rendition == "video"));
		assert!(matches!(parse("live/video/init.mp4"), (_, Endpoint::Init { rendition }) if rendition == "video"));
		assert!(
			matches!(parse("live/video/seg/7.m4s"), (_, Endpoint::Segment { rendition, sequence }) if rendition == "video" && sequence == 7)
		);
		assert!(
			matches!(parse("live/video/part/7/2.m4s"), (_, Endpoint::Part { rendition, sequence, index }) if rendition == "video" && sequence == 7 && index == 2)
		);
	}

	/// MoQ broadcast names are hierarchical paths, so a name spanning several components
	/// must still route: the endpoint is matched from the end, not the start.
	#[test]
	fn parses_hierarchical_broadcast_name() {
		assert_eq!(parse("room/user/master.m3u8").0, "room/user");
		assert_eq!(parse("a/b/c/video/media.m3u8").0, "a/b/c");
		assert_eq!(parse("room/user/video/seg/3.m4s").0, "room/user");
		assert_eq!(parse("room/user/video/part/3/1.m4s").0, "room/user");
	}

	#[test]
	fn rejects_unknown_or_incomplete_paths() {
		// No broadcast name.
		assert!(Route::parse("master.m3u8").is_none());
		assert!(Route::parse("video/media.m3u8").is_none());
		// Unknown endpoints.
		assert!(Route::parse("live/video/index.html").is_none());
		assert!(Route::parse("live/video/blah/7.m4s").is_none());
		// Non-numeric sequences.
		assert!(Route::parse("live/video/seg/abc.m4s").is_none());
		assert!(Route::parse("live/video/part/x/1.m4s").is_none());
	}

	#[test]
	fn finds_query_params() {
		assert_eq!(query_param(Some("_HLS_msn=4&_HLS_part=1"), "_HLS_msn"), Some("4"));
		assert_eq!(query_param(Some("_HLS_msn=4&_HLS_part=1"), "_HLS_part"), Some("1"));
		assert_eq!(query_param(Some("_HLS_msn=4"), "_HLS_part"), None);
		assert_eq!(query_param(None, "_HLS_msn"), None);
	}
}
