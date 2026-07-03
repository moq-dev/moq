//! `server subscribe`: WHEP server.
//!
//! `POST /<broadcast-path>` accepts a WHEP SDP offer and returns an SDP
//! answer sourced from the matching MoQ broadcast on the subscribe origin.

use axum::{
	Router,
	body::Bytes,
	extract::{OriginalUri, Path, State},
	http::{HeaderMap, HeaderValue, StatusCode, header},
	response::{IntoResponse, Response as HttpResponse},
	routing::post,
};
use str0m::Candidate;

use crate::{Error, Result, egress::EgressSource, sdp, server::Server, session};

pub use crate::server::Response;

/// Build the WHEP axum router.
pub fn router(server: Server) -> Router {
	Router::new()
		.route("/{*path}", post(handle).delete(crate::server::delete))
		.with_state(server)
}

async fn handle(
	server: State<Server>,
	path: Path<String>,
	OriginalUri(uri): OriginalUri,
	headers: HeaderMap,
	body: Bytes,
) -> HttpResponse {
	let (server, path) = (server.0, path.0);
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
			tracing::warn!(%err, "whep request failed");
			(status_for(&err), err.to_string()).into_response()
		}
	}
}

/// Router glue: enforce the WHEP `Content-Type` then hand the raw offer to
/// [`accept`], using the request path as the (unauthenticated) broadcast name.
async fn accept_offer(server: &Server, path: &str, headers: &HeaderMap, body: Bytes) -> Result<Response> {
	if !is_sdp(headers) {
		return Err(Error::InvalidSdp("expected Content-Type: application/sdp".into()));
	}
	let offer = std::str::from_utf8(&body).map_err(|err| Error::InvalidSdp(err.to_string()))?;
	accept(server, server.subscriber(), path, offer).await
}

/// Accept a WHEP SDP offer and egress the MoQ broadcast `broadcast` (a path
/// relative to `subscriber`'s root) to the negotiated WebRTC peer.
///
/// This is the negotiation core behind [`router`], exposed so an embedder can own
/// the HTTP route and authentication: verify the request, resolve the authorized
/// broadcast name, scope `subscriber` to the caller's grants, then hand the raw
/// SDP offer here. Taking the consumer explicitly (rather than using the server's)
/// lets the embedder egress through a *scoped* origin, so the subscribe scope is
/// enforced by moq-net exactly as for a native session; the bundled [`router`]
/// passes the server's own (unauthenticated) consumer. It parses the offer,
/// resolves the broadcast on `subscriber`, restricts the answer to the codecs the
/// catalog actually has, registers a media session on the shared mux, and
/// returns the SDP answer plus an opaque `resource_id` for the WHEP `Location`
/// header. The caller must run the returned [`Response`] to drive the MoQ->RTP
/// session. Mirrors [`whip::accept`](super::whip::accept).
///
/// `offer` is the raw SDP body; the caller is responsible for checking the
/// `Content-Type: application/sdp` request header. Fails with [`Error::InvalidSdp`]
/// on a malformed offer, and surfaces a not-announced broadcast (or one outside
/// `subscriber`'s scope) as [`Error::Other`].
pub async fn accept(
	server: &Server,
	subscriber: &moq_net::OriginConsumer,
	broadcast: impl moq_net::AsPath,
	offer: &str,
) -> Result<Response> {
	let offer = sdp::parse_offer(offer)?;

	// Look up the MoQ broadcast on the subscribe origin. `request_broadcast` resolves an
	// already-announced broadcast immediately and falls back to a dynamic handler if the
	// origin has one; with neither, it resolves to an error and the WHEP client retries (typical).
	let broadcast = broadcast.as_path().to_string();
	let consumer = subscriber
		.request_broadcast(&broadcast)
		.await
		.map_err(|_| Error::Other(anyhow::anyhow!("broadcast {broadcast} not announced")))?;

	let source = EgressSource::new(consumer).await?;
	let codecs = source.catalog_codecs();
	if codecs.is_empty() {
		return Err(Error::Other(anyhow::anyhow!(
			"catalog has no codecs we can egress (Opus / H.264 / H.265 / VP8 / VP9 / AV1)"
		)));
	}

	// Register a session on the shared media mux (see whip::accept). Restrict our
	// CodecConfig before accept_offer so the answer intersects the peer's offer
	// with what the catalog actually has, instead of agreeing to a codec we can't
	// fulfil; set the mux's known ICE credentials on the same config.
	let mux = server.mux().await?;
	let (creds, inbound, registration) = mux.register();
	let mut rtc = session::rtc_config_with_codecs(&codecs)
		.set_local_ice_credentials(creds)
		.build(std::time::Instant::now());
	for addr in mux.candidates() {
		let cand = Candidate::host(*addr, "udp").map_err(str0m::RtcError::from)?;
		rtc.add_local_candidate(cand);
	}

	let answer = rtc.sdp_api().accept_offer(offer).map_err(Error::Rtc)?;
	let resource_id = sdp::new_resource_id();
	let session = session::Session::egress(rtc, mux.socket(), mux.candidates().to_vec(), inbound, source);

	// Register before returning so a DELETE that races startup still finds the
	// session; Response::run unregisters itself when it ends.
	let cancel = server.register_session(resource_id.clone());

	Ok(Response {
		resource_id: resource_id.clone(),
		answer: sdp::render_answer(&answer),
		session: crate::server::AcceptedSession {
			server: server.clone(),
			resource_id,
			session: Some(session),
			publish: None,
			registration: Some(registration),
			cancel: Some(cancel),
			role: "whep server",
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
