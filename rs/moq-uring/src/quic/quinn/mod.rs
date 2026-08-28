//! The quinn backend: sans-IO [`quinn_proto`] on the worker's UDP path.
//!
//! TLS goes through rustls, which is what the rest of this workspace speaks,
//! so a build selecting this backend links one TLS stack instead of two.
//!
//! quinn-proto owns more than quiche does: its [`quinn_proto::Endpoint`] holds
//! the connection-id routing table, mints and retires ids, answers unsupported
//! versions, and buffers half-open handshakes. So [`Endpoint`] here is mostly
//! the socket plumbing around it, and the parts that are ours (the accept
//! backlog, shard steering, the driver task per connection) are the same
//! shapes the quiche backend uses.

mod connection;
mod endpoint;
mod stream;

pub use connection::Connection;
pub use endpoint::Endpoint;
pub use stream::{RecvStream, SendStream};

pub(crate) use connection::{End, Shared};

use std::sync::Arc;

use quinn_proto::crypto::rustls::{QuicClientConfig, QuicServerConfig};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};

use super::{Congestion, Error, Identity, SEGMENT, Transport, client, endpoint::CID_LEN, server};

/// Per-stream flow control credit, matching the quiche backend.
const STREAM_WINDOW: u32 = 4 * 1024 * 1024;
/// Per-connection flow control credit, matching the quiche backend.
const CONNECTION_WINDOW: u32 = 16 * 1024 * 1024;
/// How many datagrams to buffer in each direction, matching the quiche
/// backend's 64.
const DATAGRAM_WINDOW: usize = 64 * SEGMENT;

impl From<quinn_proto::ConnectionError> for Error {
	fn from(err: quinn_proto::ConnectionError) -> Self {
		use quinn_proto::ConnectionError;
		match err {
			ConnectionError::ApplicationClosed(close) => Self::App {
				code: close.error_code.into_inner(),
				reason: String::from_utf8_lossy(&close.reason).into_owned(),
			},
			ConnectionError::ConnectionClosed(close) => Self::Transport {
				code: close.error_code.into(),
				reason: String::from_utf8_lossy(&close.reason).into_owned(),
			},
			ConnectionError::TransportError(err) => Self::Transport {
				code: err.code.into(),
				reason: err.reason.clone(),
			},
			ConnectionError::TimedOut => Self::TimedOut,
			// The rest (a stateless reset, an unsupported version, exhausted
			// ids) have no code to report, and a local close is published
			// where it happens rather than waited for as an event.
			err => Self::Quic(err.to_string()),
		}
	}
}

/// The crypto provider every config here is built from.
///
/// Built explicitly rather than read from the process-wide default, so a
/// consumer that never installed one still gets working TLS.
fn provider() -> Arc<rustls::crypto::CryptoProvider> {
	static PROVIDER: std::sync::OnceLock<Arc<rustls::crypto::CryptoProvider>> = std::sync::OnceLock::new();
	PROVIDER
		.get_or_init(|| Arc::new(rustls::crypto::aws_lc_rs::default_provider()))
		.clone()
}

/// The endpoint-wide configuration: how connection ids are minted, and the
/// largest datagram we tell peers we can receive.
pub(crate) fn endpoint_config(
	shard: Option<moq_sock::shard::Shard>,
) -> Result<Arc<quinn_proto::EndpointConfig>, Error> {
	let mut config = quinn_proto::EndpointConfig::default();
	config.cid_generator(move || Box::new(Cids { shard }));
	config
		.max_udp_payload_size(SEGMENT as u16)
		.map_err(|err| Error::Quic(err.to_string()))?;
	Ok(Arc::new(config))
}

/// Mints the endpoint's connection ids, steering prefix included.
///
/// quinn-proto asks its generator for every id it issues, dials and rotations
/// alike, so this is the one place the reuseport group's byte has to be
/// stamped.
#[derive(Debug)]
struct Cids {
	shard: Option<moq_sock::shard::Shard>,
}

impl quinn_proto::ConnectionIdGenerator for Cids {
	fn generate_cid(&mut self) -> quinn_proto::ConnectionId {
		quinn_proto::ConnectionId::new(&super::endpoint::cid(self.shard))
	}

	fn cid_len(&self) -> usize {
		CID_LEN
	}

	fn cid_lifetime(&self) -> Option<std::time::Duration> {
		None
	}
}

