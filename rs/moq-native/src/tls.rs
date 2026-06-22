use crate::crypto;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::{fs, io};

#[cfg(any(feature = "quinn", feature = "noq"))]
use rustls::pki_types::PrivatePkcs8KeyDer;
#[cfg(any(feature = "quinn", feature = "noq"))]
use std::sync::RwLock;

/// Errors loading or generating TLS certificates and keys.
///
/// Shared by the client TLS config and the quinn/noq servers so each backend's
/// error type can compose it via `#[from]`.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
	#[error("failed to open certificate file")]
	Open(#[source] std::io::Error),

	#[error("failed to read file")]
	ReadFile(#[source] std::io::Error),

	#[error("failed to read certificates")]
	Read(#[source] rustls::pki_types::pem::Error),

	#[error("failed to parse private key")]
	Key(#[source] rustls::pki_types::pem::Error),

	#[error("no certificates found")]
	Empty,

	#[error("no roots found in {}", .0.display())]
	EmptyRoots(PathBuf),

	#[error(
		"no trusted roots: provide --tls-root, enable --tls-system-roots, or use --tls-fingerprint / --tls-disable-verify"
	)]
	NoRoots,

	#[error("invalid TLS fingerprint (expected hex-encoded SHA-256)")]
	Fingerprint(#[source] hex::FromHexError),

	#[error("invalid TLS fingerprint length: expected 32 bytes (SHA-256), got {0}")]
	FingerprintLength(usize),

	#[error("failed to add root certificate")]
	AddRoot(#[source] rustls::Error),

	#[error("failed to configure client certificate")]
	ClientAuth(#[source] rustls::Error),

	#[error("both --client-tls-cert and --client-tls-key must be provided")]
	IncompleteClientAuth,

	#[error("must provide both cert and key")]
	CertKeyCountMismatch,

	#[error("must provide at least one cert/key pair or generate entry")]
	NoCertSource,

	#[error("private key {} doesn't match certificate {}", key.display(), cert.display())]
	KeyMismatch {
		key: PathBuf,
		cert: PathBuf,
		#[source]
		source: rustls::Error,
	},

	#[error(transparent)]
	Rustls(#[from] rustls::Error),

	#[cfg(any(feature = "quinn", feature = "noq", feature = "quiche"))]
	#[error(transparent)]
	Rcgen(#[from] rcgen::Error),

	#[error("no crypto provider available; enable aws-lc-rs or ring feature")]
	NoCryptoProvider,
}

/// Convenience alias for results produced by this module.
pub type Result<T> = std::result::Result<T, Error>;

/// Read a PEM file into its list of certificates.
pub(crate) fn read_certs(path: &Path) -> Result<Vec<CertificateDer<'static>>> {
	let file = fs::File::open(path).map_err(Error::Open)?;
	let mut reader = io::BufReader::new(file);
	CertificateDer::pem_reader_iter(&mut reader)
		.collect::<std::result::Result<_, _>>()
		.map_err(Error::Read)
}

// ── Client ──────────────────────────────────────────────────────────

/// TLS configuration for the client.
#[serde_with::serde_as]
#[derive(Clone, Default, Debug, clap::Args, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
#[group(id = "tls-client")]
#[non_exhaustive]
pub struct Client {
	/// Trust the TLS root at this path, encoded as PEM.
	///
	/// This value can be provided multiple times for multiple roots.
	/// In config files, accepts either a single string or a TOML array.
	///
	/// These roots are added on top of the system roots. By default the system
	/// roots are only loaded when no custom root is given, so passing a root
	/// replaces them; set `--tls-system-roots` to trust both (e.g. to reach a
	/// local relay with a private CA and a remote one with a public CA).
	#[serde(skip_serializing_if = "Vec::is_empty")]
	#[arg(id = "tls-root", long = "tls-root", env = "MOQ_CLIENT_TLS_ROOT")]
	#[serde_as(as = "serde_with::OneOrMany<_>")]
	pub root: Vec<PathBuf>,

	/// Also trust the platform's native root certificates.
	///
	/// Defaults to enabled only when no `--tls-root` is given. Set it explicitly
	/// to trust the system roots alongside any custom roots, or set it to false
	/// to trust only the custom roots. Trusting neither (no custom root and
	/// system roots disabled) is rejected, since verification could never pass.
	#[serde(skip_serializing_if = "Option::is_none")]
	#[arg(
		id = "tls-system-roots",
		long = "tls-system-roots",
		env = "MOQ_CLIENT_TLS_SYSTEM_ROOTS",
		default_missing_value = "true",
		num_args = 0..=1,
		require_equals = true,
		value_parser = clap::value_parser!(bool),
	)]
	pub system_roots: Option<bool>,

	/// Pin the peer to a certificate with one of these SHA-256 fingerprints, encoded as hex.
	///
	/// This is the native equivalent of the browser's WebTransport `serverCertificateHashes`,
	/// and accepts the same values a server reports via its certificate fingerprints. Use it to
	/// trust a self-signed certificate without disabling verification or fetching the hash over
	/// an insecure `http://` request. When set, the normal CA/root chain is bypassed: only the
	/// leaf certificate's fingerprint is checked.
	///
	/// This value can be provided multiple times to accept any of several fingerprints (e.g.
	/// across a certificate rotation). In config files, accepts either a single string or a TOML array.
	#[serde(skip_serializing_if = "Vec::is_empty")]
	#[arg(id = "tls-fingerprint", long = "tls-fingerprint", env = "MOQ_CLIENT_TLS_FINGERPRINT")]
	#[serde_as(as = "serde_with::OneOrMany<_>")]
	pub fingerprint: Vec<String>,

	/// PEM file containing the client certificate chain for mTLS.
	///
	/// Only certificates are extracted; any private keys in the file are ignored.
	/// Must be paired with `--client-tls-key`.
	#[serde(skip_serializing_if = "Option::is_none")]
	#[arg(id = "client-tls-cert", long = "client-tls-cert", env = "MOQ_CLIENT_TLS_CERT")]
	pub cert: Option<PathBuf>,

	/// PEM file containing the private key for mTLS.
	///
	/// Only the private key is extracted; any certificates in the file are ignored.
	/// Must be paired with `--client-tls-cert`.
	#[serde(skip_serializing_if = "Option::is_none")]
	#[arg(id = "client-tls-key", long = "client-tls-key", env = "MOQ_CLIENT_TLS_KEY")]
	pub key: Option<PathBuf>,

	/// Danger: Disable TLS certificate verification.
	///
	/// Fine for local development and between relays, but should be used in caution in production.
	#[serde(skip_serializing_if = "Option::is_none")]
	#[arg(
		id = "tls-disable-verify",
		long = "tls-disable-verify",
		env = "MOQ_CLIENT_TLS_DISABLE_VERIFY",
		default_missing_value = "true",
		num_args = 0..=1,
		require_equals = true,
		value_parser = clap::value_parser!(bool),
	)]
	pub disable_verify: Option<bool>,
}

impl Client {
	/// Build a [`rustls::ClientConfig`] from this configuration.
	///
	/// Trusts the configured roots plus the platform's native roots (the latter
	/// gated by `system_roots`), optionally attaches a client identity for mTLS,
	/// and swaps in fingerprint pinning or disabled verification when requested.
	pub fn build(&self) -> Result<rustls::ClientConfig> {
		let provider = crypto::provider();

		// Default to system roots only when no custom root is given, so passing a
		// root replaces them unless the system roots are explicitly re-enabled.
		let system_roots = self.system_roots.unwrap_or(self.root.is_empty());

		// fingerprint pinning and disable_verify swap in their own verifier below,
		// so an empty root store is fine in those cases. Otherwise WebPKI needs at
		// least one trusted root to ever succeed, so fail fast instead of producing
		// confusing handshake errors later.
		let custom_verifier = self.disable_verify.unwrap_or_default() || !self.fingerprint.is_empty();
		if !system_roots && self.root.is_empty() && !custom_verifier {
			return Err(Error::NoRoots);
		}

		let mut roots = rustls::RootCertStore::empty();
		if system_roots {
			let native = rustls_native_certs::load_native_certs();
			for err in native.errors {
				tracing::warn!(%err, "failed to load root cert");
			}
			for cert in native.certs {
				roots.add(cert).map_err(Error::AddRoot)?;
			}
		}
		for root in &self.root {
			let certs = read_certs(root)?;
			if certs.is_empty() {
				return Err(Error::EmptyRoots(root.clone()));
			}
			for cert in certs {
				roots.add(cert).map_err(Error::AddRoot)?;
			}
		}

		// Allow TLS 1.2 in addition to 1.3 for WebSocket compatibility.
		// QUIC always negotiates TLS 1.3 regardless of this setting.
		let builder = rustls::ClientConfig::builder_with_provider(provider.clone())
			.with_protocol_versions(&[&rustls::version::TLS13, &rustls::version::TLS12])?
			.with_root_certificates(roots);

		let mut tls = match (&self.cert, &self.key) {
			(Some(cert_path), Some(key_path)) => {
				let cert_pem = fs::read(cert_path).map_err(Error::ReadFile)?;
				let chain: Vec<CertificateDer<'static>> = CertificateDer::pem_slice_iter(&cert_pem)
					.collect::<std::result::Result<_, _>>()
					.map_err(Error::Read)?;
				if chain.is_empty() {
					return Err(Error::Empty);
				}
				let key_pem = fs::read(key_path).map_err(Error::ReadFile)?;
				let key = PrivateKeyDer::from_pem_slice(&key_pem).map_err(Error::Key)?;
				builder.with_client_auth_cert(chain, key).map_err(Error::ClientAuth)?
			}
			(None, None) => builder.with_no_client_auth(),
			_ => return Err(Error::IncompleteClientAuth),
		};

		if self.disable_verify.unwrap_or_default() {
			tracing::warn!("TLS server certificate verification is disabled; A man-in-the-middle attack is possible.");
			let noop = NoCertificateVerification(provider);
			tls.dangerous().set_certificate_verifier(Arc::new(noop));
		} else if !self.fingerprint.is_empty() {
			let fingerprints = self
				.fingerprint
				.iter()
				.map(|fp| {
					let bytes = hex::decode(fp.trim()).map_err(Error::Fingerprint)?;
					match bytes.len() {
						32 => Ok(bytes),
						len => Err(Error::FingerprintLength(len)),
					}
				})
				.collect::<Result<Vec<_>>>()?;

			let verifier = FingerprintVerifier::new(provider, fingerprints);
			tls.dangerous().set_certificate_verifier(Arc::new(verifier));
		}

		Ok(tls)
	}
}

// ── Server ──────────────────────────────────────────────────────────

/// TLS configuration for the server.
///
/// Certificate and keys must currently be files on disk.
/// Alternatively, you can generate a self-signed certificate given a list of hostnames.
///
/// In config files, each list field accepts either a single string or a TOML array.
#[serde_with::serde_as]
#[derive(clap::Args, Clone, Default, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
#[group(id = "tls-server")]
#[non_exhaustive]
pub struct Server {
	/// Load the given certificate from disk.
	#[arg(long = "tls-cert", id = "tls-cert", env = "MOQ_SERVER_TLS_CERT")]
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	#[serde_as(as = "serde_with::OneOrMany<_>")]
	pub cert: Vec<PathBuf>,

	/// Load the given key from disk.
	#[arg(long = "tls-key", id = "tls-key", env = "MOQ_SERVER_TLS_KEY")]
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	#[serde_as(as = "serde_with::OneOrMany<_>")]
	pub key: Vec<PathBuf>,

	/// Or generate a new certificate and key with the given hostnames.
	/// This won't be valid unless the client uses the fingerprint or disables verification.
	#[arg(
		long = "tls-generate",
		id = "tls-generate",
		value_delimiter = ',',
		env = "MOQ_SERVER_TLS_GENERATE"
	)]
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	#[serde_as(as = "serde_with::OneOrMany<_>")]
	pub generate: Vec<String>,

	/// PEM file(s) of root CAs for validating optional client certificates (mTLS).
	///
	/// When set, clients *may* present a certificate during the TLS handshake.
	/// Valid presentations are reported via [`crate::Request::peer_identity`]
	/// and can be used by the application to grant elevated access. Clients that
	/// do not present a certificate are unaffected.
	///
	/// Only supported by the Quinn and noq backends.
	#[arg(
		long = "server-tls-root",
		id = "server-tls-root",
		value_delimiter = ',',
		env = "MOQ_SERVER_TLS_ROOT"
	)]
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	#[serde_as(as = "serde_with::OneOrMany<_>")]
	pub root: Vec<PathBuf>,
}

