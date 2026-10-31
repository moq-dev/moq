//! HTTP-client side: dial a remote WHIP/WHEP endpoint over an SDP exchange.
//!
//! Counterpart to [`crate::server`]. Whereas the server accepts POSTed
//! offers, the client mints the offer with `str0m::Rtc::sdp_api` and POSTs
//! it to the remote URL. Once the answer arrives the same internal session
//! driver takes over, so the per-codec bridges and UDP socket loop are shared.

mod whep;
mod whip;

use std::net::SocketAddr;
use std::time::Duration;

use url::Url;

/// Configuration shared by both `client publish` and `client subscribe`.
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct Config {
	/// Public UDP socket addresses to advertise as ICE host candidates in
	/// our outbound offer. Same semantics as [`crate::server::Config::ice_candidates`].
	pub ice_candidates: Vec<SocketAddr>,

	/// How long relays keep a non-latest group of an ingested media track fetchable.
	/// Same semantics as [`crate::server::Config::max_age`].
	///
	/// Ingest only ([`subscribe`](Client::subscribe) / WHEP): a WHIP
	/// [`publish`](Client::publish) reads a broadcast someone else declared, so it
	/// ignores this.
	pub max_age: Option<Duration>,

	/// Connection allocator each ingested track claims its peak-hold bitrate on.
	/// Ingest only ([`subscribe`](Client::subscribe) / WHEP); a WHIP
	/// [`publish`](Client::publish) ignores this.
	pub bandwidth: moq_net::bandwidth::Allocator,
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
	/// `broadcast` on the local origin, until the session ends.
	pub async fn subscribe(&self, url: Url, broadcast: moq_net::broadcast::Producer) -> crate::Result<()> {
		whep::dial(self, url, broadcast).await
	}

	/// `client publish`: push the broadcast `source` resolved to a remote WHIP endpoint,
	/// until the session ends.
	///
	/// It stays on that publisher instance, and fails once the instance's route dies.
	/// The source's origin resolves a rendition whose catalog `broadcast` field references
	/// a sibling broadcast.
	pub async fn publish(&self, url: Url, source: moq_mux::Source) -> crate::Result<()> {
		whip::dial(self, url, source).await
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[tokio::test]
	async fn http_error_redacts_url() {
		// A freed port refuses the connection, so reqwest fails with the dialed URL attached.
		let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
		let address = listener.local_addr().expect("local address");
		drop(listener);

		let origin = moq_tokio::origin::spawn();
		let broadcast = origin.create_broadcast("test").expect("create broadcast");
		let url = format!("http://user:pass@{address}/whep/test?jwt=secret")
			.parse()
			.expect("WHEP URL");
		let err = Client::new(Config::default())
			.subscribe(url, broadcast.broadcast().clone())
			.await
			.expect_err("dial a closed port");

		assert!(matches!(err, crate::Error::Http(_)), "unexpected error: {err}");
		let printed = format!("{err} {err:?}");
		for secret in ["jwt", "secret", "user:pass"] {
			assert!(!printed.contains(secret), "error leaked {secret}: {printed}");
		}
	}
}
