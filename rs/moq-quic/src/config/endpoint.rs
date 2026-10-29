use std::{fmt, sync::Arc};

use crate::{
	ConfigError, Duration, INITIAL_MTU, MAX_UDP_PAYLOAD, VarInt, cid_generator::ConnectionIdGenerator, crypto::HmacKey,
	server,
};

/// Builds the connection ID generator an [`Endpoint`](crate::endpoint::Endpoint) issues IDs from
pub type CidGenerator = Arc<dyn Fn() -> Box<dyn ConnectionIdGenerator> + Send + Sync>;

/// Global configuration for the endpoint, affecting all connections
///
/// Default values should be suitable for most internet applications.
#[derive(Clone)]
#[non_exhaustive]
pub struct Config {
	/// Accept incoming connections with this configuration, or `None` (the default) only to dial
	pub server: Option<server::Config>,

	/// The random 32-byte secret the stateless reset key is derived from, or `None` (the
	/// default) for a random key per endpoint
	///
	/// A restarted endpoint given the same secret resets its predecessor's connections instead of
	/// leaving peers to wait out their idle timeout. Every endpoint holding the secret resets every
	/// other holder's connections, so RFC 9000 section 21.11 requires that packets for a live
	/// connection only ever reach its owner: share it across live endpoints only behind one
	/// address that routes by connection ID.
	///
	/// Also keys the default connection ID generator, since an endpoint drops packets for
	/// connection IDs it can't validate instead of resetting them.
	pub reset_secret: Option<[u8; 32]>,

	/// Builds the generator for the connection IDs this endpoint issues, or `None` (the default)
	/// for an authenticated one keyed by [`reset_secret`](Self::reset_secret)
	///
	/// Called once per endpoint. A custom generator lets an application embed information in its
	/// connection IDs, e.g. to support stateless packet-level load balancers. With a reset secret,
	/// it must accept a predecessor's IDs for the reset to reach them.
	pub cid_generator: Option<CidGenerator>,

	/// Maximum UDP payload size accepted from peers, excluding UDP and IP overhead
	///
	/// Must be between 1200 and 65527. Defaults to 1472, which is the largest UDP payload that
	/// can be transmitted in the typical 1500 byte Ethernet MTU. Deployments on links with larger
	/// MTUs (e.g. loopback or Ethernet with jumbo frames) can raise this to improve performance at
	/// the cost of a linear increase in datagram receive buffer size.
	pub max_udp_payload_size: u16,

	/// Whether to accept QUIC packets containing any value for the fixed bit
	///
	/// Enabled by default. Helps protect against protocol ossification and makes traffic less
	/// identifiable to observers. Disable if helping observers identify this traffic as QUIC is
	/// desired.
	pub grease_quic_bit: bool,

	/// Minimum interval between outgoing stateless reset packets, or `None` for the default
	///
	/// Defaults to 20ms, or 1ms with a [`reset_secret`](Self::reset_secret), since a restart
	/// leaves every peer waiting on a reset at once: 1000 a second resets 2,000 connections in 2s,
	/// where 20ms takes 40s. A reset is always smaller than the packet that drew it, so this never
	/// amplifies.
	///
	/// Limits the impact of attacks which flood an endpoint with garbage packets, e.g.
	/// [ISAKMP/IKE amplification]. Larger values provide a stronger defense, but may delay
	/// detection of some error conditions by clients. Using a [`ConnectionIdGenerator`] with a low
	/// rate of false positives in [`validate`](ConnectionIdGenerator::validate) reduces the risk
	/// incurred by a small minimum reset interval.
	///
	/// [ISAKMP/IKE amplification]: https://bughunters.google.com/blog/5960150648750080/preventing-cross-service-udp-loops-in-quic#isakmp-ike-amplification-vs-quic
	pub min_reset_interval: Option<Duration>,

	/// Seed for the endpoint's random number generator, or `None` (the default) to seed it from
	/// the platform's entropy source
	///
	/// Set it to run the transport deterministically, or where no entropy source is available.
	pub rng_seed: Option<[u8; 32]>,
}

impl Default for Config {
	fn default() -> Self {
		Self {
			server: None,
			reset_secret: None,
			cid_generator: None,
			max_udp_payload_size: 1500 - 28, // Ethernet MTU minus IP + UDP headers
			grease_quic_bit: true,
			min_reset_interval: None,
			rng_seed: None,
		}
	}
}

