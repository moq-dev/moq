use crate::client::ClientTls;
use crate::crypto;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::{fs, io};

#[cfg(any(feature = "quinn", feature = "noq"))]
use crate::server::{ServerTlsConfig, ServerTlsInfo};
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

// ── Client config ───────────────────────────────────────────────────

/// Build a [`rustls::ClientConfig`] from the client TLS configuration.
///
/// Loads the configured roots (or the platform's native roots if none),
/// optionally attaches a client identity for mTLS, and disables server
/// certificate verification when `disable_verify` is set.
pub(crate) fn client_config(config: &ClientTls) -> Result<rustls::ClientConfig> {
	let provider = crypto::provider();

	let mut roots = rustls::RootCertStore::empty();
	if config.root.is_empty() {
		let native = rustls_native_certs::load_native_certs();
		for err in native.errors {
			tracing::warn!(%err, "failed to load root cert");
		}
		for cert in native.certs {
			roots.add(cert).map_err(Error::AddRoot)?;
		}
	} else {
		for root in &config.root {
			let certs = read_certs(root)?;
			if certs.is_empty() {
				return Err(Error::EmptyRoots(root.clone()));
			}
			for cert in certs {
				roots.add(cert).map_err(Error::AddRoot)?;
			}
		}
	}

	// Allow TLS 1.2 in addition to 1.3 for WebSocket compatibility.
	// QUIC always negotiates TLS 1.3 regardless of this setting.
	let builder = rustls::ClientConfig::builder_with_provider(provider.clone())
		.with_protocol_versions(&[&rustls::version::TLS13, &rustls::version::TLS12])?
		.with_root_certificates(roots);

	let mut tls = match (&config.cert, &config.key) {
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

	if config.disable_verify.unwrap_or_default() {
		tracing::warn!("TLS server certificate verification is disabled; A man-in-the-middle attack is possible.");
		let noop = NoCertificateVerification(provider);
		tls.dangerous().set_certificate_verifier(Arc::new(noop));
	}

	Ok(tls)
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

#[cfg(any(feature = "quinn", feature = "noq"))]
#[derive(Debug)]
pub(crate) struct FingerprintVerifier {
	provider: crypto::Provider,
	fingerprint: Vec<u8>,
}

#[cfg(any(feature = "quinn", feature = "noq"))]
impl FingerprintVerifier {
	pub fn new(provider: crypto::Provider, fingerprint: Vec<u8>) -> Self {
		Self { provider, fingerprint }
	}
}

#[cfg(any(feature = "quinn", feature = "noq"))]
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
		if fingerprint.as_ref() == self.fingerprint.as_slice() {
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

// ── ServeCerts ──────────────────────────────────────────────────────

#[cfg(any(feature = "quinn", feature = "noq"))]
#[derive(Debug)]
pub(crate) struct ServeCerts {
	pub info: Arc<RwLock<ServerTlsInfo>>,
	provider: crypto::Provider,
}

#[cfg(any(feature = "quinn", feature = "noq"))]
impl ServeCerts {
	pub fn new(provider: crypto::Provider) -> Self {
		Self {
			info: Arc::new(RwLock::new(ServerTlsInfo {
				certs: Vec::new(),
				fingerprints: Vec::new(),
			})),
			provider,
		}
	}

	pub fn load_certs(&self, config: &ServerTlsConfig) -> Result<()> {
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
pub(crate) async fn reload_certs(certs: Arc<ServeCerts>, tls_config: ServerTlsConfig) {
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
