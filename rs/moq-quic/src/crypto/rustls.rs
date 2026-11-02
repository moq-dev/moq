use std::{any::Any, io, str, sync::Arc};

#[cfg(all(feature = "aws-lc-rs", not(feature = "ring")))]
use aws_lc_rs::aead;
use bytes::BytesMut;
#[cfg(feature = "ring")]
use ring::aead;
pub use rustls::Error;
#[cfg(feature = "__rustls-post-quantum-test")]
use rustls::NamedGroup;
use rustls::{
	self, CipherSuite,
	client::danger::ServerCertVerifier,
	pki_types::{CertificateDer, PrivateKeyDer, ServerName},
	quic::{Connection, HeaderProtectionKey, KeyChange, PacketKey, Secrets, Suite, Version},
};

use crate::{
	ConnectionId, Side, TransportError, TransportErrorCode,
	crypto::{self, CryptoError, ExportKeyingMaterialError, HeaderKey, KeyPair, Keys},
	endpoint::ConnectError,
	transport_parameters::TransportParameters,
};

impl From<Side> for rustls::Side {
	fn from(s: Side) -> Self {
		match s {
			Side::Client => Self::Client,
			Side::Server => Self::Server,
		}
	}
}

/// A rustls TLS session
pub struct TlsSession {
	version: Version,
	got_handshake_data: bool,
	next_secrets: Option<Secrets>,
	inner: Connection,
	suite: Suite,
}

impl TlsSession {
	fn side(&self) -> Side {
		match self.inner {
			Connection::Client(_) => Side::Client,
			Connection::Server(_) => Side::Server,
		}
	}
}

impl crypto::Session for TlsSession {
	fn initial_keys(&self, dst_cid: ConnectionId, side: Side) -> Keys {
		initial_keys(self.version, dst_cid, side, &self.suite)
	}

	fn handshake_data(&self) -> Option<Box<dyn Any>> {
		if !self.got_handshake_data {
			return None;
		}
		Some(Box::new(HandshakeData {
			protocol: self.inner.alpn_protocol().map(|x| x.into()),
			server_name: match &self.inner {
				Connection::Client(_) => None,
				Connection::Server(session) => session.server_name().map(|x| x.into()),
			},
			protocol_version: match &self.inner {
				Connection::Client(session) => session.protocol_version(),
				Connection::Server(session) => session.protocol_version(),
			}
			.map(|x| -> Box<dyn Any> { Box::new(x) }),
			cipher_suite: match &self.inner {
				Connection::Client(session) => session.negotiated_cipher_suite(),
				Connection::Server(session) => session.negotiated_cipher_suite(),
			}
			.map(|suite| -> Box<dyn Any> { Box::new(suite.suite()) }),
			#[cfg(feature = "__rustls-post-quantum-test")]
			negotiated_key_exchange_group: self
				.inner
				.negotiated_key_exchange_group()
				.expect("key exchange group is negotiated")
				.name(),
		}))
	}

	/// For the rustls `TlsSession`, the `Any` type is `Vec<rustls::pki_types::CertificateDer>`
	fn peer_identity(&self) -> Option<Box<dyn Any>> {
		self.inner.peer_certificates().map(|v| -> Box<dyn Any> {
			Box::new(
				v.iter()
					.map(|v| v.clone().into_owned())
					.collect::<Vec<CertificateDer<'static>>>(),
			)
		})
	}

	fn is_handshaking(&self) -> bool {
		self.inner.is_handshaking()
	}

	fn read_handshake(&mut self, buf: &[u8]) -> Result<bool, TransportError> {
		self.inner.read_hs(buf).map_err(|e| {
			if let Some(alert) = self.inner.alert() {
				TransportError {
					code: TransportErrorCode::crypto(alert.into()),
					frame: None,
					reason: e.to_string(),
					crypto: Some(Arc::new(e)),
				}
			} else {
				TransportError::PROTOCOL_VIOLATION(format!("TLS error: {e}"))
			}
		})?;
		if !self.got_handshake_data {
			// Hack around the lack of an explicit signal from rustls to reflect ClientHello being
			// ready on incoming connections, or ALPN negotiation completing on outgoing
			// connections.
			let have_server_name = match &self.inner {
				Connection::Client(_) => false,
				Connection::Server(session) => session.server_name().is_some(),
			};
			if self.inner.alpn_protocol().is_some() || have_server_name || !self.is_handshaking() {
				self.got_handshake_data = true;
				return Ok(true);
			}
		}
		Ok(false)
	}

