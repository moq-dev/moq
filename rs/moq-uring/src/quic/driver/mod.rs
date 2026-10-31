//! The rustls-backed moq-quic stack on the worker's UDP path.
//!
//! TLS goes through rustls, which is what the rest of this workspace speaks,
//! so a build selecting this backend links one TLS stack instead of two.
//!
//! [`moq_quic::endpoint::Endpoint`] holds the connection-id routing table, mints and
//! retires ids, answers unsupported versions, and buffers half-open
//! handshakes. So [`Endpoint`] here is mostly the socket plumbing around it,
//! plus the parts that are ours: the accept backlog, shard steering, and the
//! driver task per connection.

mod connection;
mod endpoint;
pub mod recv;
pub mod send;

pub use connection::Connection;
pub use endpoint::Endpoint;

pub(crate) use connection::Shared;

use std::{num::NonZeroU64, sync::Arc};

use moq_quic::crypto::rustls::{QuicClientConfig, QuicServerConfig};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};

#[cfg(feature = "qlog")]
use super::qlog;
use super::{Congestion, Error, Identity, client, server, transport};
use crate::udp;

/// Streams of each kind a peer may have open at once.
const MAX_STREAMS: u64 = 1024;
/// Per-stream flow control credit, which returns as bytes are read, so it only paces a stream.
const STREAM_WINDOW: u32 = 4 * 1024 * 1024;
/// Per-connection flow control credit, which also bounds incomplete messages, so it has to fit
/// the largest one: `MIN_RECEIVE_WINDOW` is the 64 MiB announcement cap plus framing. 128 MiB
/// also holds two maximum-size frames at once.
const CONNECTION_WINDOW: u32 = 128 * 1024 * 1024;
const _: () = assert!(CONNECTION_WINDOW as u64 >= moq_net::transport::MIN_RECEIVE_WINDOW);
/// Unacknowledged outgoing data to buffer, whatever the peer allows.
const SEND_WINDOW: u32 = 16 * 1024 * 1024;
/// Datagram bytes to buffer in each direction, about 64 full-size datagrams.
const DATAGRAM_WINDOW: usize = 96 * 1024;

fn ecn_to_quic(ecn: udp::Ecn) -> moq_quic::EcnCodepoint {
	match ecn {
		udp::Ecn::Ect0 => moq_quic::EcnCodepoint::Ect0,
		udp::Ecn::Ect1 => moq_quic::EcnCodepoint::Ect1,
		udp::Ecn::Ce => moq_quic::EcnCodepoint::Ce,
	}
}

fn ecn_from_quic(ecn: moq_quic::EcnCodepoint) -> udp::Ecn {
	match ecn {
		moq_quic::EcnCodepoint::Ect0 => udp::Ecn::Ect0,
		moq_quic::EcnCodepoint::Ect1 => udp::Ecn::Ect1,
		moq_quic::EcnCodepoint::Ce => udp::Ecn::Ce,
	}
}

