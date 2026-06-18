//! `server subscribe`: WHEP server.
//!
//! `POST /<broadcast-path>` accepts a WHEP SDP offer and returns an SDP
//! answer sourced from the matching MoQ broadcast on the subscribe origin.

use axum::{
	Router,
	body::Bytes,
	extract::{Path, State},
	http::{HeaderMap, HeaderValue, StatusCode, header},
	response::{IntoResponse, Response},
	routing::post,
};
use str0m::Candidate;

use crate::{Error, Result, egress::EgressSource, sdp, server::Server, session};

pub fn router(server: Server) -> Router {
	Router::new().route("/{*path}", post(handle)).with_state(server)
}

async fn handle(server: State<Server>, path: Path<String>, headers: HeaderMap, body: Bytes) -> Response {
	let (server, path) = (server.0, path.0);
	match accept_offer(&server, &path, &headers, body).await {
		Ok((resource_id, answer)) => {
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

async fn accept_offer(server: &Server, path: &str, headers: &HeaderMap, body: Bytes) -> Result<(String, String)> {
	if !is_sdp(headers) {
		return Err(Error::InvalidSdp("expected Content-Type: application/sdp".into()));
	}
	let sdp = std::str::from_utf8(&body).map_err(|err| Error::InvalidSdp(err.to_string()))?;
	let offer = sdp::parse_offer(sdp)?;

	// Look up the MoQ broadcast on the subscriber origin. `request_broadcast` resolves an
	// already-announced broadcast immediately and falls back to a dynamic handler if the
	// origin has one; with neither, it fails fast and the WHEP client retries (typical).
	let consumer = async { server.subscriber().request_broadcast(path)?.await }
		.await
		.map_err(|_| Error::Other(anyhow::anyhow!("broadcast {path} not announced")))?;

	let source = EgressSource::new(consumer).await?;
	let codecs = source.catalog_codecs();
	if codecs.is_empty() {
		return Err(Error::Other(anyhow::anyhow!(
			"catalog has no codecs we can egress (Opus / H.264 / VP8 / VP9)"
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

	Ok((resource_id, sdp::render_answer(&answer)))
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
