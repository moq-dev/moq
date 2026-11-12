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
	///
	/// Fails with [`Error::UnsupportedCpu`](crate::Error::UnsupportedCpu) on a CPU
	/// rind can't run on.
	pub fn new(config: Config) -> crate::Result<Self> {
		// reqwest reads rustls's process default and panics without one, so install rind's
		// unless the host already chose a provider.
		moq_crypto::provider::install_default()?;
		Ok(Self {
			config,
			http: reqwest::Client::builder().build()?,
		})
	}

	pub(crate) fn config(&self) -> &Config {
		&self.config
	}

	pub(crate) fn http(&self) -> &reqwest::Client {
		&self.http
	}

	/// `client subscribe`: pull a remote WHEP feed and publish it as `broadcast`, its tracks
	/// minted under `catalog`, until the session ends.
	///
	/// The caller owns the broadcast, so its announcement and any linger after the end are
	/// the caller's too.
	pub async fn subscribe(
		&self,
		url: Url,
		broadcast: moq_net::broadcast::Producer,
		catalog: moq_mux::catalog::Config,
	) -> crate::Result<()> {
		whep::dial(self, url, broadcast, catalog).await
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
		let broadcast = origin
			.route("test", moq_net::origin::RouteOptions::hidden())
			.expect("create broadcast");
		let url = format!("http://user:pass@{address}/whep/test?jwt=secret")
			.parse()
			.expect("WHEP URL");
		let err = Client::new(Config::default())
			.unwrap()
			.subscribe(url, broadcast.broadcast().clone(), Default::default())
			.await
			.expect_err("dial a closed port");

		assert!(matches!(err, crate::Error::Http(_)), "unexpected error: {err}");
		let printed = format!("{err} {err:?}");
		for secret in ["jwt", "secret", "user:pass"] {
			assert!(!printed.contains(secret), "error leaked {secret}: {printed}");
		}
	}
}
