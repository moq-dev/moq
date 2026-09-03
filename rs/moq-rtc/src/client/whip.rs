//! `client publish`: dial a remote WHIP endpoint and push a MoQ broadcast
//! out as RTP.
//!
//! Mints an SDP offer with `sendonly` audio + video, POSTs it to the WHIP
//! resource URL, parses the returned answer, and hands the resulting
//! `str0m::Rtc` to the internal session driver running in egress mode. The
//! bitstream / RTP packetization is identical to the WHEP server path, so
//! most of the work lives in the crate-private egress source.

use str0m::{
	Candidate,
	change::SdpAnswer,
	format::Codec,
	media::{Direction, MediaKind},
};
use url::Url;

use crate::{Error, Result, client::Client, egress::EgressSource, session};

pub(crate) async fn dial(
	client: &Client,
	url: Url,
	origin: moq_net::origin::Consumer,
	path: impl moq_net::AsPath,
) -> Result<()> {
	let source = EgressSource::new(moq_mux::Source::new(origin, path)).await?;
	let codecs = source.catalog_codecs();
	if codecs.is_empty() {
		return Err(Error::Other(anyhow::anyhow!(
			"catalog has no codecs we can egress (Opus / H.264 / H.265 / VP8 / VP9 / AV1)"
		)));
	}

	let (socket, candidates) = session::bind_udp(&client.config().ice_candidates).await?;
	// Restrict to codecs the catalog can actually source so the remote
	// answer doesn't pick a codec we have no rendition for.
	let mut rtc = session::rtc_with_codecs(&codecs);
	for addr in &candidates {
		let cand = Candidate::host(*addr, "udp").map_err(Error::rtc)?;
		rtc.add_local_candidate(cand);
	}

	// Only advertise media kinds the catalog can source. str0m emits a rejected
	// m-line with no payload types when a kind has no enabled codecs, which is not
	// valid SDP and cannot be parsed by the WHIP server.
	let mut api = rtc.sdp_api();
	let mut mids = Vec::new();
	if codecs.iter().any(|codec| matches!(codec, Codec::Opus)) {
		mids.push(api.add_media(MediaKind::Audio, Direction::SendOnly, None, None, None));
	}
	if codecs
		.iter()
		.any(|codec| matches!(codec, Codec::H264 | Codec::H265 | Codec::Vp8 | Codec::Vp9 | Codec::Av1))
	{
		mids.push(api.add_media(MediaKind::Video, Direction::SendOnly, None, None, None));
	}
	let (offer, pending) = api
		.apply()
		.ok_or_else(|| Error::Other(anyhow::anyhow!("no SDP changes to apply")))?;

	let res = client
		.http()
		.post(url.clone())
		.header(reqwest::header::CONTENT_TYPE, "application/sdp")
		.header(reqwest::header::ACCEPT, "application/sdp")
		.body(offer.to_sdp_string())
		.send()
		.await
		.map_err(|err| Error::Other(anyhow::anyhow!("WHIP POST failed: {err}")))?;

	if !res.status().is_success() {
		return Err(Error::Other(anyhow::anyhow!("WHIP server returned {}", res.status())));
	}

	let body = res
		.text()
		.await
		.map_err(|err| Error::Other(anyhow::anyhow!("reading WHIP answer body: {err}")))?;
	let answer = SdpAnswer::from_sdp_string(&body).map_err(|err| Error::InvalidSdp(err.to_string()))?;

	rtc.sdp_api().accept_answer(pending, answer).map_err(Error::rtc)?;
	tracing::info!(%url, "whip client connected");

	// 1:1 socket (no demux on the client): pump its datagrams into the session.
	// The session tags each datagram with the advertised candidate matching its
	// family (str0m matches the destination against a host candidate, not the bind).
	let inbound = session::spawn_socket_reader(socket.clone());
	let mut session = session::Session::egress(rtc, socket, candidates, inbound, source);
	for mid in mids {
		session.open_media(mid)?;
	}
	tokio::spawn(async move {
		let result = session.run().await;
		session::log_session_end("whip client", &result);
	});

	Ok(())
}
