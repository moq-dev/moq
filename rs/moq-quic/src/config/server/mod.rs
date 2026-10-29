//! Accepting incoming connections

use std::{
	fmt,
	net::{SocketAddrV4, SocketAddrV6},
	sync::Arc,
};

#[cfg(any(feature = "aws-lc-rs", feature = "ring"))]
use rustls::pki_types::{CertificateDer, PrivateKeyDer};

#[cfg(any(feature = "aws-lc-rs", feature = "ring"))]
use crate::crypto::rustls::QuicServerConfig;
use crate::{
	Duration,
	crypto::{self, HandshakeTokenKey},
	transport,
};

pub mod validation_token;

/// Parameters governing incoming connections
///
/// Default values should be suitable for most internet applications.
#[derive(Clone)]
#[non_exhaustive]
pub struct Config {
	/// Transport configuration to use for incoming connections
	pub transport: transport::Config,

	/// TLS configuration used for incoming connections
	///
	/// Must be set to use TLS 1.3 only.
	pub crypto: Arc<dyn crypto::ServerConfig>,

	/// Configuration for sending and handling validation tokens
	pub validation_token: validation_token::Config,

	/// Duration after a retry token was issued for which it's considered valid, 15 seconds by
	/// default
	pub retry_token_lifetime: Duration,

	/// Whether to allow clients to migrate to new addresses
	///
	/// Improves behavior for clients that move between different internet connections or suffer NAT
	/// rebinding. Enabled by default.
	pub migration: bool,

	/// The preferred IPv4 address that will be communicated to clients during handshaking
	///
	/// If the client is able to reach this address, it will switch to it.
	pub preferred_address_v4: Option<SocketAddrV4>,

	/// The preferred IPv6 address that will be communicated to clients during handshaking
	///
	/// If the client is able to reach this address, it will switch to it.
	pub preferred_address_v6: Option<SocketAddrV6>,

	/// Maximum number of incoming connection attempts to hold before they become active
	///
	/// An attempt counts toward this limit from the time its initial packet is received until it is
	/// either registered as an active connection or otherwise disposed of. While this limit is
	/// reached, new incoming connection attempts are not admitted. Larger values have greater
	/// worst-case memory consumption, but accommodate greater application latency in handling
	/// incoming connection attempts.
	///
	/// The default value is set to 65536. With a typical Ethernet MTU of 1500 bytes, this limits
	/// memory consumption from this to under 100 MiB--a generous amount that still prevents memory
	/// exhaustion in most contexts.
	pub max_incoming: usize,

	/// Maximum number of received bytes to buffer for each incoming connection attempt
	///
	/// This limit governs packets received after the first packet and before the attempt is either
	/// registered as an active connection or otherwise disposed of. Packets received in excess of
	/// this limit are dropped, which may require handshake data to be retransmitted.
	///
	/// The default value is 10 MiB. Early-data packets are discarded.
	pub incoming_buffer_size: u64,

	/// Maximum number of received bytes to buffer collectively for all incoming connection attempts
	///
	/// This limit governs packets received after each attempt's first packet and before the attempt
	/// is either registered as an active connection or otherwise disposed of. Packets received in
	/// excess of this limit are dropped, which may require handshake data to be retransmitted.
	///
	/// The default value is set to 100 MiB--a generous amount that still prevents memory
	/// exhaustion in most contexts.
	pub incoming_buffer_size_total: u64,

	/// Used to generate one-time AEAD keys to protect handshake tokens
	pub(crate) token_key: Arc<dyn HandshakeTokenKey>,
}

#[cfg(any(feature = "aws-lc-rs", feature = "ring"))]
impl Config {
	/// Serve `crypto` with the default settings and a random handshake token key
	pub fn new(crypto: Arc<dyn crypto::ServerConfig>) -> Self {
		#[cfg(all(feature = "aws-lc-rs", not(feature = "ring")))]
		use aws_lc_rs::hkdf;
		use rand::Rng;
		#[cfg(feature = "ring")]
		use ring::hkdf;

		let rng = &mut crate::entropy::rng();
		let mut master_key = [0u8; 64];
		rng.fill_bytes(&mut master_key);
		let token_key = hkdf::Salt::new(hkdf::HKDF_SHA256, &[]).extract(&master_key);

		Self {
			transport: transport::Config::default(),
			crypto,
			validation_token: validation_token::Config::default(),
			retry_token_lifetime: Duration::from_secs(15),
			migration: true,
			preferred_address_v4: None,
			preferred_address_v6: None,
			max_incoming: 1 << 16,
			incoming_buffer_size: 10 << 20,
			incoming_buffer_size_total: 100 << 20,
			token_key: Arc::new(token_key),
		}
	}

	/// Present `cert_chain`, signed for by `key`, with the default settings
	pub fn with_single_cert(
		cert_chain: Vec<CertificateDer<'static>>,
		key: PrivateKeyDer<'static>,
	) -> Result<Self, rustls::Error> {
		Ok(Self::new(Arc::new(QuicServerConfig::new(cert_chain, key)?)))
	}
}

impl Config {
	pub(crate) fn has_preferred_address(&self) -> bool {
		self.preferred_address_v4.is_some() || self.preferred_address_v6.is_some()
	}
}

impl fmt::Debug for Config {
	fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
		fmt.debug_struct("Config")
			.field("transport", &self.transport)
			// crypto not debug
			// token not debug
			.field("retry_token_lifetime", &self.retry_token_lifetime)
			.field("validation_token", &self.validation_token)
			.field("migration", &self.migration)
			.field("preferred_address_v4", &self.preferred_address_v4)
			.field("preferred_address_v6", &self.preferred_address_v6)
			.field("max_incoming", &self.max_incoming)
			.field("incoming_buffer_size", &self.incoming_buffer_size)
			.field("incoming_buffer_size_total", &self.incoming_buffer_size_total)
			.finish_non_exhaustive()
	}
}