impl Server {
	/// Load all configured root CAs into a [`rustls::RootCertStore`].
	pub fn load_roots(&self) -> Result<rustls::RootCertStore> {
		let mut roots = rustls::RootCertStore::empty();
		for path in &self.root {
			let certs = read_certs(path)?;
			if certs.is_empty() {
				return Err(Error::Empty);
			}
			for cert in certs {
				roots.add(cert).map_err(Error::AddRoot)?;
			}
		}
		Ok(roots)
	}
}

/// A peer's validated client-certificate chain from the mTLS handshake.
///
/// Returned by [`crate::Request::peer_identity`] when the peer presented a
/// certificate that chained to a configured [`Server::root`]. Owns the chain
/// (leaf first) so callers can inspect it, e.g. [`expiry`](Self::expiry),
/// without re-parsing the type-erased QUIC identity.
pub struct PeerIdentity {
	chain: Vec<CertificateDer<'static>>,
}

impl PeerIdentity {
	/// Wrap the type-erased identity from `quinn::Connection::peer_identity`.
	/// Returns `None` if the peer presented no certificate or the identity is
	/// not a certificate chain.
	#[cfg(any(feature = "quinn", feature = "noq"))]
	pub(crate) fn from_any(identity: Option<Box<dyn std::any::Any>>) -> Option<Self> {
		let chain = identity?.downcast::<Vec<CertificateDer<'static>>>().ok()?;
		Some(Self { chain: *chain })
	}