/// Dial as `config` says.
pub(crate) fn client_config(config: &client::Config) -> Result<quinn_proto::ClientConfig, Error> {
	let provider = provider();
	let builder = rustls::ClientConfig::builder_with_provider(provider.clone())
		.with_protocol_versions(&[&rustls::version::TLS13])
		.map_err(|err| Error::Tls(err.to_string()))?;

	// Nothing is checked with verification off, so nothing is loaded: a root
	// path that does not exist must not fail a connection that was never
	// going to look at it.
	let verifier: Arc<dyn rustls::client::danger::ServerCertVerifier> = match (config.verify, config.system_roots) {
		(false, _) => Arc::new(NoVerify(provider.clone())),
		// The platform verifier reads the OS trust store the way the OS means
		// it, so extra roots go through it rather than around it.
		(true, true) if config.roots.is_empty() => Arc::new(
			rustls_platform_verifier::Verifier::new(provider.clone()).map_err(|err| Error::Tls(err.to_string()))?,
		),
		(true, true) => Arc::new(
			rustls_platform_verifier::Verifier::new_with_extra_roots(read_roots(&config.roots)?, provider.clone())
				.map_err(|err| Error::Tls(err.to_string()))?,
		),
		// Trusting only the configured roots means a store built from
		// scratch, never the platform's with ours added on top.
		(true, false) => rustls::client::WebPkiServerVerifier::builder_with_provider(
			Arc::new(root_store(&config.roots)?),
			provider.clone(),
		)
		.build()
		.map_err(|err| Error::Tls(err.to_string()))?,
	};
	let builder = builder.dangerous().with_custom_certificate_verifier(verifier);

	let mut tls = match &config.identity {
		Some(identity) => {
			let (chain, key) = keypair(identity)?;
			builder
				.with_client_auth_cert(chain, key)
				.map_err(|err| Error::Tls(err.to_string()))?
		}
		None => builder.with_no_client_auth(),
	};
	tls.alpn_protocols = alpn(&config.alpn);

	let crypto = QuicClientConfig::try_from(tls).map_err(|err| Error::Tls(err.to_string()))?;
	let mut client = quinn_proto::ClientConfig::new(Arc::new(crypto));
	client.transport_config(transport_config(&config.transport)?);
	Ok(client)
}

/// Serve as `config` says.
pub(crate) fn server_config(config: &server::Config) -> Result<quinn_proto::ServerConfig, Error> {
	config.check()?;
	let provider = provider();
	let builder = rustls::ServerConfig::builder_with_provider(provider.clone())
		.with_protocol_versions(&[&rustls::version::TLS13])
		.map_err(|err| Error::Tls(err.to_string()))?;

	let verifier = match config.client_auth.roots() {
		None => rustls::server::WebPkiClientVerifier::no_client_auth(),
		Some((roots, required)) => {
			// Client certificates chain to the roots configured here, never to
			// the platform store for public sites.
			let builder =
				rustls::server::WebPkiClientVerifier::builder_with_provider(Arc::new(root_store(roots)?), provider);
			let builder = match required {
				true => builder,
				false => builder.allow_unauthenticated(),
			};
			builder.build().map_err(|err| Error::Tls(err.to_string()))?
		}
	};

	let (chain, key) = keypair(&config.identity)?;
	let mut tls = builder
		.with_client_cert_verifier(verifier)
		.with_single_cert(chain, key)
		.map_err(|err| Error::Tls(err.to_string()))?;
	tls.alpn_protocols = alpn(&config.alpn);

	let crypto = QuicServerConfig::try_from(tls).map_err(|err| Error::Tls(err.to_string()))?;
	let mut server = quinn_proto::ServerConfig::with_crypto(Arc::new(crypto));
	server.transport_config(transport_config(&config.transport)?);
	Ok(server)
}