	fn transport_parameters(&self) -> Result<Option<TransportParameters>, TransportError> {
		match self.inner.quic_transport_parameters() {
			None => Ok(None),
			Some(buf) => match TransportParameters::read(self.side(), &mut io::Cursor::new(buf)) {
				Ok(params) => Ok(Some(params)),
				Err(e) => Err(e.into()),
			},
		}
	}

	fn write_handshake(&mut self, buf: &mut Vec<u8>) -> Option<Keys> {
		let keys = match self.inner.write_hs(buf)? {
			KeyChange::Handshake { keys } => keys,
			KeyChange::OneRtt { keys, next } => {
				self.next_secrets = Some(next);
				keys
			}
		};

		Some(Keys {
			header: KeyPair {
				local: Box::new(keys.local.header),
				remote: Box::new(keys.remote.header),
			},
			packet: KeyPair {
				local: Box::new(keys.local.packet),
				remote: Box::new(keys.remote.packet),
			},
		})
	}

	fn next_1rtt_keys(&mut self) -> Option<KeyPair<Box<dyn crypto::PacketKey>>> {
		let secrets = self.next_secrets.as_mut()?;
		let keys = secrets.next_packet_keys();
		Some(KeyPair {
			local: Box::new(keys.local),
			remote: Box::new(keys.remote),
		})
	}

	fn is_valid_retry(&self, orig_dst_cid: ConnectionId, header: &[u8], payload: &[u8]) -> bool {
		let Some(tag_start) = payload.len().checked_sub(16) else {
			return false;
		};

		let mut pseudo_packet = Vec::with_capacity(header.len() + payload.len() + orig_dst_cid.len() + 1);
		pseudo_packet.push(orig_dst_cid.len() as u8);
		pseudo_packet.extend_from_slice(&orig_dst_cid);
		pseudo_packet.extend_from_slice(header);
		let tag_start = tag_start + pseudo_packet.len();
		pseudo_packet.extend_from_slice(payload);

		let (nonce, key) = match self.version {
			Version::V1 => (RETRY_INTEGRITY_NONCE_V1, RETRY_INTEGRITY_KEY_V1),
			_ => unreachable!(),
		};

		let nonce = aead::Nonce::assume_unique_for_key(nonce);
		let key = aead::LessSafeKey::new(aead::UnboundKey::new(&aead::AES_128_GCM, &key).unwrap());

		let (aad, tag) = pseudo_packet.split_at_mut(tag_start);
		key.open_in_place(nonce, aead::Aad::from(aad), tag).is_ok()
	}

	fn export_keying_material(
		&self,
		output: &mut [u8],
		label: &[u8],
		context: &[u8],
	) -> Result<(), ExportKeyingMaterialError> {
		self.inner
			.export_keying_material(output, label, Some(context))
			.map_err(|_| ExportKeyingMaterialError)?;
		Ok(())
	}
}

const RETRY_INTEGRITY_KEY_V1: [u8; 16] = [
	0xbe, 0x0c, 0x69, 0x0b, 0x9f, 0x66, 0x57, 0x5a, 0x1d, 0x76, 0x6b, 0x54, 0xe3, 0x68, 0xc8, 0x4e,
];
const RETRY_INTEGRITY_NONCE_V1: [u8; 12] = [0x46, 0x15, 0x99, 0xd3, 0x5d, 0x63, 0x2b, 0xf2, 0x23, 0x98, 0x25, 0xbb];

impl HeaderKey for Box<dyn HeaderProtectionKey> {
	fn decrypt(&self, pn_offset: usize, packet: &mut [u8]) {
		let (header, sample) = packet.split_at_mut(pn_offset + 4);
		let (first, rest) = header.split_at_mut(1);
		let pn_end = Ord::min(pn_offset + 3, rest.len());
		self.decrypt_in_place(
			&sample[..self.sample_size()],
			&mut first[0],
			&mut rest[pn_offset - 1..pn_end],
		)
		.unwrap();
	}

