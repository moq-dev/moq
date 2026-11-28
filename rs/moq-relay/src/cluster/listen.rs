//! The cluster's own QUIC listener, apart from the public `--listen`.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// A QUIC listener for cluster peers alone: every client must present a
/// certificate chaining to [`root`](Self::root), so the public listener need
/// ask no client for one.
#[serde_with::serde_as]
#[derive(usage_rs::Args, Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
#[non_exhaustive]
pub struct Config {
	/// Listen for cluster peers over QUIC (UDP) on this address, apart from
	/// `--listen`. A client presenting no certificate that chains to
	/// `--cluster-listen-root` is refused at the TLS handshake. Peers reach it
	/// by naming its port in the URL they dial. It shares `--listen`'s versions,
	/// handshake timeout, and reset secret, and has no TCP or WebSocket
	/// counterpart.
	#[usage(
		name = "cluster-listen",
		long = "cluster-listen",
		env = "MOQ_CLUSTER_LISTEN",
		setting = "cluster.listen.bind"
	)]
	#[serde(skip_serializing_if = "Option::is_none")]
	pub bind: Option<moq_tokio::listen::Bind>,

	/// Certificate(s) the cluster listener serves, selected by SNI like `--listen-tls-cert`.
	#[usage(
		name = "cluster-listen-cert",
		long = "cluster-listen-cert",
		delimiter = ',',
		env = "MOQ_CLUSTER_LISTEN_CERT",
		setting = "cluster.listen.cert"
	)]
	#[usage(value_hint = usage_rs::ValueHint::FilePath, extensions("pem", "crt", "cer"))]
	#[serde(skip_serializing_if = "Vec::is_empty")]
	#[serde_as(as = "serde_with::OneOrMany<_>")]
	pub cert: Vec<PathBuf>,

	/// The private key for each `--cluster-listen-cert`, in the same order.
	#[usage(
		name = "cluster-listen-key",
		long = "cluster-listen-key",
		delimiter = ',',
		env = "MOQ_CLUSTER_LISTEN_KEY",
		setting = "cluster.listen.key"
	)]
	#[usage(value_hint = usage_rs::ValueHint::FilePath, extensions("pem", "key"))]
	#[serde(skip_serializing_if = "Vec::is_empty")]
	#[serde_as(as = "serde_with::OneOrMany<_>")]
	pub key: Vec<PathBuf>,

	/// PEM root CA(s) every client of the cluster listener must chain to,
	/// reloaded for new handshakes when the files change.
	#[usage(
		name = "cluster-listen-root",
		long = "cluster-listen-root",
		delimiter = ',',
		env = "MOQ_CLUSTER_LISTEN_ROOT",
		setting = "cluster.listen.root"
	)]
	#[usage(value_hint = usage_rs::ValueHint::FilePath, extensions("pem", "crt", "cer"))]
	#[serde(skip_serializing_if = "Vec::is_empty")]
	#[serde_as(as = "serde_with::OneOrMany<_>")]
	pub root: Vec<PathBuf>,
}

impl Config {
	/// Refuse a listener that would serve no identity or admit a client
	/// without a certificate, and identity settings with no listener to use them.
	pub(crate) fn check(&self) -> anyhow::Result<()> {
		if self.bind.is_none() {
			anyhow::ensure!(
				self.cert.is_empty() && self.key.is_empty() && self.root.is_empty(),
				"--cluster-listen-cert, --cluster-listen-key, and --cluster-listen-root need --cluster-listen"
			);
			return Ok(());
		}
		anyhow::ensure!(
			!self.cert.is_empty() && !self.key.is_empty(),
			"--cluster-listen needs --cluster-listen-cert and --cluster-listen-key"
		);
		anyhow::ensure!(
			!self.root.is_empty(),
			"--cluster-listen needs --cluster-listen-root: it refuses every client without a certificate"
		);
		Ok(())
	}

	/// The cluster listener's accept side, or `None` when unset: `public`'s
	/// versions, timeout, and reset secret, with this address and identity and
	/// none of its stream listeners, preferred addresses, or QUIC-LB encoding.
	pub(crate) fn server(&self, public: &moq_tokio::listen::Config) -> Option<moq_tokio::listen::Config> {
		let bind = self.bind.clone()?;
		let mut listen = public.clone();
		listen.bind = Some(bind);
		listen.tcp = Default::default();
		#[cfg(unix)]
		{
			listen.unix = Default::default();
		}
		listen.preferred_v4 = None;
		listen.preferred_v6 = None;
		listen.lb = Default::default();
		let mut tls = moq_tokio::tls::Listen::default();
		tls.cert = self.cert.clone();
		tls.key = self.key.clone();
		tls.root = self.root.clone();
		tls.require = true;
		listen.tls = tls;
		Some(listen)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn listen() -> Config {
		Config {
			bind: Some("127.0.0.1:0".parse().unwrap()),
			cert: vec!["cluster.crt".into()],
			key: vec!["cluster.key".into()],
			root: vec!["ca.pem".into()],
		}
	}

	/// Each half-configured listener is refused, naming what it lacks.
	#[test]
	fn check_refuses_a_partial_listener() {
		listen().check().unwrap();
		Config::default().check().unwrap();

		let refuse = |config: Config, expected: &str| {
			let error = config.check().expect_err(expected).to_string();
			assert!(error.contains(expected), "{error}");
		};
		let mut config = listen();
		config.root.clear();
		refuse(config, "needs --cluster-listen-root");
		let mut config = listen();
		config.cert.clear();
		refuse(config, "needs --cluster-listen-cert");
		let mut config = listen();
		config.key.clear();
		refuse(config, "needs --cluster-listen-cert");
		let mut config = listen();
		config.bind = None;
		refuse(config, "need --cluster-listen");
	}

	/// The listener requires a certificate and drops what belongs to the public port.
	#[test]
	fn server_requires_a_certificate_and_drops_the_public_extras() {
		let mut public = moq_tokio::listen::Config::default();
		public.bind = Some("[::]:443".parse().unwrap());
		public.tcp.bind = Some("127.0.0.1:0".parse().unwrap());
		public.tls.cert = vec!["public.crt".into()];
		public.tls.root = vec!["public-ca.pem".into()];
		public.preferred_v4 = Some("192.0.2.1:443".parse().unwrap());
		public.version = vec!["moq-lite-06".parse().unwrap()];

		assert!(Config::default().server(&public).is_none());
		let server = listen().server(&public).unwrap();
		assert_eq!(server.bind, listen().bind);
		assert_eq!(server.tcp.bind, None);
		assert_eq!(server.preferred_v4, None);
		assert_eq!(server.version, public.version, "versions follow --listen");
		assert!(server.tls.require);
		assert_eq!(server.tls.cert, listen().cert);
		assert_eq!(server.tls.root, listen().root);
	}
}
