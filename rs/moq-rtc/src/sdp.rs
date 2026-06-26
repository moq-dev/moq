//! SDP plumbing.
//!
//! WHIP/WHEP both shovel SDP between a peer and str0m as `application/sdp`
//! request/response bodies. The only thing we add on top of str0m's offer/answer
//! parse/serialize is a tiny wrapper to keep the call sites readable.

use std::str::FromStr;

use crate::{Error, Result};

/// Parse an `application/sdp` body as an offer.
pub fn parse_offer(body: &str) -> Result<str0m::change::SdpOffer> {
	str0m::change::SdpOffer::from_sdp_string(body).map_err(|err| Error::InvalidSdp(err.to_string()))
}

/// Serialize an SDP answer for the `application/sdp` response body.
pub fn render_answer(answer: &str0m::change::SdpAnswer) -> String {
	answer.to_sdp_string()
}

/// Build a stable WHIP/WHEP resource identifier from a UUID v4.
pub fn new_resource_id() -> String {
	uuid::Uuid::new_v4().to_string()
}

/// Parse a `Location:`-style resource path into its trailing UUID component.
///
/// WHIP DELETEs come back to `/<broadcast>/<resource-id>`; this strips
/// everything but the id so the gateway can look up the session.
pub fn parse_resource_id(path: &str) -> Result<uuid::Uuid> {
	let last = path
		.rsplit('/')
		.find(|s| !s.is_empty())
		.ok_or_else(|| Error::InvalidSdp("missing resource id".into()))?;
	uuid::Uuid::from_str(last).map_err(|err| Error::InvalidSdp(err.to_string()))
}