	fn encrypt(&self, pn_offset: usize, packet: &mut [u8]) {
		let (header, sample) = packet.split_at_mut(pn_offset + 4);
		let (first, rest) = header.split_at_mut(1);
		let pn_end = Ord::min(pn_offset + 3, rest.len());
		self.encrypt_in_place(
			&sample[..self.sample_size()],
			&mut first[0],
			&mut rest[pn_offset - 1..pn_end],
		)
		.unwrap();
	}

	fn sample_size(&self) -> usize {
		self.sample_len()
	}
}

/// Authentication data for (rustls) TLS session
#[non_exhaustive]
pub struct HandshakeData {
	/// The negotiated application protocol, if ALPN is in use
	///
	/// Guaranteed to be set if a nonempty list of protocols was specified for this connection.
	pub protocol: Option<Vec<u8>>,
	/// The server name specified by the client, if any
	///
	/// Always `None` for outgoing connections
	pub server_name: Option<String>,
	/// The protocol version negotiated with the peer, if any
	pub protocol_version: Option<Box<dyn Any>>,
	/// The cipher suite negotiated with the peer, if any
	pub cipher_suite: Option<Box<dyn Any>>,
	/// The key exchange group negotiated with the peer
	#[cfg(feature = "__rustls-post-quantum-test")]
	pub negotiated_key_exchange_group: NamedGroup,
}

/// A QUIC-compatible TLS client configuration
///
/// [`client::Config::with_root_certificates()`][root_certs] constructs a QUIC-compatible TLS policy.
/// Alternatively, `QuicClientConfig`'s [`TryFrom`] implementation can be used to wrap around a
/// custom [`rustls::ClientConfig`], in which case care should be taken around certain points:
///
/// TLS 1.3 is required. Early data is disabled, while the inner config's resumption
/// cache still enables resumed 1-RTT handshakes.
///
/// [root_certs]: crate::client::Config::with_root_certificates()
pub struct QuicClientConfig {
	pub(crate) inner: Arc<rustls::ClientConfig>,
	initial: Suite,
}

impl QuicClientConfig {
	/// Initialize a sane QUIC-compatible TLS client configuration
	///
	/// QUIC requires that TLS 1.3 be enabled. Advanced users can use any [`rustls::ClientConfig`] that
	/// satisfies this requirement.
	pub(crate) fn new(verifier: Arc<dyn ServerCertVerifier>) -> Self {
		let inner = Self::inner(verifier);
		Self {
			// We're confident that the *ring* default provider contains TLS13_AES_128_GCM_SHA256
			initial: initial_suite_from_provider(inner.crypto_provider()).expect("no initial cipher suite found"),
			inner: Arc::new(inner),
		}
	}

	/// Initialize a QUIC-compatible TLS client configuration with a separate initial cipher suite
	///
	/// This is useful if you want to avoid the initial cipher suite for traffic encryption.
	pub fn with_initial(mut inner: Arc<rustls::ClientConfig>, initial: Suite) -> Result<Self, NoInitialCipherSuite> {
		if inner.enable_early_data {
			Arc::make_mut(&mut inner).enable_early_data = false;
		}
		match initial.suite.common.suite {
			CipherSuite::TLS13_AES_128_GCM_SHA256 => Ok(Self { inner, initial }),
			_ => Err(NoInitialCipherSuite { specific: true }),
		}
	}

	pub(crate) fn inner(verifier: Arc<dyn ServerCertVerifier>) -> rustls::ClientConfig {
		// Keep in sync with `with_platform_verifier()` above
		rustls::ClientConfig::builder_with_provider(configured_provider())
			.with_protocol_versions(&[&rustls::version::TLS13])
			.unwrap() // The default providers support TLS 1.3
			.dangerous()
			.with_custom_certificate_verifier(verifier)
			.with_no_client_auth()
	}
}

impl crypto::ClientConfig for QuicClientConfig {
	fn start_session(
		self: Arc<Self>,
		version: crate::Version,
		server_name: &str,
		params: &TransportParameters,
	) -> Result<Box<dyn crypto::Session>, ConnectError> {
		let version = rustls_version(version);
		Ok(Box::new(TlsSession {
			version,
			got_handshake_data: false,
			next_secrets: None,
			inner: Connection::Client(
				rustls::quic::ClientConnection::new(
					self.inner.clone(),
					version,
					ServerName::try_from(server_name)
						.map_err(|_| ConnectError::InvalidServerName(server_name.into()))?
						.to_owned(),
					to_vec(params),
				)
				.unwrap(),
			),
			suite: self.initial,
		}))
	}
}

