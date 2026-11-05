//! `client subscribe`: dial a remote WHEP endpoint, ingest RTP into a
//! local moq-net broadcast.
//!
//! Mints an SDP offer with `recvonly` audio and video, POSTs it to the
//! WHEP resource URL with `Content-Type: application/sdp`, parses the
//! returned answer, then hands the resulting `str0m::Rtc` to the internal
//! session driver in ingest mode (the same sink the WHIP server uses).

use str0m::{
	Candidate, Rtc,
	change::SdpAnswer,
	media::{Direction, MediaKind},
};
use url::Url;

use crate::{Error, Result, client::Client, ingest::IngestSink, session};

pub(crate) async fn dial(client: &Client, url: Url, broadcast: moq_net::broadcast::Producer) -> Result<()> {
	let config = moq_mux::catalog::Config::default()
		.with_max_age(client.config().max_age)
		.with_bandwidth(client.config().bandwidth.clone());
	let sink = Box::new(IngestSink::new(broadcast, config)?);

	let socket = session::bind_udp(&client.config().ice_candidates).await?;
	let mut rtc = Rtc::new(kio::clock::now().into_std());
	for addr in socket.locals() {
		let cand = Candidate::host(*addr, "udp").map_err(Error::rtc)?;
		rtc.add_local_candidate(cand);
	}

	// Ask for both audio and video, recvonly. The remote answer can decline
	// either by signaling inactive on that m-line.
	let mut api = rtc.sdp_api();
	let mids = [
		api.add_media(MediaKind::Audio, Direction::RecvOnly, None, None, None),
		api.add_media(MediaKind::Video, Direction::RecvOnly, None, None, None),
	];
	let (offer, pending) = api.apply().ok_or(Error::NoSdpChanges)?;

	let res = client
		.http()
		.post(url.clone())
		.header(reqwest::header::CONTENT_TYPE, "application/sdp")
		.header(reqwest::header::ACCEPT, "application/sdp")
		.body(offer.to_sdp_string())
		.send()
		.await?;

	if !res.status().is_success() {
		return Err(Error::HttpStatus(res.status().as_u16()));
	}

	let body = res.text().await?;
	let answer = SdpAnswer::from_sdp_string(&body).map_err(|err| Error::InvalidSdp(err.to_string()))?;

	rtc.sdp_api().accept_answer(pending, answer).map_err(Error::rtc)?;
	// The URL routinely carries the auth token in its query, or userinfo.
	tracing::info!(url = %moq_tokio::RedactedUrl::new(&url), "whep client connected");

	let mut session = session::Session::ingest(rtc, socket, sink)?;
	for mid in mids {
		session.open_media(mid)?;
	}
	session::end("whep client", session.run().await)
}
