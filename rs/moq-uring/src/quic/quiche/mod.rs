//! The quiche backend: sans-IO [`quiche`] on the worker's UDP path.
//!
//! TLS goes through BoringSSL, which is also why the trust store is built by
//! hand here: [`quiche::Config::new`] loads the platform store before it
//! returns, so "trust only these roots" cannot be expressed by omission.

mod connection;
mod endpoint;
mod stream;

pub use connection::Connection;
pub use endpoint::Endpoint;
pub use stream::{RecvStream, SendStream};

pub(crate) use connection::Shared;

use super::{Congestion, Error, Identity, SEGMENT, Transport, client, server};

/// The platform trust store, loaded when a role config asks for it.
const SYSTEM_ROOTS: &str = "/etc/ssl/certs";

impl From<quiche::Error> for Error {
	fn from(err: quiche::Error) -> Self {
		match err {
			quiche::Error::StreamReset(code) => Self::Reset(code),
			quiche::Error::StreamStopped(code) => Self::Stop(code),
			err => Self::Quic(err.to_string()),
		}
	}
}

impl Congestion {
	/// quiche takes its controller by name, and its BBR is the v2 gcongestion
	/// port.
	fn name(self) -> &'static str {
		match self {
			Self::Loss => "cubic",
			Self::Delay => "bbr2_gcongestion",
		}
	}
}

/// Who to trust and how hard to insist, resolved from a role's config.
struct Trust {
	/// Extra PEM root files to trust.
	roots: Vec<std::path::PathBuf>,
	/// Trust the platform store as well.
	system: bool,
	/// What to do about the peer's certificate.
	verify: boring::ssl::SslVerifyMode,
}

/// The dialing half of [`tls`].
pub(crate) fn client_config(config: &client::Config) -> Result<quiche::Config, Error> {
	// Nothing is checked with verification off, so nothing is loaded: a root
	// path that does not exist must not fail a connection that was never
	// going to look at it.
	let trust = match config.verify {
		true => Trust {
			roots: config.roots.clone(),
			system: config.system_roots,
			verify: boring::ssl::SslVerifyMode::PEER,
		},
		false => Trust {
			roots: Vec::new(),
			system: false,
			verify: boring::ssl::SslVerifyMode::NONE,
		},
	};
	tls(&config.alpn, config.identity.as_ref(), trust, &config.transport)
}

/// The accepting half of [`tls`].
pub(crate) fn server_config(config: &server::Config) -> Result<quiche::Config, Error> {
	use boring::ssl::SslVerifyMode;

	config.check()?;
	let (roots, verify) = match config.client_auth.roots() {
		None => (Vec::new(), SslVerifyMode::NONE),
		Some((roots, false)) => (roots.to_vec(), SslVerifyMode::PEER),
		Some((roots, true)) => (
			roots.to_vec(),
			SslVerifyMode::PEER | SslVerifyMode::FAIL_IF_NO_PEER_CERT,
		),
	};
	let trust = Trust {
		roots,
		// Client certificates chain to the roots configured here, never to the
		// platform store for public sites.
		system: false,
		verify,
	};
	tls(&config.alpn, Some(&config.identity), trust, &config.transport)
}

