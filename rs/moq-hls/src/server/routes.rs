//! axum handlers for the HLS / LL-HLS endpoints.
//!
//! One catch-all route (`/{*rest}`) so a broadcast name may contain slashes; the
//! recognized trailing shape (`index.m3u8`, `<r>/media.m3u8`, `<r>/init.mp4`,
//! `<r>/seg/<n>.m4s`, `<r>/part/<n>/<i>.m4s`) decides the request and what's left
//! is the broadcast. Each request is authorized (the `?jwt=` token), and that
//! token is threaded into the playlists so a player carries it onto sub-requests.

use std::time::Duration;

use axum::Router;
use axum::extract::{Path, RawQuery, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use bytes::Bytes;

use super::{AuthRejected, Server};
use crate::export::store::SegmentStore;

const M3U8: &str = "application/vnd.apple.mpegurl";
const MP4: &str = "video/mp4";

/// How long a rendition lookup waits for the catalog to populate.
const READY_TIMEOUT: Duration = Duration::from_secs(5);
/// Upper bound on an LL-HLS blocking-reload / preload wait.
const BLOCK_TIMEOUT: Duration = Duration::from_secs(10);

pub fn router(server: Server) -> Router {
	Router::new().route("/{*rest}", get(handle)).with_state(server)
}

/// A parsed HLS request: which resource, and the broadcast it belongs to.
#[derive(Debug, PartialEq, Eq)]
enum Req {
	Master {
		broadcast: String,
	},
	Media {
		broadcast: String,
		rendition: String,
	},
	Init {
		broadcast: String,
		rendition: String,
	},
	Segment {
		broadcast: String,
		rendition: String,
		sequence: u64,
	},
	Part {
		broadcast: String,
		rendition: String,
		sequence: u64,
		index: usize,
	},
}

impl Req {
	fn broadcast(&self) -> &str {
		match self {
			Req::Master { broadcast }
			| Req::Media { broadcast, .. }
			| Req::Init { broadcast, .. }
			| Req::Segment { broadcast, .. }
			| Req::Part { broadcast, .. } => broadcast,
		}
	}
}

/// Parse the catch-all path tail into a request. The broadcast is everything
/// before the recognized suffix, so it may contain slashes.
fn parse(rest: &str) -> Option<Req> {
	let parts: Vec<&str> = rest.split('/').filter(|s| !s.is_empty()).collect();
	let n = parts.len();
	// Every shape is at least `<broadcast>/<file>`.
	if n < 2 {
		return None;
	}
	let last = parts[n - 1];

	// `<broadcast..>/index.m3u8` (alias `master.m3u8`).
	if last == "index.m3u8" || last == "master.m3u8" {
		return Some(Req::Master {
			broadcast: parts[..n - 1].join("/"),
		});
	}
	// `<broadcast..>/<rendition>/media.m3u8`
	if last == "media.m3u8" && n >= 3 {
		return Some(Req::Media {
			broadcast: parts[..n - 2].join("/"),
			rendition: parts[n - 2].to_string(),
		});
	}
	// `<broadcast..>/<rendition>/init.mp4`
	if last == "init.mp4" && n >= 3 {
		return Some(Req::Init {
			broadcast: parts[..n - 2].join("/"),
			rendition: parts[n - 2].to_string(),
		});
	}
	// `<broadcast..>/<rendition>/part/<seq>/<idx>.m4s`
	if n >= 5 && parts[n - 3] == "part" {
		return Some(Req::Part {
			broadcast: parts[..n - 4].join("/"),
			rendition: parts[n - 4].to_string(),
			sequence: parts[n - 2].parse().ok()?,
			index: strip_m4s(last)?.parse().ok()?,
		});
	}
	// `<broadcast..>/<rendition>/seg/<seq>.m4s`
	if n >= 4 && parts[n - 2] == "seg" {
		return Some(Req::Segment {
			broadcast: parts[..n - 3].join("/"),
			rendition: parts[n - 3].to_string(),
			sequence: strip_m4s(last)?.parse().ok()?,
		});
	}
	None
}

async fn handle(State(server): State<Server>, Path(rest): Path<String>, RawQuery(query): RawQuery) -> Response {
	let Some(req) = parse(&rest) else {
		return not_found();
	};

	// Authorize the (broadcast, token) before touching any media.
	let token = query_param(query.as_deref(), "jwt");
	if let Err(rejected) = server.authorize(req.broadcast(), token).await {
		return reject(rejected);
	}

	// The token (only the token, not the LL-HLS `_HLS_*` params) is threaded into
	// the playlists so the player carries it onto media / segment / part requests.
	let auth_query = token.map(|t| format!("jwt={t}"));

	match req {
		Req::Master { broadcast } => {
			let Some(broadcaster) = server.broadcaster(&broadcast).await else {
				return not_found();
			};
			broadcaster.wait_ready(READY_TIMEOUT).await;
			m3u8(broadcaster.master_playlist(auth_query.as_deref()))
		}
		Req::Media { broadcast, rendition } => {
			let Some(store) = store(&server, &broadcast, &rendition).await else {
				return not_found();
			};
			// LL-HLS blocking reload: wait until the requested (msn, part) lands.
			if let Some(msn) = query_param(query.as_deref(), "_HLS_msn").and_then(|v| v.parse::<u64>().ok()) {
				let part = query_param(query.as_deref(), "_HLS_part")
					.and_then(|v| v.parse::<usize>().ok())
					.unwrap_or(0);
				block_until(&store, msn, part).await;
			}
			m3u8(crate::export::render_media(&store.snapshot(), auth_query.as_deref()))
		}
		Req::Init { broadcast, rendition } => match store(&server, &broadcast, &rendition).await {
			Some(store) => match store.init() {
				Some(bytes) => media_bytes(bytes),
				None => not_found(),
			},
			None => not_found(),
		},
		Req::Segment {
			broadcast,
			rendition,
			sequence,
		} => match store(&server, &broadcast, &rendition).await {
			Some(store) => match store.segment(sequence) {
				Some(bytes) => media_bytes(bytes),
				None => not_found(),
			},
			None => not_found(),
		},
		Req::Part {
			broadcast,
			rendition,
			sequence,
			index,
		} => {
			let Some(store) = store(&server, &broadcast, &rendition).await else {
				return not_found();
			};
			// The part may be a preload hint that hasn't been produced yet; block briefly.
			block_until(&store, sequence, index).await;
			match store.part(sequence, index) {
				Some(bytes) => media_bytes(bytes),
				None => not_found(),
			}
		}
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
	([(header::CONTENT_TYPE, M3U8)], body).into_response()
}

fn media_bytes(body: Bytes) -> Response {
	([(header::CONTENT_TYPE, MP4)], body).into_response()
}

fn reject(rejected: AuthRejected) -> Response {
	match rejected {
		AuthRejected::Unauthorized(msg) => (StatusCode::UNAUTHORIZED, msg).into_response(),
		AuthRejected::Forbidden(msg) => (StatusCode::FORBIDDEN, msg).into_response(),
	}
}

fn not_found() -> Response {
	StatusCode::NOT_FOUND.into_response()
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn parses_index_and_master_alias() {
		assert_eq!(
			parse("bbb.hang/index.m3u8"),
			Some(Req::Master {
				broadcast: "bbb.hang".into()
			})
		);
		assert_eq!(
			parse("bbb.hang/master.m3u8"),
			Some(Req::Master {
				broadcast: "bbb.hang".into()
			})
		);
	}

	#[test]
	fn parses_multi_segment_broadcast() {
		assert_eq!(
			parse("cam/lobby.hang/video/media.m3u8"),
			Some(Req::Media {
				broadcast: "cam/lobby.hang".into(),
				rendition: "video".into(),
			})
		);
		assert_eq!(
			parse("cam/lobby.hang/index.m3u8"),
			Some(Req::Master {
				broadcast: "cam/lobby.hang".into()
			})
		);
	}

	#[test]
	fn parses_init_segment_and_part() {
		assert_eq!(
			parse("b/audio/init.mp4"),
			Some(Req::Init {
				broadcast: "b".into(),
				rendition: "audio".into(),
			})
		);
		assert_eq!(
			parse("b/video/seg/7.m4s"),
			Some(Req::Segment {
				broadcast: "b".into(),
				rendition: "video".into(),
				sequence: 7,
			})
		);
		assert_eq!(
			parse("a/b/video/part/7/2.m4s"),
			Some(Req::Part {
				broadcast: "a/b".into(),
				rendition: "video".into(),
				sequence: 7,
				index: 2,
			})
		);
	}

	#[test]
	fn rejects_junk() {
		assert_eq!(parse("bbb.hang"), None);
		assert_eq!(parse(""), None);
		assert_eq!(parse("b/video/seg/notanumber.m4s"), None);
	}
}