impl TryFrom<rustls::ClientConfig> for QuicClientConfig {
	type Error = NoInitialCipherSuite;

	fn try_from(inner: rustls::ClientConfig) -> Result<Self, Self::Error> {
		Arc::new(inner).try_into()
	}
}

impl TryFrom<Arc<rustls::ClientConfig>> for QuicClientConfig {
	type Error = NoInitialCipherSuite;

	fn try_from(mut inner: Arc<rustls::ClientConfig>) -> Result<Self, Self::Error> {
		if inner.enable_early_data {
			Arc::make_mut(&mut inner).enable_early_data = false;
		}
		Ok(Self {
			initial: initial_suite_from_provider(inner.crypto_provider())
				.ok_or(NoInitialCipherSuite { specific: false })?,
			inner,
		})
	}
}

/// The initial cipher suite (AES-128-GCM-SHA256) is not available
///
/// When the cipher suite is supplied `with_initial()`, it must be
/// [`CipherSuite::TLS13_AES_128_GCM_SHA256`]. When the cipher suite is derived from a config's
/// [`CryptoProvider`][provider], that provider must reference a cipher suite with the same ID.
///
/// [provider]: rustls::crypto::CryptoProvider
#[derive(Clone, Debug)]
pub struct NoInitialCipherSuite {
	/// Whether the initial cipher suite was supplied by the caller
	specific: bool,
}

impl std::fmt::Display for NoInitialCipherSuite {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.write_str(match self.specific {
			true => "invalid cipher suite specified",
			false => "no initial cipher suite found",
		})
	}
}

impl std::error::Error for NoInitialCipherSuite {}

/// A QUIC-compatible TLS server configuration
///
/// The transport constructs a `QuicServerConfig` with reasonable defaults within
/// [`server::Config::with_single_cert()`][single]. Alternatively, `QuicServerConfig`'s [`TryFrom`]
/// implementation or `with_initial` method can be used to wrap around a custom
/// [`rustls::ServerConfig`], in which case care should be taken around certain points:
///
/// TLS 1.3 is required. Early data is disabled, while session tickets still enable
/// resumed 1-RTT handshakes.
///
/// [single]: crate::server::Config::with_single_cert()
pub struct QuicServerConfig {
	inner: Arc<rustls::ServerConfig>,
	initial: Suite,
}

impl QuicServerConfig {
	pub(crate) fn new(cert_chain: Vec<CertificateDer<'static>>, key: PrivateKeyDer<'static>) -> Result<Self, Error> {
		let inner = Self::inner(cert_chain, key)?;
		Ok(Self {
			// We're confident that the *ring* default provider contains TLS13_AES_128_GCM_SHA256
			initial: initial_suite_from_provider(inner.crypto_provider()).expect("no initial cipher suite found"),
			inner: Arc::new(inner),
		})
	}

	/// Initialize a QUIC-compatible TLS client configuration with a separate initial cipher suite
	///
	/// This is useful if you want to avoid the initial cipher suite for traffic encryption.
	pub fn with_initial(mut inner: Arc<rustls::ServerConfig>, initial: Suite) -> Result<Self, NoInitialCipherSuite> {
		if inner.max_early_data_size != 0 {
			Arc::make_mut(&mut inner).max_early_data_size = 0;
		}
		match initial.suite.common.suite {
			CipherSuite::TLS13_AES_128_GCM_SHA256 => Ok(Self { inner, initial }),
			_ => Err(NoInitialCipherSuite { specific: true }),
		}
	}

	/// Initialize a sane QUIC-compatible TLS server configuration
	///
	/// TLS 1.3 is required and early data stays disabled.
	pub(crate) fn inner(
		cert_chain: Vec<CertificateDer<'static>>,
		key: PrivateKeyDer<'static>,
	) -> Result<rustls::ServerConfig, Error> {
		let inner = rustls::ServerConfig::builder_with_provider(configured_provider())
			.with_protocol_versions(&[&rustls::version::TLS13])
			.unwrap() // The *ring* default provider supports TLS 1.3
			.with_no_client_auth()
			.with_single_cert(cert_chain, key)?;

		Ok(inner)
	}
}

