use crate::crypto;
use crate::server::{ServerTlsConfig, ServerTlsInfo};
use anyhow::Context;
use rustls::pki_types::{CertificateDer, PrivatePkcs8KeyDer, ServerName, UnixTime};
use std::fs;
use std::io::{self, Cursor, Read};
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

// ── FingerprintVerifier ─────────────────────────────────────────────

#[derive(Debug)]
pub(crate) struct FingerprintVerifier {
	provider: crypto::Provider,
	fingerprint: Vec<u8>,
}

impl FingerprintVerifier {
	pub fn new(provider: crypto::Provider, fingerprint: Vec<u8>) -> Self {
		Self { provider, fingerprint }
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
	) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
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
	) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
		rustls::crypto::verify_tls12_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
	}

	fn verify_tls13_signature(
		&self,
		message: &[u8],
		cert: &CertificateDer<'_>,
		dss: &rustls::DigitallySignedStruct,
	) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
		rustls::crypto::verify_tls13_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
	}

	fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
		self.provider.signature_verification_algorithms.supported_schemes()
	}
}

// ── ServeCerts ──────────────────────────────────────────────────────

#[derive(Debug)]
pub(crate) struct ServeCerts {
	pub info: Arc<RwLock<ServerTlsInfo>>,
	provider: crypto::Provider,
}

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

	pub fn load_certs(&self, config: &ServerTlsConfig) -> anyhow::Result<()> {
		anyhow::ensure!(
			config.cert.len() == config.key.len(),
			"must provide matching --tls-cert and --tls-key pairs"
		);
		anyhow::ensure!(
			!config.cert.is_empty() || !config.identity.is_empty() || !config.generate.is_empty(),
			"must provide at least one of --tls-cert/--tls-key, --tls-identity, or --tls-generate"
		);

		let mut certs = Vec::new();

		// Load paired cert/key files.
		for (cert, key) in config.cert.iter().zip(config.key.iter()) {
			certs.push(Arc::new(self.load(cert, key)?));
		}

		// Load combined identity files (cert chain + private key in one PEM).
		for identity in config.identity.iter() {
			certs.push(Arc::new(self.load(identity, identity)?));
		}

		// Generate a new certificate if requested.
		if !config.generate.is_empty() {
			certs.push(Arc::new(self.generate(&config.generate)?));
		}

		self.set_certs(certs);
		Ok(())
	}

	// Load a certificate and corresponding key from files (or a single combined file if the paths match).
	fn load(&self, chain_path: &PathBuf, key_path: &PathBuf) -> anyhow::Result<rustls::sign::CertifiedKey> {
		let chain = fs::File::open(chain_path).context("failed to open cert file")?;
		let mut chain = io::BufReader::new(chain);

		let chain: Vec<CertificateDer> = rustls_pemfile::certs(&mut chain)
			.collect::<Result<_, _>>()
			.context("failed to read certs")?;

		anyhow::ensure!(
			!chain.is_empty(),
			"could not find certificate in {}",
			chain_path.display()
		);

		// Read the PEM private key
		let mut keys = fs::File::open(key_path).context("failed to open key file")?;

		// Read the keys into a Vec so we can parse it twice.
		let mut buf = Vec::new();
		keys.read_to_end(&mut buf)?;

		let key = rustls_pemfile::private_key(&mut Cursor::new(&buf))?
			.with_context(|| format!("missing private key in {}", key_path.display()))?;
		let key = self.provider.key_provider.load_private_key(key)?;

		let certified_key = rustls::sign::CertifiedKey::new(chain, key);

		certified_key.keys_match().context(format!(
			"private key {} doesn't match certificate {}",
			key_path.display(),
			chain_path.display()
		))?;

		Ok(certified_key)
	}

	#[cfg(any(feature = "aws-lc-rs", feature = "ring"))]
	fn generate(&self, hostnames: &[String]) -> anyhow::Result<rustls::sign::CertifiedKey> {
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
	fn generate(&self, _hostnames: &[String]) -> anyhow::Result<rustls::sign::CertifiedKey> {
		anyhow::bail!("no crypto provider available; enable aws-lc-rs or ring feature");
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

// ── reload_certs (unix) ─────────────────────────────────────────────

#[cfg(unix)]
pub(crate) async fn reload_certs(certs: Arc<ServeCerts>, tls_config: ServerTlsConfig) {
	use tokio::signal::unix::{SignalKind, signal};

	// Dunno why we wouldn't be allowed to listen for signals, but just in case.
	let mut listener = signal(SignalKind::user_defined1()).expect("failed to listen for signals");

	while listener.recv().await.is_some() {
		tracing::info!("reloading server certificates");

		if let Err(err) = certs.load_certs(&tls_config) {
			tracing::warn!(%err, "failed to reload server certificates");
		}
	}
}

#[cfg(all(test, any(feature = "aws-lc-rs", feature = "ring")))]
mod tests {
	use super::*;
	use std::io::Write;

	fn der_to_pem(label: &str, der: &[u8]) -> String {
		use base64::Engine;
		let body = base64::engine::general_purpose::STANDARD.encode(der);
		let mut out = format!("-----BEGIN {label}-----\n");
		for chunk in body.as_bytes().chunks(64) {
			out.push_str(std::str::from_utf8(chunk).unwrap());
			out.push('\n');
		}
		out.push_str(&format!("-----END {label}-----\n"));
		out
	}

	fn generate_pair() -> (String, String) {
		let key_pair = rcgen::KeyPair::generate().unwrap();
		let params = rcgen::CertificateParams::new(["localhost".to_string()]).unwrap();
		let cert = params.self_signed(&key_pair).unwrap();
		let cert_pem = der_to_pem("CERTIFICATE", cert.der());
		let key_pem = der_to_pem("PRIVATE KEY", key_pair.serialized_der());
		(cert_pem, key_pem)
	}

	fn write_tempfile(contents: &str) -> tempfile::NamedTempFile {
		let mut f = tempfile::NamedTempFile::new().unwrap();
		f.write_all(contents.as_bytes()).unwrap();
		f
	}

	#[test]
	fn load_separate_cert_and_key() {
		let (cert_pem, key_pem) = generate_pair();
		let cert_file = write_tempfile(&cert_pem);
		let key_file = write_tempfile(&key_pem);

		let config = ServerTlsConfig {
			cert: vec![cert_file.path().to_path_buf()],
			key: vec![key_file.path().to_path_buf()],
			..Default::default()
		};

		let serve = ServeCerts::new(crypto::provider());
		serve.load_certs(&config).expect("separate cert/key should load");
		assert_eq!(serve.info.read().unwrap().certs.len(), 1);
	}

	#[test]
	fn load_combined_identity() {
		let (cert_pem, key_pem) = generate_pair();
		let combined = write_tempfile(&format!("{cert_pem}{key_pem}"));

		let config = ServerTlsConfig {
			identity: vec![combined.path().to_path_buf()],
			..Default::default()
		};

		let serve = ServeCerts::new(crypto::provider());
		serve.load_certs(&config).expect("combined identity should load");
		assert_eq!(serve.info.read().unwrap().certs.len(), 1);
	}

	#[test]
	fn load_identity_with_key_first() {
		// Order of blocks in the PEM shouldn't matter.
		let (cert_pem, key_pem) = generate_pair();
		let combined = write_tempfile(&format!("{key_pem}{cert_pem}"));

		let config = ServerTlsConfig {
			identity: vec![combined.path().to_path_buf()],
			..Default::default()
		};

		let serve = ServeCerts::new(crypto::provider());
		serve.load_certs(&config).expect("key-first identity should load");
	}

	#[test]
	fn mixed_identity_and_pair() {
		let (cert_a, key_a) = generate_pair();
		let cert_file = write_tempfile(&cert_a);
		let key_file = write_tempfile(&key_a);

		let (cert_b, key_b) = generate_pair();
		let combined = write_tempfile(&format!("{cert_b}{key_b}"));

		let config = ServerTlsConfig {
			cert: vec![cert_file.path().to_path_buf()],
			key: vec![key_file.path().to_path_buf()],
			identity: vec![combined.path().to_path_buf()],
			..Default::default()
		};

		let serve = ServeCerts::new(crypto::provider());
		serve.load_certs(&config).expect("mixed config should load");
		assert_eq!(serve.info.read().unwrap().certs.len(), 2);
	}

	#[test]
	fn reject_identity_missing_key() {
		let (cert_pem, _) = generate_pair();
		let cert_only = write_tempfile(&cert_pem);

		let config = ServerTlsConfig {
			identity: vec![cert_only.path().to_path_buf()],
			..Default::default()
		};

		let serve = ServeCerts::new(crypto::provider());
		let err = serve.load_certs(&config).expect_err("cert-only identity should fail");
		assert!(
			err.to_string().contains("missing private key"),
			"unexpected error: {err}"
		);
	}

	#[test]
	fn reject_identity_missing_cert() {
		let (_, key_pem) = generate_pair();
		let key_only = write_tempfile(&key_pem);

		let config = ServerTlsConfig {
			identity: vec![key_only.path().to_path_buf()],
			..Default::default()
		};

		let serve = ServeCerts::new(crypto::provider());
		let err = serve.load_certs(&config).expect_err("key-only identity should fail");
		assert!(
			err.to_string().contains("could not find certificate"),
			"unexpected error: {err}"
		);
	}

	#[test]
	fn reject_mismatched_pair_counts() {
		let (cert_pem, _) = generate_pair();
		let cert_file = write_tempfile(&cert_pem);

		let config = ServerTlsConfig {
			cert: vec![cert_file.path().to_path_buf()],
			key: vec![],
			..Default::default()
		};

		let serve = ServeCerts::new(crypto::provider());
		serve
			.load_certs(&config)
			.expect_err("mismatched cert/key counts should fail");
	}
}
