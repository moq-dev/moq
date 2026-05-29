//! `client publish`: dial a remote WHIP endpoint and push a MoQ broadcast
//! out as RTP.
//!
//! Mints an SDP offer with `sendonly` audio + video, POSTs it to the WHIP
//! resource URL, parses the returned answer, and hands the resulting
//! `str0m::Rtc` to a `crate::session::Session` running in egress mode. The
//! bitstream / RTP packetization is identical to the WHEP server path, so
//! most of the work lives in [`crate::egress`].

use std::time::Instant;

use str0m::{
	Candidate, Rtc,
	change::SdpAnswer,
	media::{Direction, MediaKind},
};
use url::Url;

use crate::{Error, Result, client::Client, egress::EgressSource, session};

pub(crate) async fn dial(client: &Client, url: Url, broadcast: moq_net::BroadcastConsumer) -> Result<()> {
	let source = EgressSource::new(broadcast).await?;

	let (socket, candidates) = session::bind_udp(&client.config().ice_candidates).await?;
	let mut rtc = Rtc::new(Instant::now());
	for addr in &candidates {
		let cand = Candidate::host(*addr, "udp").map_err(str0m::RtcError::from)?;
		rtc.add_local_candidate(cand);
	}

	// Advertise sendonly audio + video. str0m's default CodecConfig enables
	// every codec it supports; the remote answer picks the intersection.
	// EgressSource picks a matching catalog rendition once `MediaAdded`
	// fires per accepted m-line.
	let mut api = rtc.sdp_api();
	api.add_media(MediaKind::Audio, Direction::SendOnly, None, None, None);
	api.add_media(MediaKind::Video, Direction::SendOnly, None, None, None);
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

	rtc.sdp_api().accept_answer(pending, answer).map_err(Error::Rtc)?;
	tracing::info!(%url, "whip client connected");

	let session = session::Session::egress(rtc, socket, source);
	tokio::spawn(async move {
		if let Err(err) = session.run().await {
			tracing::warn!(%err, "whip client session ended");
		}
	});

	Ok(())
}
