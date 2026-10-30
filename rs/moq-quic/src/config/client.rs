//! Dialing outgoing connections

use std::{fmt, sync::Arc};

#[cfg(any(feature = "aws-lc-rs", feature = "ring"))]
use rustls::client::WebPkiServerVerifier;

#[cfg(any(feature = "aws-lc-rs", feature = "ring"))]
use crate::crypto::rustls::{QuicClientConfig, configured_provider};
use crate::{
	MAX_CID_SIZE, RandomConnectionIdGenerator, TokenMemoryCache, TokenStore, Version,
	cid_generator::ConnectionIdGenerator, crypto, shared::ConnectionId, transport,
};

/// Configuration for outgoing connections
///
/// Default values should be suitable for most internet applications.
#[derive(Clone)]
#[non_exhaustive]
pub struct Config {
	/// Transport configuration to use
	pub transport: transport::Config,

	/// Cryptographic configuration to use
	pub(crate) crypto: Arc<dyn crypto::ClientConfig>,

	/// QUIC protocol version to use
	pub version: Version,

	/// Validation token store to use
	pub(crate) token_store: Arc<dyn TokenStore>,

	/// Provider that populates the destination connection ID of Initial Packets
	pub(crate) initial_dst_cid_provider: Arc<dyn Fn() -> ConnectionId + Send + Sync>,
}

impl Config {
	/// Dial with any crypto, such as the fuzz target's plaintext
	pub(crate) fn from_crypto(crypto: Arc<dyn crypto::ClientConfig>) -> Self {
		Self {
			transport: transport::Config::default(),
			crypto,
			version: Version::V1,
			token_store: Arc::new(TokenMemoryCache::default()),
			initial_dst_cid_provider: Arc::new(|| RandomConnectionIdGenerator::new(MAX_CID_SIZE).generate_cid()),
		}
	}
}

#[cfg(any(feature = "aws-lc-rs", feature = "ring"))]
impl Config {
	/// Run the handshake with `tls` and the default settings
	///
	/// The config must enable TLS 1.3, or connecting fails with [`ConnectError::Tls`].
	/// Early data stays off: if `tls` enables it, the connection uses a copy that does not.
	///
	/// [`ConnectError::Tls`]: crate::endpoint::ConnectError::Tls
	pub fn new(tls: Arc<rustls::ClientConfig>) -> Self {
		Self::from_crypto(Arc::new(QuicClientConfig::new(tls)))
	}

	/// Dial trusting the specified trust anchors
	pub fn with_root_certificates(
		roots: Arc<rustls::RootCertStore>,
	) -> Result<Self, rustls::client::VerifierBuilderError> {
		let verifier = WebPkiServerVerifier::builder_with_provider(roots, configured_provider()).build()?;
		Ok(Self::new(Arc::new(QuicClientConfig::inner(verifier))))
	}
}

impl fmt::Debug for Config {
	fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
		fmt.debug_struct("Config")
			.field("transport", &self.transport)
			// crypto not debug
			// token_store not debug
			.field("version", &self.version)
			.finish_non_exhaustive()
	}
}