impl TryFrom<rustls::ServerConfig> for QuicServerConfig {
	type Error = NoInitialCipherSuite;

	fn try_from(inner: rustls::ServerConfig) -> Result<Self, Self::Error> {
		Arc::new(inner).try_into()
	}
}

impl TryFrom<Arc<rustls::ServerConfig>> for QuicServerConfig {
	type Error = NoInitialCipherSuite;

	fn try_from(mut inner: Arc<rustls::ServerConfig>) -> Result<Self, Self::Error> {
		if inner.max_early_data_size != 0 {
			Arc::make_mut(&mut inner).max_early_data_size = 0;
		}
		Ok(Self {
			initial: initial_suite_from_provider(inner.crypto_provider())
				.ok_or(NoInitialCipherSuite { specific: false })?,
			inner,
		})
	}
}

impl crypto::ServerConfig for QuicServerConfig {
	fn start_session(
		self: Arc<Self>,
		version: crate::Version,
		params: &TransportParameters,
	) -> Box<dyn crypto::Session> {
		let version = rustls_version(version);
		Box::new(TlsSession {
			version,
			got_handshake_data: false,
			next_secrets: None,
			inner: Connection::Server(
				rustls::quic::ServerConnection::new(self.inner.clone(), version, to_vec(params)).unwrap(),
			),
			suite: self.initial,
		})
	}

	fn initial_keys(&self, version: crate::Version, dst_cid: ConnectionId) -> Keys {
		initial_keys(rustls_version(version), dst_cid, Side::Server, &self.initial)
	}

	fn retry_tag(&self, version: crate::Version, orig_dst_cid: ConnectionId, packet: &[u8]) -> [u8; 16] {
		let (nonce, key) = match rustls_version(version) {
			Version::V1 => (RETRY_INTEGRITY_NONCE_V1, RETRY_INTEGRITY_KEY_V1),
			_ => unreachable!(),
		};

		let mut pseudo_packet = Vec::with_capacity(packet.len() + orig_dst_cid.len() + 1);
		pseudo_packet.push(orig_dst_cid.len() as u8);
		pseudo_packet.extend_from_slice(&orig_dst_cid);
		pseudo_packet.extend_from_slice(packet);

		let nonce = aead::Nonce::assume_unique_for_key(nonce);
		let key = aead::LessSafeKey::new(aead::UnboundKey::new(&aead::AES_128_GCM, &key).unwrap());

		let tag = key
			.seal_in_place_separate_tag(nonce, aead::Aad::from(pseudo_packet), &mut [])
			.unwrap();
		let mut result = [0; 16];
		result.copy_from_slice(tag.as_ref());
		result
	}
}

pub(crate) fn initial_suite_from_provider(provider: &Arc<rustls::crypto::CryptoProvider>) -> Option<Suite> {
	provider
		.cipher_suites
		.iter()
		.find_map(|cs| match (cs.suite(), cs.tls13()) {
			(CipherSuite::TLS13_AES_128_GCM_SHA256, Some(suite)) => Some(suite.quic_suite()),
			_ => None,
		})
		.flatten()
}

pub(crate) fn configured_provider() -> Arc<rustls::crypto::CryptoProvider> {
	#[cfg(all(feature = "aws-lc-rs", not(feature = "ring")))]
	let provider = rustls::crypto::aws_lc_rs::default_provider();
	#[cfg(feature = "ring")]
	let provider = rustls::crypto::ring::default_provider();
	Arc::new(provider)
}

fn to_vec(params: &TransportParameters) -> Vec<u8> {
	let mut bytes = Vec::new();
	params.write(&mut bytes);
	bytes
}

pub(crate) fn initial_keys(version: Version, dst_cid: ConnectionId, side: Side, suite: &Suite) -> Keys {
	let keys = suite.keys(&dst_cid, side.into(), version);
	Keys {
		header: KeyPair {
			local: Box::new(keys.local.header),
			remote: Box::new(keys.remote.header),
		},
		packet: KeyPair {
			local: Box::new(keys.local.packet),
			remote: Box::new(keys.remote.packet),
		},
	}
}

impl crypto::PacketKey for Box<dyn PacketKey> {
	fn encrypt(&self, packet: u64, buf: &mut [u8], header_len: usize) {
		let (header, payload_tag) = buf.split_at_mut(header_len);
		let (payload, tag_storage) = payload_tag.split_at_mut(payload_tag.len() - self.tag_len());
		let tag = self.encrypt_in_place(packet, &*header, payload).unwrap();
		tag_storage.copy_from_slice(tag.as_ref());
	}

