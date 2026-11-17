//! HTTP-client side: dial a remote WHIP/WHEP endpoint over an SDP exchange.
//!
//! Counterpart to [`crate::server`]. Whereas the server accepts POSTed
//! offers, the client mints the offer with `str0m::Rtc::sdp_api` and POSTs
//! it to the remote URL. Once the answer arrives the same internal session
//! driver takes over, so the per-codec bridges and UDP socket loop are shared.

mod whep;
mod whip;

use url::Url;

/// Configuration shared by both `client publish` and `client pull`.
///
/// No options yet; it is where a later one, such as a dial policy, goes without
/// changing [`Client::new`].
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct Config {}

/// Outbound WHIP/WHEP dialer.
///
/// Owns a [`reqwest::Client`] reused across calls so connection pooling and
/// rustls config survive between resources.
#[derive(Clone)]
pub struct Client {
	http: reqwest::Client,
}

impl Client {
	/// Build a dialer from the shared client [`Config`]. The underlying
	/// [`reqwest::Client`] (with its connection pool and rustls config) is created
	/// once here and reused across every [`pull`](Self::pull) /
	/// [`publish`](Self::publish) call.
	///
	/// Fails with [`Error::UnsupportedCpu`](crate::Error::UnsupportedCpu) on a CPU
	/// rind can't run on.
	pub fn new(config: Config) -> crate::Result<Self> {
		// Destructured so a new option cannot compile without being used.
		let Config {} = config;
		// reqwest reads rustls's process default and panics without one, so install rind's
		// unless the host already chose a provider.
		moq_crypto::provider::install_default()?;
		Ok(Self {
			http: reqwest::Client::builder().build()?,
		})
	}

	pub(crate) fn http(&self) -> &reqwest::Client {
		&self.http
	}

	/// `client pull`: ingest a remote WHEP feed into `broadcast`, publishing its renditions
	/// on `catalog`, until the session ends.
	///
	/// The catalog gets its first update once the session negotiates a track. The caller owns
	/// the broadcast, so its announcement and any linger after the end are the caller's too.
	/// Any end, clean or not, aborts the catalog and every track with its cause.
	pub async fn pull(
		&self,
		url: Url,
		broadcast: &moq_net::broadcast::Producer,
		catalog: &moq_mux::catalog::Producer,
	) -> crate::Result<()> {
		let result = whep::dial(self, url, broadcast.clone(), catalog.clone()).await;
		// A dial that fails before its session runs leaves the catalog open; a session already
		// aborted it with its cause, which this keeps.
		if let Err(err) = &result {
			catalog.clone().abort(moq_net::Error::Transport(err.to_string()));
		}
		result
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

		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast, Default::default()).unwrap();
		let url = format!("http://user:pass@{address}/whep/test?jwt=secret")
			.parse()
			.expect("WHEP URL");
		let err = Client::new(Config::default())
			.unwrap()
			.pull(url, &broadcast, &catalog)
			.await
			.expect_err("dial a closed port");

		assert!(matches!(err, crate::Error::Http(_)), "unexpected error: {err}");
		assert!(
			catalog.consume().unwrap().next().await.is_err(),
			"the failed dial aborts the catalog"
		);
		let printed = format!("{err} {err:?}");
		for secret in ["jwt", "secret", "user:pass"] {
			assert!(!printed.contains(secret), "error leaked {secret}: {printed}");
		}
	}
}