impl Config {
	/// Accept incoming connections with `server`, on top of dialing
	pub fn with_server(mut self, server: server::Config) -> Self {
		self.server = Some(server);
		self
	}

	/// Refuse what a peer would reject or the state machine cannot honor
	///
	/// Endpoints call this on every config they are handed, so call it only to fail earlier.
	pub fn validate(&self) -> Result<(), ConfigError> {
		if !(INITIAL_MTU..=MAX_UDP_PAYLOAD).contains(&self.max_udp_payload_size) {
			return Err(ConfigError::MaxUdpPayloadSize);
		}
		if let Some(server) = &self.server {
			server.transport.validate()?;
		}
		Ok(())
	}

	/// The settings an endpoint runs on, with every default filled in, and its connection ID
	/// generator
	#[cfg(any(feature = "aws-lc-rs", feature = "ring"))]
	pub(crate) fn resolve(&self) -> Result<(Resolved, Box<dyn ConnectionIdGenerator>), ConfigError> {
		#[cfg(all(feature = "aws-lc-rs", not(feature = "ring")))]
		use aws_lc_rs::{hkdf, hmac};
		use rand::Rng;
		#[cfg(feature = "ring")]
		use ring::{hkdf, hmac};

		use crate::cid_generator::HashedConnectionIdGenerator;

		struct Len(usize);
		impl hkdf::KeyType for Len {
			fn len(&self) -> usize {
				self.0
			}
		}

		self.validate()?;

		let (reset_key, cid_key, min_reset_interval): (Arc<dyn HmacKey>, _, _) = match &self.reset_secret {
			Some(secret) => {
				let prk = hkdf::Salt::new(hkdf::HKDF_SHA256, &[]).extract(secret);
				let reset_key = prk
					.expand(&[b"moq-quic reset key"], hmac::HMAC_SHA256)
					.expect("an HMAC key fits HKDF-SHA256");

				let mut cid_key = [0; 8];
				prk.expand(&[b"moq-quic cid key"], Len(cid_key.len()))
					.and_then(|okm| okm.fill(&mut cid_key))
					.expect("8 bytes fit HKDF-SHA256");
				(
					Arc::new(hmac::Key::from(reset_key)),
					Some(u64::from_le_bytes(cid_key)),
					Duration::from_millis(1),
				)
			}
			None => {
				let mut reset_key = [0; 64];
				crate::entropy::rng().fill_bytes(&mut reset_key);
				(
					Arc::new(hmac::Key::new(hmac::HMAC_SHA256, &reset_key)),
					None,
					Duration::from_millis(20),
				)
			}
		};

		let cid_generator = match &self.cid_generator {
			Some(generator) => generator(),
			None => match cid_key {
				Some(key) => Box::new(HashedConnectionIdGenerator::from_key(key)),
				None => Box::<HashedConnectionIdGenerator>::default(),
			},
		};

		let resolved = Resolved {
			reset_key,
			max_udp_payload_size: self.max_udp_payload_size.into(),
			grease_quic_bit: self.grease_quic_bit,
			min_reset_interval: self.min_reset_interval.unwrap_or(min_reset_interval),
		};
		Ok((resolved, cid_generator))
	}
}

impl fmt::Debug for Config {
	fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
		fmt.debug_struct("Config")
			.field("server", &self.server)
			// Never logged.
			.field("reset_secret", &self.reset_secret.map(|_| "[redacted]"))
			.field("cid_generator", &self.cid_generator.is_some())
			.field("max_udp_payload_size", &self.max_udp_payload_size)
			.field("grease_quic_bit", &self.grease_quic_bit)
			.field("min_reset_interval", &self.min_reset_interval)
			.field("rng_seed", &self.rng_seed.map(|_| "[redacted]"))
			.finish()
	}
}

/// What an [`Endpoint`](crate::endpoint::Endpoint) runs on, resolved from a [`Config`]
pub(crate) struct Resolved {
	pub(crate) reset_key: Arc<dyn HmacKey>,
	pub(crate) max_udp_payload_size: VarInt,
	pub(crate) grease_quic_bit: bool,
	pub(crate) min_reset_interval: Duration,
}