	/// The validated certificate chain, leaf first.
	///
	/// Exposes [`rustls::pki_types::CertificateDer`] directly (already part of
	/// this crate's public API via the `rustls` re-export), so a major `rustls`
	/// bump is a breaking change for consumers of this method.
	pub fn chain(&self) -> &[CertificateDer<'static>] {
		&self.chain
	}

	/// The leaf certificate's `notAfter`, if it parses. A `notAfter` before the
	/// Unix epoch is reported as `None`.
	pub fn expiry(&self) -> Option<std::time::SystemTime> {
		use std::time::{Duration, UNIX_EPOCH};

		let leaf = self.chain.first()?;
		let (_, cert) = x509_parser::parse_x509_certificate(leaf).ok()?;
		let secs = u64::try_from(cert.validity().not_after.timestamp()).ok()?;
		Some(UNIX_EPOCH + Duration::from_secs(secs))
	}
}

/// TLS certificate information including fingerprints.
#[derive(Debug)]
pub struct Info {
	#[cfg(any(feature = "noq", feature = "quinn"))]
	pub(crate) certs: Vec<Arc<rustls::sign::CertifiedKey>>,
	pub fingerprints: Vec<String>,
}

// ── NoCertificateVerification ───────────────────────────────────────

#[derive(Debug)]
struct NoCertificateVerification(crypto::Provider);

