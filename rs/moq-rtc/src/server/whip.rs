//! `server publish`: WHIP server (RFC 9725).
//!
//! `POST /<broadcast-path>` accepts an SDP offer (`Content-Type: application/sdp`)
//! and returns an SDP answer. The request path becomes the broadcast name on
//! the upstream publish origin.

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

/// Build the WHIP axum router.
pub fn router(server: Server) -> Router {
	Router::new()
		.route("/{*path}", post(handle).delete(crate::server::delete))
		.with_state(server)
}

async fn handle(
	State(server): State<Server>,
	Path(path): Path<String>,
	OriginalUri(uri): OriginalUri,
	headers: HeaderMap,
	body: Bytes,
) -> HttpResponse {
	match accept_offer(&server, &path, &headers, body).await {
		Ok(response) => {
			let Response {
				resource_id,
				answer,
				session,
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
async fn accept_offer(server: &Server, path: &str, headers: &HeaderMap, body: Bytes) -> Result<Response> {
	if !is_sdp(headers) {
		return Err(Error::InvalidSdp("expected Content-Type: application/sdp".into()));
	}
	let offer = std::str::from_utf8(&body).map_err(|err| Error::InvalidSdp(err.to_string()))?;
	accept(server, server.publisher(), path, offer).await
}

/// Accept a WHIP SDP offer and publish the negotiated WebRTC media into
/// `publisher` under `broadcast` (a path relative to that producer's root).
///
/// This is the negotiation core behind [`router`], exposed so an embedder can
/// own the HTTP route and authentication: verify the request, resolve the
/// authorized broadcast name, scope `publisher` to the caller's grants, then hand
/// the raw SDP offer here. Taking the producer explicitly (rather than using the
/// server's) lets the embedder publish through a *scoped* origin, so the publish
/// scope is enforced by moq-net exactly as for a native session; the bundled
/// [`router`] passes the server's own (unauthenticated) producer. It parses the
/// offer, registers the broadcast (so a fast subscriber doesn't 404 in the gap
/// before the first RTP packet), registers a media session on the shared mux,
/// and returns the SDP answer plus an opaque `resource_id` for the WHIP
/// `Location` header. The caller must run the returned [`Response`] to drive the
/// RTP->MoQ session.
///
/// `offer` is the raw SDP body; the caller is responsible for checking the
/// `Content-Type: application/sdp` request header. Fails with
/// [`Error::InvalidSdp`] on a malformed offer and surfaces
/// [`moq_net::Error::Unauthorized`] (as [`Error::Other`]) if `broadcast` is
/// outside `publisher`'s scope.
pub async fn accept(
	server: &Server,
	publisher: &moq_net::OriginProducer,
	broadcast: impl moq_net::AsPath,
	offer: &str,
) -> Result<Response> {
	let offer = sdp::parse_offer(offer)?;

	// Register the broadcast on the publish origin before negotiating, so a
	// fast subscriber doesn't see a 404 in the gap between the SDP answer
	// and the first RTP packet.
	let producer = moq_net::BroadcastInfo::new().produce();
	let consumer = producer.consume();
	let publish = publisher
		.publish_broadcast(broadcast, &consumer)
		.map_err(|err| Error::Other(anyhow::anyhow!("failed to publish broadcast: {err}")))?;

	let sink = Box::new(IngestSink::new(producer)?);

	// Register a session on the shared media mux: known ICE credentials (so the
	// demux routes this peer's STUN by ufrag), an inbox to read datagrams from,
	// and a guard the session task holds for its lifetime (unregisters on exit).
	let mux = server.mux().await?;
	let (creds, inbound, registration) = mux.register();
	let mut rtc = RtcConfig::new()
		.set_local_ice_credentials(creds)
		.build(std::time::Instant::now());
	for addr in mux.candidates() {
		let cand = Candidate::host(*addr, "udp").map_err(str0m::RtcError::from)?;
		rtc.add_local_candidate(cand);
	}

	let answer = rtc.sdp_api().accept_offer(offer).map_err(Error::Rtc)?;
	let resource_id = sdp::new_resource_id();
	let session = session::Session::ingest(rtc, mux.socket(), mux.candidates().to_vec(), inbound, sink);

	// Register before returning so a DELETE that races the first packet still
	// finds the session; Response::run unregisters itself when it ends.
	let cancel = server.register_session(resource_id.clone());

	Ok(Response {
		resource_id: resource_id.clone(),
		answer: sdp::render_answer(&answer),
		session: crate::server::AcceptedSession {
			server: server.clone(),
			resource_id,
			session: Some(session),
			publish: Some(publish),
			registration: Some(registration),
			cancel: Some(cancel),
			role: "whip server",
		},
	})
}

fn is_sdp(headers: &HeaderMap) -> bool {
	headers
		.get(header::CONTENT_TYPE)
		.and_then(|v| v.to_str().ok())
		.map(|v| v.eq_ignore_ascii_case("application/sdp"))
		.unwrap_or(false)
}

fn status_for(err: &Error) -> StatusCode {
	match err {
		Error::InvalidSdp(_) => StatusCode::BAD_REQUEST,
		Error::UnsupportedCodec(_) => StatusCode::UNSUPPORTED_MEDIA_TYPE,
		Error::SessionNotFound => StatusCode::NOT_FOUND,
		_ => StatusCode::INTERNAL_SERVER_ERROR,
	}
}
