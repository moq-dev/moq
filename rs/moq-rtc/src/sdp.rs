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

/// Count accepted audio/video m-lines where this endpoint receives RTP.
pub fn count_local_recv(sdp: &str) -> usize {
	count_media(sdp, Direction::receives)
}

/// Count accepted audio/video m-lines where the remote endpoint sends RTP.
pub fn count_remote_send(sdp: &str) -> usize {
	count_media(sdp, Direction::sends)
}

#[derive(Clone, Copy)]
enum Direction {
	SendOnly,
	RecvOnly,
	SendRecv,
	Inactive,
}

impl Direction {
	fn parse(line: &str) -> Option<Self> {
		match line {
			"a=sendonly" => Some(Self::SendOnly),
			"a=recvonly" => Some(Self::RecvOnly),
			"a=sendrecv" => Some(Self::SendRecv),
			"a=inactive" => Some(Self::Inactive),
			_ => None,
		}
	}

	fn sends(self) -> bool {
		matches!(self, Self::SendOnly | Self::SendRecv)
	}

	fn receives(self) -> bool {
		matches!(self, Self::RecvOnly | Self::SendRecv)
	}
}

struct Media {
	audio_or_video: bool,
	accepted: bool,
	direction: Direction,
}

fn count_media(sdp: &str, matches_direction: fn(Direction) -> bool) -> usize {
	let mut count = 0;
	let mut session_direction = Direction::SendRecv;
	let mut media = None;

	for line in sdp.lines() {
		let line = line.trim();
		if line.starts_with("m=") {
			finish_media(&mut count, media.take(), matches_direction);
			media = Some(parse_media(line, session_direction));
			continue;
		}

		let Some(direction) = Direction::parse(line) else {
			continue;
		};
		match &mut media {
			Some(media) => media.direction = direction,
			None => session_direction = direction,
		}
	}

	finish_media(&mut count, media, matches_direction);
	count
}

fn parse_media(line: &str, direction: Direction) -> Media {
	let mut parts = line.split_whitespace();
	let kind = parts.next().unwrap_or_default().trim_start_matches("m=");
	let port = parts.next().unwrap_or_default();
	Media {
		audio_or_video: matches!(kind, "audio" | "video"),
		accepted: !port.is_empty() && port != "0",
		direction,
	}
}

fn finish_media(count: &mut usize, media: Option<Media>, matches_direction: fn(Direction) -> bool) {
	let Some(media) = media else {
		return;
	};
	if media.audio_or_video && media.accepted && matches_direction(media.direction) {
		*count += 1;
	}
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
	use super::{count_local_recv, count_remote_send, ensure_media_format};

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

	#[test]
	fn counts_only_accepted_receiving_media() {
		let sdp = concat!(
			"v=0\r\n",
			"a=sendrecv\r\n",
			"m=audio 9 UDP/TLS/RTP/SAVPF 111\r\n",
			"a=recvonly\r\n",
			"m=video 0 UDP/TLS/RTP/SAVPF 96\r\n",
			"a=recvonly\r\n",
			"m=application 9 UDP/DTLS/SCTP webrtc-datachannel\r\n",
			"a=sendrecv\r\n",
			"m=video 9 UDP/TLS/RTP/SAVPF 97\r\n",
			"a=sendonly\r\n",
		);
		assert_eq!(count_local_recv(sdp), 1);
		assert_eq!(count_remote_send(sdp), 1);
	}
}
