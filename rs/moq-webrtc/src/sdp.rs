//! SDP plumbing.
//!
//! WHIP/WHEP both shovel SDP between a peer and str0m as `application/sdp`
//! request/response bodies. On top of str0m's offer/answer parse/serialize this
//! refuses data channels and repairs the answer's text where str0m's falls short.

use std::borrow::Cow;

use crate::{Error, Result};

/// An offer's data channel sections, each answered rejected beside str0m's answer.
///
/// The gateway carries no data channels, and str0m has no setting that refuses one: it
/// accepts an offered `m=application` section and runs SCTP that nobody reads. So
/// [`parse_offer`] takes those sections out before str0m sees the offer, and
/// [`render_answer`] puts each back at its index, with port 0 and out of the BUNDLE
/// group.
#[derive(Debug, Default)]
pub struct Refused {
	/// Each refused section's index among the offer's media sections, in order, and
	/// its rejected answer.
	sections: Vec<(usize, String)>,
}

/// Parse an `application/sdp` body as an offer, setting its data channels aside.
pub fn parse_offer(body: &str) -> Result<(str0m::change::SdpOffer, Refused)> {
	let (body, refused) = refuse_channels(body);
	let offer = str0m::change::SdpOffer::from_sdp_string(&body).map_err(|err| Error::InvalidSdp(err.to_string()))?;
	Ok((offer, refused))
}

/// Take each `m=application` section out of an offer, and its mid out of the BUNDLE group.
fn refuse_channels(offer: &str) -> (Cow<'_, str>, Refused) {
	let mut refused = Refused::default();
	if !offer.contains("m=application") {
		return (Cow::Borrowed(offer), refused);
	}

	// Each line keeps its own ending, so the offer's CRLF or LF survives.
	let mut session = Vec::new();
	let mut sections: Vec<Vec<&str>> = Vec::new();
	for line in offer.split_inclusive('\n') {
		if line.starts_with("m=") {
			sections.push(vec![line]);
		} else if let Some(section) = sections.last_mut() {
			section.push(line);
		} else {
			session.push(line);
		}
	}

	let mut kept = Vec::new();
	let mut mids = Vec::new();
	for (index, section) in sections.into_iter().enumerate() {
		let mut tokens = section[0].split_whitespace();
		if tokens.next() != Some("m=application") {
			kept.push(section);
			continue;
		}
		// m=application <port> <proto> <fmt>...; the answer echoes the proto and formats.
		let _port = tokens.next();
		let mut answer = format!(
			"m=application 0 {}\r\nc=IN IP4 0.0.0.0\r\n",
			tokens.collect::<Vec<_>>().join(" ")
		);
		let mid = section.iter().find_map(|line| line.trim_end().strip_prefix("a=mid:"));
		if let Some(mid) = mid {
			answer.push_str(&format!("a=mid:{mid}\r\n"));
			mids.push(mid);
		}
		refused.sections.push((index, answer));
	}

	let mut out = String::with_capacity(offer.len());
	for line in session {
		let text = line.trim_end();
		match text.strip_prefix("a=group:BUNDLE") {
			Some(group) => {
				let group: Vec<&str> = group.split_whitespace().filter(|mid| !mids.contains(mid)).collect();
				if !group.is_empty() {
					let ending = &line[text.len()..];
					out.push_str(&format!("a=group:BUNDLE {}{ending}", group.join(" ")));
				}
			}
			None => out.push_str(line),
		}
	}
	for line in kept.into_iter().flatten() {
		out.push_str(line);
	}
	(Cow::Owned(out), refused)
}

/// Serialize an SDP answer for the `application/sdp` response body, with each of the
/// offer's [`Refused`] data channels answered rejected in its place.
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
pub fn render_answer(answer: &str0m::change::SdpAnswer, refused: &Refused) -> String {
	// SDP uses CRLF line endings (RFC 4566); splitting and rejoining on "\r\n"
	// round-trips exactly, including the trailing CRLF.
	let answer = answer
		.to_sdp_string()
		.split("\r\n")
		.map(ensure_media_format)
		.collect::<Vec<Cow<str>>>()
		.join("\r\n");
	restore_channels(&answer, refused)
}

