//! `client publish`: dial a remote WHIP endpoint, egress a local broadcast.
//!
//! Mints an SDP offer with `sendonly` audio and video sourced from the
//! given [`moq_net::BroadcastConsumer`]. Gated on the per-codec
//! re-packetizer (MoQ frame -> RTP), the same blocker as
//! [`crate::server::whep`].

use url::Url;

use crate::{Error, Result, client::Client};

pub(crate) async fn dial(_client: &Client, _url: Url, _broadcast: moq_net::BroadcastConsumer) -> Result<()> {
	Err(Error::Other(anyhow::anyhow!(
		"client publish (WHIP): re-packetization is not implemented yet. See moq-rtc README."
	)))
}