impl rustls::client::danger::ServerCertVerifier for NoCertificateVerification {
	fn verify_server_cert(
		&self,
		_end_entity: &CertificateDer<'_>,
		_intermediates: &[CertificateDer<'_>],
		_server_name: &ServerName<'_>,
		_ocsp: &[u8],
		_now: UnixTime,
	) -> std::result::Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
		Ok(rustls::client::danger::ServerCertVerified::assertion())
	}

	fn verify_tls12_signature(
		&self,
		message: &[u8],
		cert: &CertificateDer<'_>,
		dss: &rustls::DigitallySignedStruct,
	) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
		rustls::crypto::verify_tls12_signature(message, cert, dss, &self.0.signature_verification_algorithms)
	}

	fn verify_tls13_signature(
		&self,
		message: &[u8],
		cert: &CertificateDer<'_>,
		dss: &rustls::DigitallySignedStruct,
	) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
		rustls::crypto::verify_tls13_signature(message, cert, dss, &self.0.signature_verification_algorithms)
	}

	fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
		self.0.signature_verification_algorithms.supported_schemes()
	}
}

// ── FingerprintVerifier ─────────────────────────────────────────────

#[derive(Debug)]
pub(crate) struct FingerprintVerifier {
	provider: crypto::Provider,
	fingerprints: Vec<Vec<u8>>,
}