/// The per-connection knobs both roles share.
fn transport_config(config: &Transport) -> Result<Arc<quinn_proto::TransportConfig>, Error> {
	use quinn_proto::VarInt;

	let idle = quinn_proto::IdleTimeout::try_from(config.idle_timeout)
		.map_err(|_| Error::Quic(format!("idle timeout out of range: {:?}", config.idle_timeout)))?;
	let streams = VarInt::from_u64(config.max_streams)
		.map_err(|_| Error::Quic(format!("stream limit out of range: {}", config.max_streams)))?;

	let mut transport = quinn_proto::TransportConfig::default();
	transport.max_idle_timeout(Some(idle));
	transport.keep_alive_interval(config.keep_alive);
	transport.max_concurrent_bidi_streams(streams);
	transport.max_concurrent_uni_streams(streams);
	transport.stream_receive_window(STREAM_WINDOW.into());
	transport.receive_window(CONNECTION_WINDOW.into());
	transport.send_window(CONNECTION_WINDOW.into());
	transport.datagram_receive_buffer_size(Some(DATAGRAM_WINDOW));
	transport.datagram_send_buffer_size(DATAGRAM_WINDOW);
	// Every datagram in a GSO train is one SEGMENT, so the packet size is not
	// quinn's to discover: pin it and turn the probing off.
	transport.initial_mtu(SEGMENT as u16);
	transport.min_mtu(SEGMENT as u16);
	transport.mtu_discovery_config(None);
	transport.congestion_controller_factory(match config.congestion {
		Congestion::Loss => Arc::new(quinn_proto::congestion::CubicConfig::default())
			as Arc<dyn quinn_proto::congestion::ControllerFactory + Send + Sync>,
		// quinn's BBR is v1.
		Congestion::Delay => Arc::new(quinn_proto::congestion::BbrConfig::default()),
	});
	Ok(Arc::new(transport))
}

/// ALPN protocols on the wire, which is a length-prefixed list of byte
/// strings rather than the `String`s a caller configures.
fn alpn(protocols: &[String]) -> Vec<Vec<u8>> {
	protocols.iter().map(|proto| proto.as_bytes().to_vec()).collect()
}

/// Split an [`Identity`] into the chain and key rustls wants.
fn keypair(identity: &Identity) -> Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>), Error> {
	let chain = CertificateDer::pem_slice_iter(identity.cert())
		.collect::<Result<Vec<_>, _>>()
		.map_err(|err| Error::Tls(format!("certificate: {err}")))?;
	if chain.is_empty() {
		return Err(Error::Tls("certificate chain holds no certificates".to_string()));
	}
	let key = PrivateKeyDer::from_pem_slice(identity.key()).map_err(|err| Error::Tls(format!("key: {err}")))?;
	Ok((chain, key))
}

/// Read every PEM certificate in each root file, naming the one that fails.
///
/// A root is routinely a bundle of several CAs, and taking only the first
/// would reject a peer chaining to any of the others while looking configured.
/// A file holding none is an error rather than a store that trusts nothing.
fn read_roots(paths: &[std::path::PathBuf]) -> Result<Vec<CertificateDer<'static>>, Error> {
	let mut roots = Vec::new();
	for path in paths {
		let pem = std::fs::read(path).map_err(|err| Error::Tls(format!("{}: {err}", path.display())))?;
		let certs = CertificateDer::pem_slice_iter(&pem)
			.collect::<Result<Vec<_>, _>>()
			.map_err(|err| Error::Tls(format!("{}: {err}", path.display())))?;
		if certs.is_empty() {
			return Err(Error::Tls(format!("{}: no certificates", path.display())));
		}
		roots.extend(certs);
	}
	Ok(roots)
}

/// A store holding exactly the roots `paths` names, and nothing else.
fn root_store(paths: &[std::path::PathBuf]) -> Result<rustls::RootCertStore, Error> {
	let mut store = rustls::RootCertStore::empty();
	for root in read_roots(paths)? {
		store.add(root).map_err(|err| Error::Tls(err.to_string()))?;
	}
	Ok(store)
}

/// Accepts any server certificate, for
/// [`verify`](client::Config::verify) turned off.
#[derive(Debug)]
struct NoVerify(Arc<rustls::crypto::CryptoProvider>);

impl rustls::client::danger::ServerCertVerifier for NoVerify {
	fn verify_server_cert(
		&self,
		_end_entity: &CertificateDer<'_>,
		_intermediates: &[CertificateDer<'_>],
		_server_name: &rustls::pki_types::ServerName<'_>,
		_ocsp: &[u8],
		_now: rustls::pki_types::UnixTime,
	) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
		Ok(rustls::client::danger::ServerCertVerified::assertion())
	}

	fn verify_tls12_signature(
		&self,
		message: &[u8],
		cert: &CertificateDer<'_>,
		dss: &rustls::DigitallySignedStruct,
	) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
		rustls::crypto::verify_tls12_signature(message, cert, dss, &self.0.signature_verification_algorithms)
	}

	fn verify_tls13_signature(
		&self,
		message: &[u8],
		cert: &CertificateDer<'_>,
		dss: &rustls::DigitallySignedStruct,
	) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
		rustls::crypto::verify_tls13_signature(message, cert, dss, &self.0.signature_verification_algorithms)
	}

	fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
		self.0.signature_verification_algorithms.supported_schemes()
	}
}