impl From<moq_quic::connection::Error> for Error {
	fn from(err: moq_quic::connection::Error) -> Self {
		use moq_quic::connection::Error as ConnectionError;
		match err {
			ConnectionError::ApplicationClosed(close) => Self::App {
				code: close.error_code.into_inner(),
				reason: close.reason,
			},
			// A local close reports the code it sent, as the peer sees it.
			ConnectionError::LocallyClosed { code, reason } => Self::App {
				code: code.into_inner(),
				reason,
			},
			ConnectionError::ConnectionClosed(close) => Self::Transport {
				code: close.error_code.into(),
				reason: close.reason,
			},
			ConnectionError::TransportError(err) => Self::Transport {
				code: err.code.into(),
				reason: err.reason,
			},
			ConnectionError::TimedOut | ConnectionError::HandshakeTimeout => Self::TimedOut,
			// The rest (a stateless reset, an unsupported version, exhausted
			// ids) have no code to report.
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

/// The endpoint-wide configuration: how connection ids are minted, the reset key, and what to
/// serve.
pub(crate) fn endpoint_config(
	shard: Option<moq_sock::shard::Shard>,
	reset_secret: Option<[u8; 32]>,
	server: Option<moq_quic::server::Config>,
) -> moq_quic::endpoint::Config {
	let mut config = moq_quic::endpoint::Config::default();
	config.server = server;
	// The random ids below validate a predecessor's too, so the reset reaches them.
	config.reset_secret = reset_secret;
	// The steering filter recognizes short headers by their fixed bit.
	config.grease_quic_bit = shard.is_none();
	// moq-quic asks its generator for every id it issues, dials and rotations
	// alike, so naming the shard here steers all of them. Random rather than
	// hashed ids, so neither path gives up nonce bytes to a signature.
	config.cid_generator = Some(Arc::new(move || {
		let mut cids = moq_quic::RandomConnectionIdGenerator::default();
		if let Some(shard) = shard {
			cids.set_shard(shard.index(), shard.count());
		}
		Box::new(cids)
	}));
	config
}

/// Dial as `config` says.
pub(crate) fn client_config(config: &client::Config) -> Result<moq_quic::client::Config, Error> {
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
	let mut client = moq_quic::client::Config::new(Arc::new(crypto));
	client.transport = transport_config(&config.transport)?;
	#[cfg(feature = "qlog")]
	with_qlog(&mut client.transport, &config.transport, qlog::Side::Client);
	Ok(client)
}

/// Serve as `config` says.
pub(crate) fn server_config(config: &server::Config) -> Result<moq_quic::server::Config, Error> {
	let crypto = QuicServerConfig::try_from(config.tls.clone()).map_err(|err| Error::Tls(err.to_string()))?;
	let mut server = moq_quic::server::Config::new(Arc::new(crypto));
	server.transport = transport_config(&config.transport)?;
	#[cfg(feature = "qlog")]
	with_qlog(&mut server.transport, &config.transport, qlog::Side::Server);
	Ok(server)
}

/// A TLS 1.3 policy presenting `identity`, speaking `alpn`, and checking clients as `auth` says.
pub(crate) fn identity_server_config(
	identity: &Identity,
	alpn: Vec<Vec<u8>>,
	auth: &server::ClientAuth,
) -> Result<Arc<rustls::ServerConfig>, Error> {
	let provider = provider();
	let builder = rustls::ServerConfig::builder_with_provider(provider.clone())
		.with_protocol_versions(&[&rustls::version::TLS13])
		.map_err(|err| Error::Tls(err.to_string()))?;

	let verifier = match auth.roots() {
		None => rustls::server::WebPkiClientVerifier::no_client_auth(),
		// An empty store rejects every chain, so asking for a certificate with
		// nothing to check it against refuses exactly the clients that obey.
		Some(([], _)) => {
			return Err(Error::Tls(
				"client authentication needs at least one root certificate".to_string(),
			));
		}
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

	let (chain, key) = keypair(identity)?;
	let mut tls = builder
		.with_client_cert_verifier(verifier)
		.with_single_cert(chain, key)
		.map_err(|err| Error::Tls(err.to_string()))?;
	tls.alpn_protocols = alpn;
	Ok(Arc::new(tls))
}

/// Attach the configured qlog sink, if any.
///
/// moq-quic holds one sink per transport config, which is per endpoint on the
/// accept side and per dial on the connect side; its events carry the
/// connection's qlog `group_id` either way.
#[cfg(feature = "qlog")]
fn with_qlog(transport: &mut moq_quic::transport::Config, config: &transport::Config, side: qlog::Side) {
	let Some(sink) = config.qlog.clone() else {
		return;
	};

	let mut qlog = moq_quic::transport::qlog::Config::default();
	qlog.writer = Some(sink.trace(side));
	transport.qlog = qlog.into_stream();
}

/// The per-connection knobs both roles share.
fn transport_config(config: &transport::Config) -> Result<moq_quic::transport::Config, Error> {
	let mut transport = moq_quic::transport::Config::default();
	transport.idle_timeout = Some(config.idle_timeout);
	transport.keep_alive = config.keep_alive;
	// Our defaults where the caller set none, rather than moq-quic's; the rate caps pass through.
	transport.limits = config.limits;
	let limits = &mut transport.limits;
	limits.max_streams = limits.max_streams.or(NonZeroU64::new(MAX_STREAMS));
	limits.receive_window = limits.receive_window.or(NonZeroU64::new(CONNECTION_WINDOW.into()));
	limits.stream_receive_window = limits.stream_receive_window.or(NonZeroU64::new(STREAM_WINDOW.into()));
	limits.send_window = limits.send_window.or(NonZeroU64::new(SEND_WINDOW.into()));
	transport.datagram.receive_buffer = Some(DATAGRAM_WINDOW);
	transport.datagram.send_buffer = DATAGRAM_WINDOW;
	// moq-quic probes by default; the 1200-byte floor reaches every path.
	if !config.mtu_discovery {
		transport.mtu = None;
	}
	transport.congestion = match config.congestion {
		Congestion::Loss => {
			Arc::new(moq_congestion::CubicConfig::default()) as Arc<dyn moq_congestion::ControllerFactory + Send + Sync>
		}
		Congestion::Delay => Arc::new(moq_congestion::Bbr3Config::default()),
		Congestion::None => Arc::new(moq_congestion::Unlimited),
	};
	// A value that cannot go on the wire is an error: clamping it would ignore what was set.
	transport.validate()?;
	Ok(transport)
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

#[cfg(test)]
mod tests {
	use std::num::NonZeroU64;

	use super::{CONNECTION_WINDOW, MAX_STREAMS, SEND_WINDOW, STREAM_WINDOW, transport, transport_config};
	use crate::quic::Rate;

	#[test]
	fn limits_default_to_the_fixed_credits() {
		let limits = transport_config(&transport::Config::default())
			.expect("defaults")
			.limits;
		assert_eq!(limits.max_streams, NonZeroU64::new(MAX_STREAMS));
		assert_eq!(limits.stream_receive_window, NonZeroU64::new(STREAM_WINDOW.into()));
		assert_eq!(limits.receive_window, NonZeroU64::new(CONNECTION_WINDOW.into()));
		assert_eq!(limits.send_window, NonZeroU64::new(SEND_WINDOW.into()));
	}

	#[test]
	fn configured_limits_replace_the_defaults() {
		let mut config = transport::Config::default();
		config.limits.receive_window = NonZeroU64::new(256 << 20);
		config.limits.stream_receive_window = NonZeroU64::new(8 << 20);
		config.limits.send_window = NonZeroU64::new(32 << 20);
		config.limits.receive_rate = Some(Rate::BytesPerSecond(NonZeroU64::new(125_000).unwrap()));
		config.limits.send_rate = Some(Rate::Unlimited);

		let limits = transport_config(&config).expect("windows").limits;
		assert_eq!(limits.receive_window, NonZeroU64::new(256 << 20));
		assert_eq!(limits.stream_receive_window, NonZeroU64::new(8 << 20));
		assert_eq!(limits.send_window, NonZeroU64::new(32 << 20));
		assert_eq!(limits.receive_rate, config.limits.receive_rate);
		assert_eq!(limits.send_rate, config.limits.send_rate);
	}

	#[test]
	fn a_receive_window_past_a_varint_is_an_error() {
		let mut over = transport::Config::default();
		over.limits.receive_window = NonZeroU64::new(1 << 62);
		let err = transport_config(&over).expect_err("past a varint");
		assert!(err.to_string().contains("receive_window"), "{err}");

		let mut over = transport::Config::default();
		over.limits.stream_receive_window = NonZeroU64::new(u64::MAX);
		let err = transport_config(&over).expect_err("past a varint");
		assert!(err.to_string().contains("stream_receive_window"), "{err}");
	}
}