impl FingerprintVerifier {
	pub fn new(provider: crypto::Provider, fingerprints: Vec<Vec<u8>>) -> Self {
		Self { provider, fingerprints }
	}
}

impl rustls::client::danger::ServerCertVerifier for FingerprintVerifier {
	fn verify_server_cert(
		&self,
		end_entity: &CertificateDer<'_>,
		_intermediates: &[CertificateDer<'_>],
		_server_name: &ServerName<'_>,
		_ocsp: &[u8],
		_now: UnixTime,
	) -> std::result::Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
		let fingerprint = crypto::sha256(&self.provider, end_entity);
		if self.fingerprints.iter().any(|fp| fingerprint.as_ref() == fp.as_slice()) {
			Ok(rustls::client::danger::ServerCertVerified::assertion())
		} else {
			Err(rustls::Error::General("fingerprint mismatch".into()))
		}
	}

	fn verify_tls12_signature(
		&self,
		message: &[u8],
		cert: &CertificateDer<'_>,
		dss: &rustls::DigitallySignedStruct,
	) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
		rustls::crypto::verify_tls12_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
	}

	fn verify_tls13_signature(
		&self,
		message: &[u8],
		cert: &CertificateDer<'_>,
		dss: &rustls::DigitallySignedStruct,
	) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
		rustls::crypto::verify_tls13_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
	}

	fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
		self.provider.signature_verification_algorithms.supported_schemes()
	}
}

#[cfg(test)]
#[cfg(all(any(feature = "quinn", feature = "noq", feature = "quiche"), feature = "aws-lc-rs"))]
mod tests {
	use super::*;
	use rustls::client::danger::ServerCertVerifier;
	use rustls::pki_types::ServerName;

	fn self_signed() -> CertificateDer<'static> {
		let key = rcgen::KeyPair::generate().unwrap();
		let params = rcgen::CertificateParams::new(vec!["localhost".to_string()]).unwrap();
		params.self_signed(&key).unwrap().into()
	}

	#[cfg(any(feature = "quinn", feature = "noq"))]
	#[test]
	fn peer_identity_expiry_reads_not_after() {
		// notAfter at a whole second so the round-trip is exact.
		let not_after = ::time::OffsetDateTime::from_unix_timestamp(2_000_000_000).unwrap();

		let key = rcgen::KeyPair::generate().unwrap();
		let mut params = rcgen::CertificateParams::new(vec!["localhost".to_string()]).unwrap();
		params.not_after = not_after;
		let cert: CertificateDer<'static> = params.self_signed(&key).unwrap().into();

		// quinn/noq hand back the chain as a boxed Vec<CertificateDer>.
		let identity: Box<dyn std::any::Any> = Box::new(vec![cert]);
		let parsed = PeerIdentity::from_any(Some(identity)).expect("chain parsed");
		let expiry = parsed.expiry().expect("expiry parsed");
		assert_eq!(
			expiry.duration_since(std::time::UNIX_EPOCH).unwrap().as_secs(),
			2_000_000_000
		);
	}

	#[cfg(any(feature = "quinn", feature = "noq"))]
	#[test]
	fn peer_identity_none_without_chain() {
		assert!(PeerIdentity::from_any(None).is_none());
		// A wrong downcast type (not a cert chain) yields None rather than panicking.
		let bogus: Box<dyn std::any::Any> = Box::new(42u32);
		assert!(PeerIdentity::from_any(Some(bogus)).is_none());
	}

	#[test]
	fn fingerprint_verifier_matches_and_rejects() {
		let provider = crypto::provider();
		let cert = self_signed();
		let fingerprint = crypto::sha256(&provider, cert.as_ref()).as_ref().to_vec();

		let name = ServerName::try_from("localhost").unwrap();
		let now = UnixTime::now();

		let verifier = FingerprintVerifier::new(provider.clone(), vec![fingerprint]);
		assert!(verifier.verify_server_cert(&cert, &[], &name, &[], now).is_ok());

		// A different leaf certificate must not satisfy the pin.
		let other = self_signed();
		assert!(verifier.verify_server_cert(&other, &[], &name, &[], now).is_err());
	}

	#[test]
	fn build_installs_fingerprint_verifier() {
		let cert = self_signed();
		let fingerprint = hex::encode(crypto::sha256(&crypto::provider(), cert.as_ref()));

		// A bogus hash still builds; verification happens at handshake time.
		let config = Client {
			fingerprint: vec![fingerprint],
			..Default::default()
		};
		assert!(config.build().is_ok());
	}

	#[test]
	fn build_rejects_invalid_fingerprint_hex() {
		let config = Client {
			fingerprint: vec!["not-hex".to_string()],
			..Default::default()
		};
		assert!(matches!(config.build(), Err(Error::Fingerprint(_))));
	}

	#[test]
	fn build_rejects_wrong_length_fingerprint() {
		// Valid hex, but only 2 bytes instead of 32.
		let config = Client {
			fingerprint: vec!["abcd".to_string()],
			..Default::default()
		};
		assert!(matches!(config.build(), Err(Error::FingerprintLength(2))));
	}

	#[test]
	fn build_rejects_no_roots() {
		// System roots disabled with no custom root and no alternate verifier:
		// nothing could ever verify, so reject up front.
		let config = Client {
			system_roots: Some(false),
			..Default::default()
		};
		assert!(matches!(config.build(), Err(Error::NoRoots)));
	}

	#[test]
	fn build_allows_no_roots_when_verification_overridden() {
		// disable_verify swaps in its own verifier, so an empty store is fine.
		let config = Client {
			system_roots: Some(false),
			disable_verify: Some(true),
			..Default::default()
		};
		assert!(config.build().is_ok());

		// Same for fingerprint pinning.
		let cert = self_signed();
		let fingerprint = hex::encode(crypto::sha256(&crypto::provider(), cert.as_ref()));
		let config = Client {
			system_roots: Some(false),
			fingerprint: vec![fingerprint],
			..Default::default()
		};
		assert!(config.build().is_ok());
	}
}

