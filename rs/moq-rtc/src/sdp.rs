//! SDP plumbing.
//!
//! WHIP/WHEP both shovel SDP between a peer and str0m as `application/sdp`
//! request/response bodies. The only thing we add on top of str0m's offer/answer
//! parse/serialize is a tiny wrapper to keep the call sites readable.

use std::borrow::Cow;
use std::str::FromStr;

use crate::{Error, Result};

/// Parse an `application/sdp` body as an offer.
pub fn parse_offer(body: &str) -> Result<str0m::change::SdpOffer> {
	str0m::change::SdpOffer::from_sdp_string(body).map_err(|err| Error::InvalidSdp(err.to_string()))
}

/// Serialize an SDP answer for the `application/sdp` response body.
///
/// str0m can emit a *rejected* media line (port 0) with an EMPTY format list,
/// e.g. `m=audio 0 UDP/TLS/RTP/SAVPF `. This happens when we restrict the
/// `CodecConfig` (see [`crate::session::rtc_config_with_codecs`]) and the peer's offer
/// carries a codec the broadcast can't egress -- the classic case being AAC
/// audio over WHEP, which we can't carry, so the audio m-line comes back
/// rejected with no payload. An m-line with no `<fmt>` violates RFC 4566, and a
/// browser rejects the WHOLE answer in `setRemoteDescription`, killing playback
/// of the media (e.g. video) that WAS negotiated. So we give any such line a
/// placeholder static payload; the media stays rejected (port 0), so the
/// placeholder is never used.
pub fn render_answer(answer: &str0m::change::SdpAnswer) -> String {
	// SDP uses CRLF line endings (RFC 4566); splitting and rejoining on "\r\n"
	// round-trips exactly, including the trailing CRLF.
	answer
		.to_sdp_string()
		.split("\r\n")
		.map(ensure_media_format)
		.collect::<Vec<Cow<str>>>()
		.join("\r\n")
}

/// Give an `m=` line a placeholder format payload when it has none (see
/// [`render_answer`]); every other line passes through untouched.
fn ensure_media_format(line: &str) -> Cow<'_, str> {
	if !line.starts_with("m=") {
		return Cow::Borrowed(line);
	}
	// m=<media> <port> <proto> <fmt>...; fewer than 4 tokens means no <fmt>.
	if line.split_whitespace().count() >= 4 {
		return Cow::Borrowed(line);
	}
	Cow::Owned(format!("{} 0", line.trim_end()))
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

#[cfg(test)]
mod tests {
	use super::ensure_media_format;

	#[test]
	fn rejected_mline_with_no_format_gets_placeholder() {
		// str0m's malformed rejected audio line.
		assert_eq!(
			ensure_media_format("m=audio 0 UDP/TLS/RTP/SAVPF "),
			"m=audio 0 UDP/TLS/RTP/SAVPF 0"
		);
		assert_eq!(
			ensure_media_format("m=audio 0 UDP/TLS/RTP/SAVPF"),
			"m=audio 0 UDP/TLS/RTP/SAVPF 0"
		);
	}

	#[test]
	fn well_formed_lines_are_untouched() {
		let video = "m=video 9 UDP/TLS/RTP/SAVPF 96 97";
		assert_eq!(ensure_media_format(video), video);
		let attr = "a=ice-ufrag:abcd";
		assert_eq!(ensure_media_format(attr), attr);
	}
}