	fn decrypt(&self, packet: u64, header: &[u8], payload: &mut BytesMut) -> Result<(), CryptoError> {
		let plain = self
			.decrypt_in_place(packet, header, payload.as_mut())
			.map_err(|_| CryptoError)?;
		let plain_len = plain.len();
		payload.truncate(plain_len);
		Ok(())
	}

	fn tag_len(&self) -> usize {
		(**self).tag_len()
	}

	fn confidentiality_limit(&self) -> u64 {
		(**self).confidentiality_limit()
	}

	fn integrity_limit(&self) -> u64 {
		(**self).integrity_limit()
	}
}

fn rustls_version(version: crate::Version) -> Version {
	match version {
		crate::Version::V1 => Version::V1,
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn handshake(
		client: Arc<rustls::ClientConfig>,
		server: Arc<rustls::ServerConfig>,
	) -> (rustls::HandshakeKind, bool, bool) {
		let mut client = rustls::quic::ClientConnection::new(
			client,
			Version::V1,
			ServerName::try_from("localhost").unwrap(),
			Vec::new(),
		)
		.unwrap();
		let mut server = rustls::quic::ServerConnection::new(server, Version::V1, Vec::new()).unwrap();
		let early_keys = client.zero_rtt_keys().is_some();
		loop {
			let mut to_server = Vec::new();
			let _ = client.write_hs(&mut to_server);
			if !to_server.is_empty() {
				server.read_hs(&to_server).unwrap();
			}
			let mut to_client = Vec::new();
			let _ = server.write_hs(&mut to_client);
			if !to_client.is_empty() {
				client.read_hs(&to_client).unwrap();
			}
			if to_server.is_empty() && to_client.is_empty() {
				break;
			}
		}
		assert!(!client.is_handshaking());
		assert!(!server.is_handshaking());
		(
			client.handshake_kind().unwrap(),
			early_keys,
			client.is_early_data_accepted(),
		)
	}

	/// A ticket issued with early data still resumes after either public config entry point
	/// disables it, and a caller's shared configs retain their original policy.
	#[test]
	fn supplied_configs_reject_early_data_and_keep_resumption() {
		for explicit_suite in [false, true] {
			let identity = rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
			let cert = identity.cert.der().clone();
			let mut roots = rustls::RootCertStore::empty();
			roots.add(cert.clone()).unwrap();
			let mut client = rustls::ClientConfig::builder_with_provider(configured_provider())
				.with_protocol_versions(&[&rustls::version::TLS13])
				.unwrap()
				.with_root_certificates(roots)
				.with_no_client_auth();
			client.enable_early_data = true;
			let mut server = QuicServerConfig::inner(
				vec![cert],
				PrivateKeyDer::Pkcs8(identity.signing_key.serialize_der().into()),
			)
			.unwrap();
			server.max_early_data_size = u32::MAX;
			let client = Arc::new(client);
			let server = Arc::new(server);
			assert_eq!(
				handshake(client.clone(), server.clone()),
				(rustls::HandshakeKind::Full, false, false)
			);
			let wrapped_server = if explicit_suite {
				QuicServerConfig::with_initial(
					server.clone(),
					initial_suite_from_provider(server.crypto_provider()).unwrap(),
				)
				.unwrap()
			} else {
				QuicServerConfig::try_from(server.clone()).unwrap()
			};
			assert_eq!(server.max_early_data_size, u32::MAX);
			// A third-party client holds an older ticket and really offers early data.
			assert_eq!(
				handshake(client.clone(), wrapped_server.inner.clone()),
				(rustls::HandshakeKind::Resumed, true, false)
			);
			let wrapped_client = if explicit_suite {
				QuicClientConfig::with_initial(
					client.clone(),
					initial_suite_from_provider(client.crypto_provider()).unwrap(),
				)
				.unwrap()
			} else {
				QuicClientConfig::try_from(client.clone()).unwrap()
			};
			assert!(client.enable_early_data);
			assert_eq!(
				handshake(wrapped_client.inner, wrapped_server.inner),
				(rustls::HandshakeKind::Resumed, false, false)
			);
		}
	}
}
