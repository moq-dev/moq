//! SDP plumbing.
//!
//! WHIP/WHEP both shovel SDP between a peer and str0m as `application/sdp`
//! request/response bodies. On top of str0m's offer/answer parse/serialize this
//! refuses data channels and repairs the answer's text where str0m's falls short.

use std::{borrow::Cow, collections::HashSet};

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

/// The attributes of the transport a BUNDLE group shares. A max-bundle offer can carry
/// them only on the group's first section, which may be a data channel.
const TRANSPORT: [&str; 7] = [
	"a=ice-ufrag:",
	"a=ice-pwd:",
	"a=ice-options:",
	"a=fingerprint:",
	"a=setup:",
	"a=candidate:",
	"a=end-of-candidates",
];

/// Split SDP text into its session lines and each media section's lines, every line
/// keeping its own ending.
fn split_sections<'a>(sdp: &'a str, ending: &str) -> (Vec<&'a str>, Vec<Vec<&'a str>>) {
	let mut session = Vec::new();
	let mut sections: Vec<Vec<&str>> = Vec::new();
	for line in sdp.split_inclusive(ending) {
		if line.starts_with("m=") {
			sections.push(vec![line]);
		} else if let Some(section) = sections.last_mut() {
			section.push(line);
		} else {
			session.push(line);
		}
	}
	(session, sections)
}

/// The port of a section's `m=<media> <port> ...` line.
fn port<'a>(section: &[&'a str]) -> Option<&'a str> {
	section.first()?.split_whitespace().nth(1)
}

