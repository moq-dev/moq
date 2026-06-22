//! `client subscribe`: dial a remote WHEP endpoint, ingest RTP into a
//! local moq-net broadcast.
//!
//! Mints an SDP offer with `recvonly` audio and video, POSTs it to the
//! WHEP resource URL with `Content-Type: application/sdp`, parses the
//! returned answer, then hands the resulting `str0m::Rtc` to a
//! [`crate::session::Session`] that reuses [`IngestSink`] (the same sink the
//! WHIP server uses).

use std::time::Instant;

use str0m::{
	Candidate, Rtc,
	change::SdpAnswer,
	media::{Direction, MediaKind},
};
use url::Url;

use crate::{Error, Result, client::Client, ingest::IngestSink, session};

pub(crate) async fn dial(client: &Client, url: Url, broadcast: moq_net::BroadcastProducer) -> Result<()> {
	let sink = Box::new(IngestSink::new(broadcast)?);

	let (socket, candidates) = session::bind_udp(&client.config().ice_candidates).await?;
	let mut rtc = Rtc::new(Instant::now());
	for addr in &candidates {
		let cand = Candidate::host(*addr, "udp").map_err(str0m::RtcError::from)?;
		rtc.add_local_candidate(cand);
	}

	// Ask for both audio and video, recvonly. The remote answer can decline
	// either by signaling inactive on that m-line.
	let mut api = rtc.sdp_api();
	api.add_media(MediaKind::Audio, Direction::RecvOnly, None, None, None);
	api.add_media(MediaKind::Video, Direction::RecvOnly, None, None, None);
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
		.map_err(|err| Error::Other(anyhow::anyhow!("WHEP POST failed: {err}")))?;

	if !res.status().is_success() {
		return Err(Error::Other(anyhow::anyhow!("WHEP server returned {}", res.status())));
	}

	let body = res
		.text()
		.await
		.map_err(|err| Error::Other(anyhow::anyhow!("reading WHEP answer body: {err}")))?;
	let answer = SdpAnswer::from_sdp_string(&body).map_err(|err| Error::InvalidSdp(err.to_string()))?;

	rtc.sdp_api().accept_answer(pending, answer).map_err(Error::Rtc)?;
	tracing::info!(%url, "whep client connected");

	// 1:1 socket (no demux on the client): pump its datagrams into the session.
	// Report the first advertised candidate as the local address (str0m matches
	// each datagram's destination against a host candidate, not the socket bind).
	let local = candidates[0];
	let inbound = session::spawn_socket_reader(socket.clone());
	let session = session::Session::ingest(rtc, socket, local, inbound, sink);
	tokio::spawn(async move {
		if let Err(err) = session.run().await {
			tracing::warn!(%err, "whep client session ended");
		}
	});

	Ok(())
}