/// Build the quiche config shared by both roles.
fn tls(
	alpn: &[String],
	identity: Option<&Identity>,
	trust: Trust,
	transport: &Transport,
) -> Result<quiche::Config, Error> {
	use boring::ssl::{SslContextBuilder, SslMethod};

	let mut builder = SslContextBuilder::new(SslMethod::tls()).map_err(|err| Error::Tls(err.to_string()))?;
	apply_trust(&mut builder, &trust)?;

	if let Some(identity) = identity {
		apply_identity(&mut builder, identity)?;
	}

	let mut config = quiche::Config::with_boring_ssl_ctx_builder(quiche::PROTOCOL_VERSION, builder)?;
	let alpn: Vec<&[u8]> = alpn.iter().map(|p| p.as_bytes()).collect();
	config.set_application_protos(&alpn)?;
	config.set_max_idle_timeout(transport.idle_timeout.as_millis() as u64);
	config.set_max_recv_udp_payload_size(SEGMENT);
	config.set_max_send_udp_payload_size(SEGMENT);
	config.set_initial_max_data(16 * 1024 * 1024);
	config.set_initial_max_stream_data_bidi_local(4 * 1024 * 1024);
	config.set_initial_max_stream_data_bidi_remote(4 * 1024 * 1024);
	config.set_initial_max_stream_data_uni(4 * 1024 * 1024);
	config.set_initial_max_streams_bidi(transport.max_streams);
	config.set_initial_max_streams_uni(transport.max_streams);
	config.set_cc_algorithm_name(transport.congestion.name())?;
	config.enable_dgram(true, 64, 64);
	Ok(config)
}

/// Present `identity`, from the PEM it already holds.
///
/// The file-based boring setters would re-read the paths on every call, which
/// is the whole reason [`Identity`] carries bytes.
fn apply_identity(builder: &mut boring::ssl::SslContextBuilder, identity: &Identity) -> Result<(), Error> {
	use boring::pkey::PKey;
	use boring::x509::X509;

	let tls = |err: boring::error::ErrorStack| Error::Tls(err.to_string());
	let chain = X509::stack_from_pem(identity.cert()).map_err(tls)?;
	let (leaf, intermediates) = chain
		.split_first()
		.ok_or_else(|| Error::Tls("certificate chain holds no certificates".to_string()))?;
	builder.set_certificate(leaf).map_err(tls)?;
	for cert in intermediates {
		builder.add_extra_chain_cert(cert.clone()).map_err(tls)?;
	}

	let key = PKey::private_key_from_pem(identity.key()).map_err(tls)?;
	builder.set_private_key(&key).map_err(tls)?;
	Ok(())
}

/// Point `builder` at exactly the roots `trust` names.
///
/// With [`Trust::system`] off the store is built from scratch, which is the
/// only way to mean it: adding to the store quiche's own constructor leaves
/// behind would trust the platform's CAs on top of the configured ones.
fn apply_trust(builder: &mut boring::ssl::SslContextBuilder, trust: &Trust) -> Result<(), Error> {
	use boring::x509::store::X509StoreBuilder;

	if trust.system {
		builder
			.set_default_verify_paths()
			.map_err(|err| Error::Tls(format!("{SYSTEM_ROOTS}: {err}")))?;
		for root in &trust.roots {
			for cert in read_roots(root)? {
				builder
					.cert_store_mut()
					.add_cert(cert)
					.map_err(|err| Error::Tls(format!("{}: {err}", root.display())))?;
			}
		}
	} else {
		let mut store = X509StoreBuilder::new().map_err(|err| Error::Tls(err.to_string()))?;
		for root in &trust.roots {
			for cert in read_roots(root)? {
				store
					.add_cert(cert)
					.map_err(|err| Error::Tls(format!("{}: {err}", root.display())))?;
			}
		}
		builder.set_cert_store_builder(store);
	}
	builder.set_verify(trust.verify);
	Ok(())
}

/// Read every PEM certificate in one root file, naming it when it fails.
///
/// A root is routinely a bundle of several CAs, and taking only the first
/// would reject a peer chaining to any of the others while looking configured.
/// A file holding none is an error rather than a store that trusts nothing.
fn read_roots(path: &std::path::Path) -> Result<Vec<boring::x509::X509>, Error> {
	let pem = std::fs::read(path).map_err(|err| Error::Tls(format!("{}: {err}", path.display())))?;
	let certs =
		boring::x509::X509::stack_from_pem(&pem).map_err(|err| Error::Tls(format!("{}: {err}", path.display())))?;
	if certs.is_empty() {
		return Err(Error::Tls(format!("{}: no certificates", path.display())));
	}
	Ok(certs)
}
