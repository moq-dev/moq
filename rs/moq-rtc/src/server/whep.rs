//! `server subscribe`: WHEP server.
//!
//! `POST /<broadcast-path>` would accept a WHEP SDP offer and return an
//! SDP answer sourced from the matching MoQ broadcast on the subscribe
//! origin. The HTTP plumbing is in place; the per-codec re-packetizer
//! (MoQ frame -> RTP) is the blocker shared with [`crate::client::whip`].

use axum::{
	Router,
	extract::{Path, State},
	http::StatusCode,
	response::{IntoResponse, Response},
	routing::post,
};

use crate::server::Server;

pub fn router(server: Server) -> Router {
	Router::new().route("/{*path}", post(handle)).with_state(server)
}

async fn handle(State(_server): State<Server>, Path(_path): Path<String>) -> Response {
	(
		StatusCode::NOT_IMPLEMENTED,
		"server subscribe (WHEP): re-packetization is not implemented yet. See moq-rtc README.",
	)
		.into_response()
}
