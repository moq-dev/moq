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
	pub crypto: Arc<dyn crypto::ClientConfig>,

	/// QUIC protocol version to use
	pub version: Version,

	/// Validation token store to use
	pub(crate) token_store: Arc<dyn TokenStore>,

	/// Provider that populates the destination connection ID of Initial Packets
	pub(crate) initial_dst_cid_provider: Arc<dyn Fn() -> ConnectionId + Send + Sync>,
}

impl Config {
	/// Dial with `crypto` and the default settings
	pub fn new(crypto: Arc<dyn crypto::ClientConfig>) -> Self {
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
	/// Dial trusting the specified trust anchors
	pub fn with_root_certificates(
		roots: Arc<rustls::RootCertStore>,
	) -> Result<Self, rustls::client::VerifierBuilderError> {
		Ok(Self::new(Arc::new(QuicClientConfig::new(
			WebPkiServerVerifier::builder_with_provider(roots, configured_provider()).build()?,
		))))
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
