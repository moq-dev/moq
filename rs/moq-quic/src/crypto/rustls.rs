use std::{io, str, sync::Arc};

#[cfg(all(feature = "aws-lc-rs", not(feature = "ring")))]
use aws_lc_rs::aead;
use bytes::BytesMut;
#[cfg(feature = "ring")]
use ring::aead;
use rustls::{
	self,
	client::danger::ServerCertVerifier,
	pki_types::{CertificateDer, PrivateKeyDer, ServerName},
	quic::{Connection, HeaderProtectionKey, KeyChange, PacketKey, Secrets, Suite, Version},
};

use crate::{
	ConnectionId, Side, TransportError, TransportErrorCode,
	crypto::{self, CryptoError, ExportKeyingMaterialError, HandshakeData, HeaderKey, KeyPair, Keys},
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
pub(crate) struct TlsSession {
	version: Version,
	got_handshake_data: bool,
	next_secrets: Option<Secrets>,
	inner: Connection,
	initial: Suite,
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
		initial_keys(self.version, dst_cid, side, &self.initial)
	}

	fn handshake_data(&self) -> Option<HandshakeData> {
		if !self.got_handshake_data {
			return None;
		}
		Some(HandshakeData {
			alpn: self.inner.alpn_protocol().map(|x| x.into()),
			server_name: match &self.inner {
				Connection::Client(_) => None,
				Connection::Server(session) => session.server_name().map(|x| x.into()),
			},
		})
	}

	fn peer_identity(&self) -> Option<Vec<CertificateDer<'static>>> {
		let chain = self.inner.peer_certificates()?;
		Some(chain.iter().map(|cert| cert.clone().into_owned()).collect())
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

/// A rustls client config made safe for QUIC: early data stays off, while the config's
/// resumption cache still enables resumed 1-RTT handshakes.
pub(crate) struct QuicClientConfig {
	pub(crate) inner: Arc<rustls::ClientConfig>,
	initial: Option<Suite>,
}

impl QuicClientConfig {
	/// Wraps a caller's config, copying it only if it enabled early data.
	pub(crate) fn new(mut inner: Arc<rustls::ClientConfig>) -> Self {
		if inner.enable_early_data {
			Arc::make_mut(&mut inner).enable_early_data = false;
		}
		let initial = initial_suite(inner.crypto_provider());
		Self { inner, initial }
	}

	/// A TLS 1.3 config on this crate's provider that verifies the server with `verifier`.
	pub(crate) fn inner(verifier: Arc<dyn ServerCertVerifier>) -> rustls::ClientConfig {
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
		let initial = self.initial.ok_or_else(|| ConnectError::Tls(no_initial_suite()))?;
		let server_name = ServerName::try_from(server_name)
			.map_err(|_| ConnectError::InvalidServerName(server_name.into()))?
			.to_owned();
		let inner = rustls::quic::ClientConnection::new(self.inner.clone(), version, server_name, to_vec(params))
			.map_err(ConnectError::Tls)?;
		Ok(Box::new(TlsSession {
			version,
			got_handshake_data: false,
			next_secrets: None,
			inner: Connection::Client(inner),
			initial,
		}))
	}
}

/// A rustls server config made safe for QUIC: early data stays off, while session tickets
/// still enable resumed 1-RTT handshakes.
pub(crate) struct QuicServerConfig {
	pub(crate) inner: Arc<rustls::ServerConfig>,
	initial: Option<Suite>,
}

impl QuicServerConfig {
	/// Wraps a caller's config, copying it only if it accepted early data.
	pub(crate) fn new(mut inner: Arc<rustls::ServerConfig>) -> Self {
		if inner.max_early_data_size != 0 {
			Arc::make_mut(&mut inner).max_early_data_size = 0;
		}
		let initial = initial_suite(inner.crypto_provider());
		Self { inner, initial }
	}

	/// A TLS 1.3 config on this crate's provider that presents `cert_chain`.
	pub(crate) fn inner(
		cert_chain: Vec<CertificateDer<'static>>,
		key: PrivateKeyDer<'static>,
	) -> Result<rustls::ServerConfig, rustls::Error> {
		rustls::ServerConfig::builder_with_provider(configured_provider())
			.with_protocol_versions(&[&rustls::version::TLS13])
			.unwrap() // The default providers support TLS 1.3
			.with_no_client_auth()
			.with_single_cert(cert_chain, key)
	}
}

impl crypto::ServerConfig for QuicServerConfig {
	fn start_session(
		self: Arc<Self>,
		version: crate::Version,
		params: &TransportParameters,
	) -> Result<Box<dyn crypto::Session>, TransportError> {
		let version = rustls_version(version);
		// The endpoint never starts a session when `initial_keys` found no suite.
		let initial = self.initial.expect("Initial keys came from this config's suite");
		// Only a config that cannot do QUIC at all (no TLS 1.3) fails here.
		let inner = rustls::quic::ServerConnection::new(self.inner.clone(), version, to_vec(params)).map_err(|e| {
			TransportError {
				code: TransportErrorCode::INTERNAL_ERROR,
				frame: None,
				reason: format!("TLS config: {e}"),
				crypto: Some(Arc::new(e)),
			}
		})?;
		Ok(Box::new(TlsSession {
			version,
			got_handshake_data: false,
			next_secrets: None,
			inner: Connection::Server(inner),
			initial,
		}))
	}

	fn initial_keys(&self, version: crate::Version, dst_cid: ConnectionId) -> Option<Keys> {
		Some(initial_keys(
			rustls_version(version),
			dst_cid,
			Side::Server,
			self.initial.as_ref()?,
		))
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

/// The suite that protects Initial packets, which RFC 9001 fixes as AES-128-GCM-SHA256.
///
/// Taken from the config's own provider, so a restricted provider (such as a FIPS module)
/// protects Initial packets too. `None` when the provider lacks the suite: the config then
/// fails each connection rather than borrowing another provider's.
pub(crate) fn initial_suite(provider: &rustls::crypto::CryptoProvider) -> Option<Suite> {
	provider
		.cipher_suites
		.iter()
		.find(|suite| suite.suite() == rustls::CipherSuite::TLS13_AES_128_GCM_SHA256)?
		.tls13()?
		.quic_suite()
}

fn no_initial_suite() -> rustls::Error {
	rustls::Error::General("the provider lacks TLS13_AES_128_GCM_SHA256, which QUIC Initial packets need".into())
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

	/// A ticket issued with early data still resumes once the wrappers disable it, and a
	/// caller's shared configs retain their original policy.
	#[test]
	fn supplied_configs_reject_early_data_and_keep_resumption() {
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
		let wrapped_server = QuicServerConfig::new(server.clone());
		assert_eq!(server.max_early_data_size, u32::MAX);
		// A third-party client holds an older ticket and really offers early data.
		assert_eq!(
			handshake(client.clone(), wrapped_server.inner.clone()),
			(rustls::HandshakeKind::Resumed, true, false)
		);
		let wrapped_client = QuicClientConfig::new(client.clone());
		assert!(client.enable_early_data);
		assert_eq!(
			handshake(wrapped_client.inner, wrapped_server.inner),
			(rustls::HandshakeKind::Resumed, false, false)
		);
	}

	/// A provider without the Initial suite fails each connection instead of borrowing
	/// another provider's suite.
	#[test]
	fn provider_without_initial_suite_fails_connections() {
		let mut provider = (*configured_provider()).clone();
		provider
			.cipher_suites
			.retain(|suite| suite.suite() != rustls::CipherSuite::TLS13_AES_128_GCM_SHA256);
		let provider = Arc::new(provider);
		let identity = rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();

		let client = rustls::ClientConfig::builder_with_provider(provider.clone())
			.with_protocol_versions(&[&rustls::version::TLS13])
			.unwrap()
			.with_root_certificates(rustls::RootCertStore::empty())
			.with_no_client_auth();
		let client = Arc::new(QuicClientConfig::new(Arc::new(client)));
		let params = TransportParameters::default();
		let err = crypto::ClientConfig::start_session(client, crate::Version::V1, "localhost", &params).err();
		assert!(matches!(err, Some(ConnectError::Tls(_))));

		let server = rustls::ServerConfig::builder_with_provider(provider)
			.with_protocol_versions(&[&rustls::version::TLS13])
			.unwrap()
			.with_no_client_auth()
			.with_single_cert(
				vec![identity.cert.der().clone()],
				PrivateKeyDer::Pkcs8(identity.signing_key.serialize_der().into()),
			)
			.unwrap();
		let server = QuicServerConfig::new(Arc::new(server));
		let dcid = ConnectionId::new(&[1; 8]);
		assert!(crypto::ServerConfig::initial_keys(&server, crate::Version::V1, dcid).is_none());
	}
}