// ── ServeCerts ──────────────────────────────────────────────────────

#[cfg(any(feature = "quinn", feature = "noq"))]
#[derive(Debug)]
pub(crate) struct ServeCerts {
	pub info: Arc<RwLock<Info>>,
	provider: crypto::Provider,
}

#[cfg(any(feature = "quinn", feature = "noq"))]
impl ServeCerts {
	pub fn new(provider: crypto::Provider) -> Self {
		Self {
			info: Arc::new(RwLock::new(Info {
				certs: Vec::new(),
				fingerprints: Vec::new(),
			})),
			provider,
		}
	}

	pub fn load_certs(&self, config: &Server) -> Result<()> {
		if config.cert.len() != config.key.len() {
			return Err(Error::CertKeyCountMismatch);
		}
		if config.cert.is_empty() && config.generate.is_empty() {
			return Err(Error::NoCertSource);
		}

		let mut certs = Vec::new();

		// Load the certificate and key files based on their index.
		for (cert, key) in config.cert.iter().zip(config.key.iter()) {
			certs.push(Arc::new(self.load(cert, key)?));
		}

		// Generate a new certificate if requested.
		if !config.generate.is_empty() {
			certs.push(Arc::new(self.generate(&config.generate)?));
		}

		self.set_certs(certs);
		Ok(())
	}

	// Load a certificate and corresponding key from a file, but don't add it to the certs
	fn load(&self, chain_path: &Path, key_path: &Path) -> Result<rustls::sign::CertifiedKey> {
		let chain = read_certs(chain_path)?;
		if chain.is_empty() {
			return Err(Error::Empty);
		}

		// Read the PEM private key
		let key = PrivateKeyDer::from_pem_file(key_path).map_err(Error::Key)?;
		let key = self.provider.key_provider.load_private_key(key)?;

		let certified_key = rustls::sign::CertifiedKey::new(chain, key);

		certified_key.keys_match().map_err(|source| Error::KeyMismatch {
			key: key_path.to_path_buf(),
			cert: chain_path.to_path_buf(),
			source,
		})?;

		Ok(certified_key)
	}

