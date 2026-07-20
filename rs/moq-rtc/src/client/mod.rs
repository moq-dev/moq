//! HTTP-client side: dial a remote WHIP/WHEP endpoint over an SDP exchange.
//!
//! Counterpart to [`crate::server`]. Whereas the server accepts POSTed
//! offers, the client mints the offer with `str0m::Rtc::sdp_api` and POSTs
//! it to the remote URL. Once the answer arrives the same internal session
//! driver takes over, so the per-codec bridges and UDP socket loop are shared.

mod whep;
mod whip;

use std::net::SocketAddr;

use url::Url;

/// Configuration shared by both `client publish` and `client subscribe`.
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct Config {
	/// Public UDP socket addresses to advertise as ICE host candidates in
	/// our outbound offer. Same semantics as [`crate::server::Config::ice_candidates`].
	pub ice_candidates: Vec<SocketAddr>,
}

/// Outbound WHIP/WHEP dialer.
///
/// Owns a [`reqwest::Client`] reused across calls so connection pooling and
/// rustls config survive between resources.
#[derive(Clone)]
pub struct Client {
	config: Config,
	http: reqwest::Client,
}

impl Client {
	/// Build a dialer from the shared client [`Config`]. The underlying
	/// [`reqwest::Client`] (with its connection pool and rustls config) is created
	/// once here and reused across every [`subscribe`](Self::subscribe) /
	/// [`publish`](Self::publish) call.
	pub fn new(config: Config) -> Self {
		Self {
			config,
			http: reqwest::Client::new(),
		}
	}

	pub(crate) fn config(&self) -> &Config {
		&self.config
	}

	pub(crate) fn http(&self) -> &reqwest::Client {
		&self.http
	}

	/// `client subscribe`: pull a remote WHEP feed and publish it as
	/// `broadcast` on the local origin. Returns once the session is
	/// running in the background.
	pub async fn subscribe(&self, url: Url, broadcast: moq_net::broadcast::Producer) -> crate::Result<()> {
		whep::dial(self, url, broadcast).await
	}

	/// `client publish`: pull the broadcast at `path` from `origin` and push it to a
	/// remote WHIP endpoint. Gated on the per-codec re-packetizer.
	///
	/// Taking the origin plus path (rather than a resolved [`moq_net::broadcast::Consumer`])
	/// lets the egress resolve a rendition whose catalog `broadcast` field references a
	/// sibling broadcast, against the same origin.
	pub async fn publish(
		&self,
		url: Url,
		origin: moq_net::origin::Consumer,
		path: impl moq_net::AsPath,
	) -> crate::Result<()> {
		whip::dial(self, url, origin, path).await
	}
}
