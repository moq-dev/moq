//! `server publish`: WHIP server (RFC 9725).
//!
//! `POST /<broadcast-path>` accepts an SDP offer (`Content-Type: application/sdp`)
//! and returns an SDP answer. The request path becomes the broadcast name on
//! the upstream publish origin.

#[cfg(feature = "server")]
use axum::{
	Router,
	body::Bytes,
	extract::{OriginalUri, Path, State},
	http::{HeaderMap, HeaderValue, StatusCode, header},
	response::{IntoResponse, Response as HttpResponse},
	routing::post,
};
use str0m::{Candidate, RtcConfig};

use crate::{Error, Result, ingest::IngestSink, sdp, server::Server, session};

pub use crate::server::Response;

#[cfg(feature = "server")]
#[derive(Clone)]
struct RouterState {
	server: Server,
	publisher: moq_net::origin::Producer,
}

/// Build the WHIP axum router. Only with the `server` feature.
#[cfg(feature = "server")]
pub(crate) fn router(server: Server, publisher: moq_net::origin::Producer) -> Router {
	Router::new()
		.route("/{*path}", post(handle).delete(delete))
		.with_state(RouterState { server, publisher })
}

#[cfg(feature = "server")]
async fn handle(
	State(state): State<RouterState>,
	Path(path): Path<String>,
	OriginalUri(uri): OriginalUri,
	headers: HeaderMap,
	body: Bytes,
) -> HttpResponse {
	match accept_offer(&state.server, &state.publisher, &path, &headers, body).await {
		Ok(response) => {
			let Response {
				resource_id,
				answer,
				session,
				..
			} = response;
			let mut response_headers = HeaderMap::new();
			response_headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/sdp"));
			if let Some(loc) = crate::server::session_location(&uri, &resource_id) {
				response_headers.insert(header::LOCATION, loc);
			}
			tokio::spawn(async move {
				let _ = session.run().await;
			});
			(StatusCode::CREATED, response_headers, answer).into_response()
		}
		Err(err) => {
			tracing::warn!(%err, "whip request failed");
			(status_for(&err), err.to_string()).into_response()
		}
	}
}

/// Router glue: enforce the WHIP `Content-Type` then hand the raw offer to
/// [`accept`], using the request path as the (unauthenticated) broadcast name.
#[cfg(feature = "server")]
async fn accept_offer(
	server: &Server,
	publisher: &moq_net::origin::Producer,
	path: &str,
	headers: &HeaderMap,
	body: Bytes,
) -> Result<Response> {
	if !is_sdp(headers) {
		return Err(Error::InvalidSdp("expected Content-Type: application/sdp".into()));
	}
	let offer = std::str::from_utf8(&body).map_err(|err| Error::InvalidSdp(err.to_string()))?;
	accept(
		server,
		publisher,
		moq_net::Path::new(path).map_err(moq_net::Error::from)?,
		offer,
	)
	.await
}

#[cfg(feature = "server")]
async fn delete(State(state): State<RouterState>, Path(path): Path<String>) -> StatusCode {
	crate::server::delete(&state.server, &path)
}

/// Accept a WHIP SDP offer and publish the negotiated WebRTC media into
/// `publisher` under `broadcast` (a path relative to that producer's root).
///
/// This is the negotiation core behind [`Server::publish_router`], exposed so an embedder can
/// own the HTTP route and authentication: verify the request, resolve the
/// authorized broadcast name, scope `publisher` to the caller's grants, then hand
/// the raw SDP offer here. Taking the producer explicitly (rather than using the
/// server's) lets the embedder publish through a *scoped* origin, so the publish
/// scope is enforced by moq-net exactly as for a native session; the bundled
/// [`Server::publish_router`] passes the server's own (unauthenticated) producer. It parses the
/// offer, registers a media session on the shared mux, announces the
/// broadcast under a fresh epoch once negotiation succeeds (so a fast subscriber
/// doesn't 404 in the gap before the first RTP packet, and a reconnecting
/// encoder replaces a stale session's broadcast at once), and returns the SDP answer plus an opaque `resource_id` for the WHIP
/// `Location` header. The caller must run the returned [`Response`] to drive the
/// RTP->MoQ session.
///
/// `offer` is the raw SDP body; the caller is responsible for checking the
/// `Content-Type: application/sdp` request header. Fails with
/// [`Error::InvalidSdp`] on a malformed offer and surfaces
/// [`moq_net::Error::Unauthorized`] if `broadcast` is
/// outside `publisher`'s scope.
pub async fn accept(
	server: &Server,
	publisher: &moq_net::origin::Producer,
	broadcast: impl Into<moq_net::Path>,
	offer: &str,
) -> Result<Response> {
	let offer = sdp::parse_offer(offer)?;
	let broadcast: moq_net::Path = broadcast.into();

	// Create the broadcast hidden up front, so an out-of-scope offer is refused before
	// any mux work. It announces only once negotiation succeeds (below).
	let producer = publisher.create_broadcast(&broadcast)?;
	let handle = producer.clone();
	let config = moq_mux::catalog::Config::default()
		.with_max_age(server.config().max_age)
		.with_bandwidth(server.config().bandwidth.clone());
	let sink = Box::new(IngestSink::new(producer.broadcast().clone(), config)?);

	// Register a session on the shared media mux: known ICE credentials (so the
	// demux routes this peer's STUN by ufrag), an inbox to read datagrams from,
	// and a guard the session task holds for its lifetime (unregisters on exit).
	let mux = server.mux().await?;
	let (creds, inbound, registration) = mux.register();
	let mut rtc = RtcConfig::new()
		.set_local_ice_credentials(creds)
		.build(moq_clock::now().into_std());
	for addr in mux.candidates() {
		let cand = Candidate::host(*addr, "udp").map_err(Error::rtc)?;
		rtc.add_local_candidate(cand);
	}

	let answer = rtc.sdp_api().accept_offer(offer).map_err(Error::rtc)?;

	// Announce only once negotiation succeeded, so a failed offer leaves a live
	// session's broadcast alone, but before returning the answer, so a fast
	// subscriber doesn't see a 404 in the gap before the first RTP packet. Each
	// offer is its own publisher instance, so a reconnect replaces a stale
	// session's broadcast instead of resuming into it.
	let epoch = moq_net::Epoch::mint();
	handle.announce(moq_net::origin::Route::default().with_epoch(epoch.clone()))?;

	let resource_id = sdp::new_resource_id();
	let socket = moq_ice::Socket::new(mux.socket(), mux.candidates().to_vec(), inbound)?;
	let session = session::Session::ingest(rtc, socket, sink)?;

	// Register before returning so a DELETE that races the first packet still
	// finds the session; Response::run unregisters itself when it ends.
	let cancel = server.register_session(resource_id.clone());
	tracing::info!(%broadcast, %epoch, "whip publish accepted");

	Ok(Response {
		resource_id: resource_id.clone(),
		answer: sdp::render_answer(&answer),
		session: crate::server::AcceptedSession {
			server: server.clone(),
			resource_id,
			session: Some(session),
			registration: Some(registration),
			cancel: Some(cancel),
			role: "whip server",
		},
	})
}