/// Take each `m=application` section out of an offer, and its mid out of the BUNDLE group.
///
/// A refused section in the group keeps its share of the group's transport: each
/// attribute no other section or the session carries moves to the session, as do its
/// candidates, which str0m reads from either.
fn refuse_channels(offer: &str) -> (Cow<'_, str>, Refused) {
	let mut refused = Refused::default();
	if !offer.contains("m=application") {
		return (Cow::Borrowed(offer), refused);
	}

	// The offer's CRLF or LF survives.
	let (session, sections) = split_sections(offer, "\n");
	let group: HashSet<&str> = session
		.iter()
		.find_map(|line| line.trim_end().strip_prefix("a=group:BUNDLE"))
		.map(|group| group.split_whitespace().collect())
		.unwrap_or_default();

	let mut kept = Vec::new();
	let mut mids = HashSet::new();
	let mut transport = Vec::new();
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
			mids.insert(mid);
			if group.contains(&mid) {
				transport.extend(
					section
						.into_iter()
						.filter(|line| TRANSPORT.iter().any(|a| line.starts_with(a))),
				);
			}
		}
		refused.sections.push((index, answer));
	}

	// At most one of each other key, decided up front so the work stays linear in the
	// offer. Every candidate moves: str0m dedupes them.
	let mut taken: Vec<&str> = TRANSPORT
		.iter()
		.copied()
		.filter(|key| {
			session
				.iter()
				.chain(kept.iter().flatten())
				.any(|line| line.starts_with(key))
		})
		.collect();
	let mut hoisted: Vec<&str> = Vec::new();
	for line in transport {
		let key = TRANSPORT
			.iter()
			.copied()
			.find(|a| line.starts_with(a))
			.unwrap_or_default();
		if key == "a=candidate:" {
			hoisted.push(line);
		} else if !taken.contains(&key) {
			taken.push(key);
			hoisted.push(line);
		}
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
	for line in hoisted {
		out.push_str(line);
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
	restore_channels(&candidates_on_accepted(&answer), refused)
}

/// Move the answer's candidates off a rejected first section onto the first accepted one.
///
/// str0m writes them only on the first section, where the BUNDLE group's tag usually
/// sits. A rejected first section, such as audio a broadcast can't egress, is out of the
/// group, so a browser would ignore its candidates.
fn candidates_on_accepted(answer: &str) -> Cow<'_, str> {
	let (session, mut sections) = split_sections(answer, "\r\n");
	let is_candidate = |line: &&str| line.starts_with("a=candidate:") || line.starts_with("a=end-of-candidates");
	let Some(first) = sections.first() else {
		return Cow::Borrowed(answer);
	};
	if port(first) != Some("0") || !first.iter().any(is_candidate) {
		return Cow::Borrowed(answer);
	}
	let Some(accepted) = sections.iter().position(|section| port(section) != Some("0")) else {
		return Cow::Borrowed(answer);
	};
	let (candidates, rest): (Vec<&str>, Vec<&str>) = sections[0].iter().copied().partition(is_candidate);
	sections[0] = rest;
	sections[accepted].extend(candidates);
	Cow::Owned(session.into_iter().chain(sections.into_iter().flatten()).collect())
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

	/// A max-bundle offer whose data channel tags the group carries the shared transport
	/// only there, with its media bundle-only; the media still gets that transport.
	#[test]
	fn a_data_channel_tagging_the_bundle_keeps_its_transport() {
		let now = kio::clock::now().into_std();
		let mut offerer = crate::session::rtc_config().unwrap().build(now);
		offerer.add_local_candidate(Candidate::host(([2, 2, 2, 2], 2000).into(), "udp").unwrap());
		let mut api = offerer.sdp_api();
		let audio = api.add_media(MediaKind::Audio, Direction::SendOnly, None, None, None);
		let (offer, _) = api.apply().unwrap();
		let offer = offer.to_sdp_string();
		let (session, media) = offer.split_at(offer.find("m=audio").unwrap());
		let session = session.replace(
			&format!("a=group:BUNDLE {audio}\r\n"),
			&format!("a=group:BUNDLE dc {audio}\r\n"),
		);
		let is_transport = |line: &&str| super::TRANSPORT.iter().any(|a| line.starts_with(a));
		let transport: String = media.split_inclusive("\r\n").filter(is_transport).collect();
		assert!(transport.contains("a=candidate:"), "{offer}");
		let media: String = media
			.split_inclusive("\r\n")
			.filter(|line| !is_transport(line))
			.collect::<String>()
			.replacen("m=audio 9 ", "m=audio 0 ", 1)
			+ "a=bundle-only\r\n";
		let offer = format!(
			"{session}m=application 9 UDP/DTLS/SCTP webrtc-datachannel\r\n\
			c=IN IP4 0.0.0.0\r\n\
			a=mid:dc\r\n\
			{transport}\
			a=sctp-port:5000\r\n{media}"
		);

		let answer = answer(&offer);
		let sections: Vec<&str> = answer.split("m=").skip(1).collect();
		assert!(sections[0].starts_with("application 0 "), "{answer}");
		assert!(sections[1].starts_with("audio 9 "), "{answer}");
		assert!(answer.contains(&format!("a=group:BUNDLE {audio}\r\n")), "{answer}");
	}

	/// A rejected first section, such as audio a broadcast can't egress, leaves the
	/// answer's candidates to the first accepted one.
	#[test]
	fn candidates_skip_a_rejected_first_section() {
		let now = kio::clock::now().into_std();
		let mut offerer = crate::session::rtc_config().unwrap().build(now);
		let mut api = offerer.sdp_api();
		api.add_media(MediaKind::Audio, Direction::RecvOnly, None, None, None);
		let video = api.add_media(MediaKind::Video, Direction::RecvOnly, None, None, None);
		let (offer, pending) = api.apply().unwrap();

		let (offer, refused) = parse_offer(&offer.to_sdp_string()).unwrap();
		let mut rtc = crate::session::rtc_with_codecs(&[str0m::format::Codec::Vp8]).unwrap();
		rtc.add_local_candidate(Candidate::host(([1, 1, 1, 1], 1000).into(), "udp").unwrap());
		let answer = render_answer(&rtc.sdp_api().accept_offer(offer).unwrap(), &refused);

		let sections: Vec<&str> = answer.split("m=").skip(1).collect();
		assert!(sections[0].starts_with("audio 0 "), "{answer}");
		assert!(!sections[0].contains("a=candidate:"), "{answer}");
		assert!(sections[1].starts_with("video 9 "), "{answer}");
		assert!(sections[1].contains("a=candidate:"), "{answer}");
		assert!(answer.contains(&format!("a=group:BUNDLE {video}\r\n")), "{answer}");

		let answer = SdpAnswer::from_sdp_string(&answer).unwrap();
		offerer.sdp_api().accept_answer(pending, answer).unwrap();
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

	/// A body near the request limit of transport lines and data channels stays linear:
	/// each key and mid is checked against a set decided once, not rescanned per line.
	#[test]
	fn a_flood_of_refused_transport_stays_linear() {
		const LINES: usize = 100_000;
		let mut offer = String::from("v=0\r\no=- 1 2 IN IP4 0.0.0.0\r\ns=-\r\nt=0 0\r\na=group:BUNDLE x");
		for mid in 0..LINES {
			offer.push_str(&format!(" {mid}"));
		}
		offer.push_str("\r\nm=application 9 UDP/DTLS/SCTP webrtc-datachannel\r\na=mid:x\r\n");
		for _ in 0..LINES {
			offer.push_str("a=ice-ufrag:u\r\n");
		}
		for _ in 0..LINES {
			offer.push_str("a=setup:actpass\r\n");
		}
		for n in 0..LINES {
			offer.push_str(&format!("a=candidate:{n} 1 udp 1 1.1.1.1 1000 typ host\r\n"));
		}
		for mid in 0..LINES {
			offer.push_str(&format!(
				"m=application 9 UDP/DTLS/SCTP webrtc-datachannel\r\na=mid:{mid}\r\n"
			));
		}

		// Quadratic work here takes ~10^10 comparisons, past nextest's timeout.
		let (body, refused) = super::refuse_channels(&offer);
		assert_eq!(refused.sections.len(), LINES + 1);
		assert_eq!(body.matches("a=ice-ufrag:").count(), 1);
		assert_eq!(body.matches("a=setup:").count(), 1);
		assert_eq!(body.matches("a=candidate:").count(), LINES);
		assert!(!body.contains("a=group:BUNDLE"));
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