	#[cfg(any(feature = "aws-lc-rs", feature = "ring"))]
	fn generate(&self, hostnames: &[String]) -> Result<rustls::sign::CertifiedKey> {
		let key_pair = rcgen::KeyPair::generate()?;

		let mut params = rcgen::CertificateParams::new(hostnames)?;

		// Make the certificate valid for two weeks, starting yesterday (in case of clock drift).
		// WebTransport certificates MUST be valid for two weeks at most.
		params.not_before = ::time::OffsetDateTime::now_utc() - ::time::Duration::days(1);
		params.not_after = params.not_before + ::time::Duration::days(14);

		// Generate the certificate
		let cert = params.self_signed(&key_pair)?;

		// Convert the rcgen type to the rustls type.
		let key_der = key_pair.serialized_der().to_vec();
		let key_der = PrivatePkcs8KeyDer::from(key_der);
		let key = self.provider.key_provider.load_private_key(key_der.into())?;

		// Create a rustls::sign::CertifiedKey
		Ok(rustls::sign::CertifiedKey::new(vec![cert.into()], key))
	}

	#[cfg(not(any(feature = "aws-lc-rs", feature = "ring")))]
	fn generate(&self, _hostnames: &[String]) -> Result<rustls::sign::CertifiedKey> {
		Err(Error::NoCryptoProvider)
	}

	// Replace the certificates
	pub fn set_certs(&self, certs: Vec<Arc<rustls::sign::CertifiedKey>>) {
		let fingerprints = certs
			.iter()
			.map(|ck| {
				let fingerprint = crate::crypto::sha256(&self.provider, ck.cert[0].as_ref());
				hex::encode(fingerprint)
			})
			.collect();

		let mut info = self.info.write().expect("info write lock poisoned");
		info.certs = certs;
		info.fingerprints = fingerprints;
	}

	// Return the best certificate for the given ClientHello.
	fn best_certificate(
		&self,
		client_hello: &rustls::server::ClientHello<'_>,
	) -> Option<Arc<rustls::sign::CertifiedKey>> {
		let server_name = client_hello.server_name()?;
		let dns_name = rustls::pki_types::ServerName::try_from(server_name).ok()?;

		for ck in self.info.read().expect("info read lock poisoned").certs.iter() {
			let leaf: webpki::EndEntityCert = ck
				.end_entity_cert()
				.expect("missing certificate")
				.try_into()
				.expect("failed to parse certificate");

			if leaf.verify_is_valid_for_subject_name(&dns_name).is_ok() {
				return Some(ck.clone());
			}
		}

		None
	}
}

#[cfg(any(feature = "quinn", feature = "noq"))]
impl rustls::server::ResolvesServerCert for ServeCerts {
	fn resolve(&self, client_hello: rustls::server::ClientHello<'_>) -> Option<Arc<rustls::sign::CertifiedKey>> {
		if let Some(cert) = self.best_certificate(&client_hello) {
			return Some(cert);
		}

		// If this happens, it means the client was trying to connect to an unknown hostname.
		// We do our best and return the first certificate.
		tracing::warn!(server_name = ?client_hello.server_name(), "no SNI certificate found");

		self.info
			.read()
			.expect("info read lock poisoned")
			.certs
			.first()
			.cloned()
	}
}

// ── reload_certs ────────────────────────────────────────────────────

/// Watch the on-disk cert/key files and reload them whenever they change.
///
/// Reacting to the filesystem means cert-manager, Kubernetes secret mounts, and
/// `mv`-into-place rotate certs with no external signal. Returns immediately when
/// only generated certs are configured: there's nothing on disk to watch.
#[cfg(any(feature = "quinn", feature = "noq"))]
pub(crate) async fn reload_certs(certs: Arc<ServeCerts>, tls_config: Server) {
	let paths: Vec<PathBuf> = tls_config.cert.iter().chain(tls_config.key.iter()).cloned().collect();
	if paths.is_empty() {
		return;
	}

	let mut watcher = match crate::watch::FileWatcher::new(&paths) {
		Ok(watcher) => watcher,
		Err(err) => {
			tracing::error!(%err, "failed to watch certificate files; hot reload disabled");
			return;
		}
	};

	loop {
		watcher.changed().await;
		tracing::info!("reloading server certificates");

		if let Err(err) = certs.load_certs(&tls_config) {
			tracing::warn!(%err, "failed to reload server certificates");
		}
	}
}