#[cfg(feature = "server")]
fn is_sdp(headers: &HeaderMap) -> bool {
	headers
		.get(header::CONTENT_TYPE)
		.and_then(|v| v.to_str().ok())
		.map(|v| v.eq_ignore_ascii_case("application/sdp"))
		.unwrap_or(false)
}

#[cfg(feature = "server")]
fn status_for(err: &Error) -> StatusCode {
	match err {
		Error::InvalidSdp(_) | Error::Moq(moq_net::Error::EmptySegment) => StatusCode::BAD_REQUEST,
		Error::UnsupportedCodec(_) => StatusCode::UNSUPPORTED_MEDIA_TYPE,
		Error::SessionNotFound => StatusCode::NOT_FOUND,
		Error::Moq(moq_net::Error::Unauthorized) => StatusCode::UNAUTHORIZED,
		_ => StatusCode::INTERNAL_SERVER_ERROR,
	}
}

#[cfg(test)]
#[expect(clippy::disallowed_methods, reason = "tests read the host clock directly")]
mod tests {
	use str0m::{
		format::Codec,
		media::{Direction, MediaKind},
	};

	use super::*;

	/// An encoder's offer to send one Opus track.
	fn offer() -> String {
		let mut rtc = session::rtc_with_codecs(&[Codec::Opus]);
		let mut api = rtc.sdp_api();
		api.add_media(MediaKind::Audio, Direction::SendOnly, None, None, None);
		let (offer, _pending) = api.apply().expect("an offer");
		offer.to_sdp_string()
	}

	/// A session that ends without a DELETE, here one that never connects, ends its tracks
	/// with the transport cause, so a reader sees the publisher go rather than a producer
	/// dropped unfinished.
	#[tokio::test(start_paused = true)]
	async fn a_session_ending_without_a_delete_ends_its_tracks() {
		let server = Server::new(crate::server::Config::default());
		let origin = moq_tokio::origin::spawn();
		let consumer = origin.consume();
		let session = accept(&server, &origin, moq_net::Path::from_static("live/cam0"), &offer())
			.await
			.unwrap();
		let broadcast = consumer.request("live/cam0", None).await.unwrap();
		let mut catalog = broadcast
			.track(hang::Catalog::DEFAULT_NAME)
			.unwrap()
			.subscribe(None)
			.await
			.unwrap();

		let ended = session.run().await;
		assert!(matches!(ended, Err(crate::Error::IceTimeout)), "{ended:?}");
		let end = loop {
			match catalog.recv_group().await {
				Ok(Some(_)) => continue,
				end => break end.map(|_| ()),
			}
		};
		assert!(matches!(end, Err(moq_net::Error::Transport(_))), "{end:?}");
	}

	/// An encoder reconnecting to the same broadcast while its stale session is still
	/// open restarts the broadcast at once: each offer is its own epoch, so a fresh
	/// request reaches the reconnect, while the stale viewer stays on the old one until
	/// it leaves.
	#[tokio::test]
	async fn a_reconnect_replaces_the_stale_session() {
		let server = Server::new(crate::server::Config::default());
		let origin = moq_tokio::origin::spawn();
		let consumer = origin.consume();
		let _stale_session = accept(&server, &origin, moq_net::Path::from_static("live/cam0"), &offer())
			.await
			.unwrap();
		let stale = consumer.request("live/cam0", None).await.unwrap();
		let mut catalog = stale
			.track(hang::Catalog::DEFAULT_NAME)
			.unwrap()
			.subscribe(None)
			.await
			.unwrap();

		let _fresh_session = accept(&server, &origin, moq_net::Path::from_static("live/cam0"), &offer())
			.await
			.unwrap();
		// The session binds a real UDP socket, so this runs on the wall clock.
		let stale_viewer = tokio::time::timeout(std::time::Duration::from_millis(200), async {
			loop {
				match catalog.recv_group().await {
					Ok(Some(_)) => continue,
					other => return other.map(|_| ()),
				}
			}
		})
		.await;
		assert!(stale_viewer.is_err(), "the stale viewer ended: {stale_viewer:?}");
		let fresh = consumer.request("live/cam0", None).await.unwrap();
		assert!(!fresh.is_clone(&stale), "viewers reach the reconnected session");
	}
}
