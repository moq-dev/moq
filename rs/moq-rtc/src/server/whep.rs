//! `server subscribe`: WHEP server.
//!
//! `POST /<broadcast-path>` accepts a WHEP SDP offer and returns an SDP
//! answer sourced from the matching MoQ broadcast on the subscribe origin.

use axum::{
	Router,
	body::Bytes,
	extract::{Path, State},
	http::{HeaderMap, HeaderValue, StatusCode, header},
	response::{IntoResponse, Response as HttpResponse},
	routing::post,
};
use str0m::Candidate;

use crate::{Error, Result, egress::EgressSource, sdp, server::Server, session};

pub use crate::server::Response;

/// Build the WHEP axum router.
pub fn router(server: Server) -> Router {
	Router::new().route("/{*path}", post(handle)).with_state(server)
}

async fn handle(server: State<Server>, path: Path<String>, headers: HeaderMap, body: Bytes) -> HttpResponse {
	let (server, path) = (server.0, path.0);
	match accept_offer(&server, &path, &headers, body).await {
		Ok(Response { resource_id, answer }) => {
			let mut response_headers = HeaderMap::new();
			response_headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/sdp"));
			if let Ok(loc) = HeaderValue::from_str(&format!("/{path}/{resource_id}")) {
				response_headers.insert(header::LOCATION, loc);
			}
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
	accept(server, path, offer).await
}

/// Accept a WHEP SDP offer and egress the MoQ broadcast `broadcast` (a path
/// relative to the subscribe origin's root) to the negotiated WebRTC peer.
///
/// This is the negotiation core behind [`router`], exposed so an embedder can own
/// the HTTP route and authentication: verify the request, resolve the authorized
/// broadcast name, then hand the raw SDP offer here. It parses the offer, resolves
/// the broadcast on the subscribe origin, restricts the answer to the codecs the
/// catalog actually has, binds the ICE socket, spawns the MoQ->RTP session, and
/// returns the SDP answer plus an opaque `resource_id` for the WHEP `Location`
/// header. Mirrors [`whip::accept`](super::whip::accept).
///
/// `offer` is the raw SDP body; the caller is responsible for checking the
/// `Content-Type: application/sdp` request header. Fails with [`Error::InvalidSdp`]
/// on a malformed offer, and surfaces a not-announced broadcast (or one outside
/// the subscribe origin's scope) as [`Error::Other`].
pub async fn accept(server: &Server, broadcast: impl moq_net::AsPath, offer: &str) -> Result<Response> {
	let offer = sdp::parse_offer(offer)?;

	// Look up the MoQ broadcast on the subscriber origin. `request_broadcast` resolves an
	// already-announced broadcast immediately and falls back to a dynamic handler if the
	// origin has one; with neither, it fails fast and the WHEP client retries (typical).
	let broadcast = broadcast.as_path().to_string();
	let consumer = async { server.subscriber().request_broadcast(&broadcast)?.await }
		.await
		.map_err(|_| Error::Other(anyhow::anyhow!("broadcast {broadcast} not announced")))?;

	let source = EgressSource::new(consumer).await?;
	let codecs = source.catalog_codecs();
	if codecs.is_empty() {
		return Err(Error::Other(anyhow::anyhow!(
			"catalog has no codecs we can egress (Opus / H.264 / H.265 / VP8 / VP9 / AV1)"
		)));
	}

	let (socket, candidates) = session::bind_udp(&server.config().ice_candidates).await?;
	// Restrict our CodecConfig before accept_offer so the answer intersects
	// the peer's offer with what the catalog actually has, instead of
	// agreeing to a codec we can't fulfil.
	let mut rtc = session::rtc_with_codecs(&codecs);
	for addr in &candidates {
		let cand = Candidate::host(*addr, "udp").map_err(str0m::RtcError::from)?;
		rtc.add_local_candidate(cand);
	}

	let answer = rtc.sdp_api().accept_offer(offer).map_err(Error::Rtc)?;
	let resource_id = sdp::new_resource_id();
	let session = session::Session::egress(rtc, socket, source);

	tokio::spawn(async move {
		if let Err(err) = session.run().await {
			tracing::warn!(%err, "whep session ended");
		}
	});

	Ok(Response {
		resource_id,
		answer: sdp::render_answer(&answer),
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
