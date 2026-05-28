//! WHEP egress endpoint.
//!
//! `POST /<broadcast-path>` accepts an SDP offer and returns an SDP answer.
//! The matching broadcast must already be announced on the gateway's
//! subscriber origin.
//!
//! **Status:** v1 scope is WHIP ingest. WHEP egress requires re-packetizing
//! the catalog's bitstreams (Annex-B for H.264, raw frames for VP8/VP9,
//! framed Opus) back into RTP, plus jitter and keyframe coordination. The
//! plumbing is in place; the per-codec re-packetization is a follow-up.

use axum::{
	Router,
	extract::{Path, State},
	http::StatusCode,
	response::{IntoResponse, Response},
	routing::post,
};

use crate::Gateway;

pub fn router(gateway: Gateway) -> Router {
	Router::new().route("/{*path}", post(handle)).with_state(gateway)
}

async fn handle(State(_gateway): State<Gateway>, Path(_path): Path<String>) -> Response {
	(
		StatusCode::NOT_IMPLEMENTED,
		"WHEP egress: re-packetization is not implemented yet. See moq-rtc README.",
	)
		.into_response()
}