/// Put each refused data channel section back into the answer at its offer's index.
fn restore_channels(answer: &str, refused: &Refused) -> String {
	if refused.sections.is_empty() {
		return answer.to_string();
	}
	let mut out = String::with_capacity(answer.len() + 64 * refused.sections.len());
	let mut pending = refused.sections.iter().peekable();
	let mut index = 0;
	for line in answer.split_inclusive("\r\n") {
		if line.starts_with("m=") {
			// Refused sections come before the accepted one now at their index.
			while let Some((_, section)) = pending.next_if(|(at, _)| *at == index) {
				out.push_str(section);
				index += 1;
			}
			index += 1;
		}
		out.push_str(line);
	}
	// Those after the last accepted section.
	for (_, section) in pending {
		out.push_str(section);
	}
	out
}

/// Add a `b=TIAS` line at `rate` to every audio and video section the answer accepts, so
/// the publisher's encoder targets at most `rate` from its first frame.
///
/// TIAS (RFC 3890) is bits per second without transport overhead, and what Chrome and
/// Firefox read first. It is the only line: str0m's parser takes at most one `b=` per
/// section and fails the whole description on a second, so adding `b=AS` too would break
/// every str0m publisher, this crate's own WHIP client included. str0m renders no
/// bandwidth line in an answer, so it's spliced into the text after the section's `c=`
/// line, where RFC 4566 orders `b=`.
pub fn cap_answer(answer: &str, rate: moq_net::bandwidth::Rate) -> String {
	let lines = format!("b=TIAS:{}\r\n", rate.as_bps());
	let mut out = String::with_capacity(answer.len() + 64);
	let mut capped = false;
	for line in answer.split_inclusive("\r\n") {
		if line.starts_with("m=") {
			// m=<media> <port> ...; a rejected section has port 0.
			let mut tokens = line.split_whitespace();
			let media = tokens.next().unwrap_or_default();
			capped = matches!(media, "m=audio" | "m=video") && tokens.next() != Some("0");
		}
		out.push_str(line);
		if capped && line.starts_with("c=") {
			out.push_str(&lines);
			capped = false;
		}
	}
	out
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
#[cfg(feature = "server")]
pub fn parse_resource_id(path: &str) -> Result<uuid::Uuid> {
	let last = path
		.rsplit('/')
		.find(|s| !s.is_empty())
		.ok_or_else(|| Error::InvalidSdp("missing resource id".into()))?;
	last.parse::<uuid::Uuid>()
		.map_err(|err| Error::InvalidSdp(err.to_string()))
}

#[cfg(test)]
mod tests {
	use str0m::Candidate;
	use str0m::change::SdpAnswer;
	use str0m::media::{Direction, MediaKind};

	use super::{cap_answer, ensure_media_format, parse_offer, render_answer};

	/// Answer `offer` as the WHIP server does, with one local candidate.
	fn answer(offer: &str) -> String {
		let (offer, refused) = parse_offer(offer).unwrap();
		let mut rtc = crate::session::rtc_with_codecs(&[str0m::format::Codec::Opus]).unwrap();
		let candidate = Candidate::host(([1, 1, 1, 1], 1000).into(), "udp").unwrap();
		rtc.add_local_candidate(candidate);
		let answer = rtc.sdp_api().accept_offer(offer).unwrap();
		render_answer(&answer, &refused)
	}

	/// The gateway carries no data channels, so an offered one is answered rejected and
	/// left out of the BUNDLE group, while the media beside it is accepted, and the
	/// offerer takes that answer.
	#[test]
	fn an_offered_data_channel_is_answered_rejected() {
		let now = kio::clock::now().into_std();
		let mut offerer = crate::session::rtc_config().unwrap().build(now);
		let mut api = offerer.sdp_api();
		let audio = api.add_media(MediaKind::Audio, Direction::SendOnly, None, None, None);
		api.add_channel("chat".into());
		let (offer, pending) = api.apply().unwrap();
		let offer = offer.to_sdp_string();
		let channel = offer
			.split("\r\n")
			.filter_map(|line| line.strip_prefix("a=mid:"))
			.find(|mid| *mid != audio.to_string())
			.unwrap();
		assert!(offer.contains("m=application 9 "), "{offer}");

		let answer = answer(&offer);
		let audio_at = answer.find("m=audio 9 ").expect("the audio was rejected");
		let rejected =
			format!("m=application 0 UDP/DTLS/SCTP webrtc-datachannel\r\nc=IN IP4 0.0.0.0\r\na=mid:{channel}\r\n");
		let channel_at = answer.find(&rejected).expect(&answer);
		assert!(audio_at < channel_at, "the sections changed order: {answer}");
		assert!(answer.ends_with(&rejected), "{answer}");
		assert!(answer.contains(&format!("a=group:BUNDLE {audio}\r\n")), "{answer}");

		let answer = SdpAnswer::from_sdp_string(&answer).unwrap();
		offerer.sdp_api().accept_answer(pending, answer).unwrap();
	}

	/// An offer that puts its data channel first gets it answered first, and the
	/// candidates on the accepted media rather than the rejected line.
	#[test]
	fn a_data_channel_offered_first_is_answered_first() {
		let now = kio::clock::now().into_std();
		let mut offerer = crate::session::rtc_config().unwrap().build(now);
		let mut api = offerer.sdp_api();
		let audio = api.add_media(MediaKind::Audio, Direction::SendOnly, None, None, None);
		let (offer, _) = api.apply().unwrap();
		let offer = offer.to_sdp_string();
		let (session, media) = offer.split_at(offer.find("m=audio").unwrap());
		let session = session.replace(
			&format!("a=group:BUNDLE {audio}\r\n"),
			&format!("a=group:BUNDLE dc {audio}\r\n"),
		);
		// LF line endings, as some clients send, survive the refusal.
		let offer = format!(
			"{session}m=application 9 UDP/DTLS/SCTP webrtc-datachannel\r\n\
			c=IN IP4 0.0.0.0\r\n\
			a=mid:dc\r\n\
			a=sctp-port:5000\r\n{media}"
		)
		.replace("\r\n", "\n");

		let answer = answer(&offer);
		let sections: Vec<&str> = answer.split("m=").skip(1).collect();
		assert_eq!(sections.len(), 2, "{answer}");
		assert_eq!(
			sections[0],
			"application 0 UDP/DTLS/SCTP webrtc-datachannel\r\nc=IN IP4 0.0.0.0\r\na=mid:dc\r\n"
		);
		assert!(sections[1].starts_with("audio 9 "), "{answer}");
		assert!(sections[1].contains("a=candidate:"), "{answer}");
		assert!(answer.contains(&format!("a=group:BUNDLE {audio}\r\n")), "{answer}");
	}

	/// An offer of only a data channel has nothing to answer.
	#[test]
	fn an_offer_of_only_a_data_channel_has_no_bundle() {
		let offer = "v=0\r\n\
			o=- 1 2 IN IP4 0.0.0.0\r\n\
			s=-\r\n\
			t=0 0\r\n\
			a=group:BUNDLE dc\r\n\
			m=application 9 UDP/DTLS/SCTP webrtc-datachannel\r\n\
			c=IN IP4 0.0.0.0\r\n\
			a=mid:dc\r\n";
		let (body, refused) = super::refuse_channels(offer);
		assert_eq!(body, "v=0\r\no=- 1 2 IN IP4 0.0.0.0\r\ns=-\r\nt=0 0\r\n");
		assert_eq!(refused.sections.len(), 1);
	}

	#[test]
	fn a_capped_answer_bounds_each_accepted_media_section() {
		let answer = "v=0\r\n\
			o=- 1 2 IN IP4 0.0.0.0\r\n\
			s=-\r\n\
			t=0 0\r\n\
			m=audio 9 UDP/TLS/RTP/SAVPF 111\r\n\
			c=IN IP4 0.0.0.0\r\n\
			a=mid:0\r\n\
			m=video 0 UDP/TLS/RTP/SAVPF 0\r\n\
			c=IN IP4 0.0.0.0\r\n\
			a=mid:1\r\n\
			m=application 0 UDP/DTLS/SCTP webrtc-datachannel\r\n\
			c=IN IP4 0.0.0.0\r\n\
			a=mid:2\r\n\
			m=video 9 UDP/TLS/RTP/SAVPF 96\r\n\
			c=IN IP4 0.0.0.0\r\n\
			a=mid:3\r\n";
		let capped = cap_answer(answer, moq_net::bandwidth::Rate::from_kbps(2_500));
		let sections: Vec<&str> = capped.split("m=").collect();
		let bounded = "c=IN IP4 0.0.0.0\r\nb=TIAS:2500000\r\na=mid";
		assert!(!sections[0].contains("b="), "the session section stays unbounded");
		assert!(sections[1].contains(bounded), "{}", sections[1]);
		assert!(!sections[2].contains("b="), "a rejected section stays unbounded");
		assert!(!sections[3].contains("b="), "the rejected data channel stays unbounded");
		assert!(sections[4].contains(bounded), "{}", sections[4]);
	}

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
