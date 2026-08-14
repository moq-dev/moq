//! TLS trust, certificates, and keys, split by role.
//!
//! [`Client`] (`--client-tls-*`) picks who to trust: system roots, custom roots,
//! a pinned SHA-256 fingerprint, or nothing at all. [`Server`] (`--server-tls-*`)
//! supplies the certificate chain to serve, loaded from disk or self-signed on
//! startup, and optionally the roots that authenticate mTLS clients.
//!
//! Certificates, keys, and custom root CAs loaded from disk are watched and hot
//! reloaded for new handshakes, so rotating them normally needs no restart.
//! Quiche servers are the exception: their client-auth roots are fixed when the
//! listener is built. [`Certificates`] reads the current served set back out.

use crate::crypto;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::{fs, io};

#[cfg(all(
	any(feature = "quinn", feature = "noq", feature = "quiche"),
	any(feature = "aws-lc-rs", feature = "ring")
))]
use rustls::pki_types::PrivatePkcs8KeyDer;
/// Errors loading or generating TLS certificates and keys.
///
/// Shared by the client TLS config and the quinn/noq servers so each backend's
/// error type can compose it via `#[from]`.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
	/// A certificate file couldn't be opened, usually a bad path or permissions.
	#[error("failed to open certificate file")]
	Open(#[source] std::io::Error),

	/// A certificate or key file was opened but couldn't be read to the end.
	#[error("failed to read file")]
	ReadFile(#[source] std::io::Error),

	/// A file's contents aren't valid PEM certificates.
	#[error("failed to read certificates")]
	Read(#[source] rustls::pki_types::pem::Error),

	/// A file's contents aren't a valid PEM private key.
	#[error("failed to parse private key")]
	Key(#[source] rustls::pki_types::pem::Error),

	/// A PEM file parsed cleanly but held no certificates.
	#[error("no certificates found")]
	Empty,

	/// A root PEM file parsed cleanly but held no certificates, so it would trust nothing.
	#[error("no roots found in {}", .0.display())]
	EmptyRoots(PathBuf),

	/// Nothing is configured that could ever verify a server certificate.
	#[error(
		"no trusted roots: provide --client-tls-root, enable --client-tls-system-roots, or use --client-tls-fingerprint / --client-tls-disable-verify"
	)]
	NoRoots,

	/// A configured fingerprint isn't valid hex.
	#[error("invalid TLS fingerprint (expected hex-encoded SHA-256)")]
	Fingerprint(#[source] hex::FromHexError),

	/// A configured fingerprint is valid hex but the wrong size for a SHA-256 digest.
	#[error("invalid TLS fingerprint length: expected 32 bytes (SHA-256), got {0}")]
	FingerprintLength(usize),

	/// Fingerprint pinning was combined with CA roots. Pinning bypasses the chain, so one of
	/// the two would be silently ignored.
	#[error(
		"--client-tls-fingerprint cannot be combined with --client-tls-root or --client-tls-system-roots: fingerprint pinning bypasses CA verification"
	)]
	FingerprintWithRoots,

	/// Trust material was configured alongside the flag that ignores all of it.
	#[error(
		"--client-tls-disable-verify cannot be combined with --client-tls-fingerprint, --client-tls-root or --client-tls-system-roots: it accepts every certificate, so the trust material would be ignored"
	)]
	DisableVerifyWithTrust,

	/// A root certificate parsed as PEM but rustls rejected it as a trust anchor.
	#[error("failed to add root certificate")]
	AddRoot(#[source] rustls::Error),

	/// The JNI call in [`init_android`] failed, so the platform verifier is unavailable.
	#[cfg(target_os = "android")]
	#[error("failed to initialize the Android platform verifier")]
	AndroidInit(#[source] jni::errors::Error),

	/// rustls rejected the mTLS client certificate and key, e.g. they don't match.
	#[error("failed to configure client certificate")]
	ClientAuth(#[source] rustls::Error),

	/// Only one half of the mTLS client identity was given; it needs both a cert and a key.
	#[error("both --client-tls-cert and --client-tls-key must be provided")]
	IncompleteClientAuth,

	/// The server was given a different number of certificates than keys. They pair by index.
	#[error("must provide both cert and key")]
	CertKeyCountMismatch,

	/// The server has no certificate to serve: no cert/key pair and no hostnames to generate one for.
	#[error("must provide at least one cert/key pair or generate entry")]
	NoCertSource,

	/// A server cert/key pair was paired up by index but the key isn't the certificate's.
	#[error("private key {} doesn't match certificate {}", key.display(), cert.display())]
	KeyMismatch {
		/// Path of the private key file.
		key: PathBuf,
		/// Path of the certificate file it was paired with.
		cert: PathBuf,
		/// Why rustls says the two don't match.
		#[source]
		source: rustls::Error,
	},

	/// A rustls error with no more specific context, e.g. building a config.
	#[error(transparent)]
	Rustls(#[from] rustls::Error),

	/// The mTLS client-certificate verifier couldn't be built from the configured roots.
	#[cfg(any(feature = "quinn", feature = "noq", feature = "quiche"))]
	#[error("failed to build client certificate verifier")]
	ClientVerifier(#[source] rustls::server::VerifierBuilderError),

	/// The server-certificate verifier couldn't be built from the configured roots.
	#[error("failed to build server certificate verifier")]
	ServerVerifier(#[source] rustls::client::VerifierBuilderError),

	/// Generating a self-signed certificate failed.
	#[cfg(any(feature = "quinn", feature = "noq", feature = "quiche"))]
	#[error(transparent)]
	Rcgen(#[from] rcgen::Error),

	/// The crate was built without a crypto provider, so no TLS is possible.
	#[error("no crypto provider available; enable aws-lc-rs or ring feature")]
	NoCryptoProvider,
}

/// Convenience alias for results produced by this module.
pub type Result<T> = std::result::Result<T, Error>;

/// Parse a hex-encoded SHA-256 certificate fingerprint.
pub fn parse_fingerprint(value: &str) -> Result<[u8; 32]> {
	let bytes = hex::decode(value.trim()).map_err(Error::Fingerprint)?;
	bytes.try_into().map_err(|v: Vec<u8>| Error::FingerprintLength(v.len()))
}

/// Read a PEM file into its list of certificates.
pub(crate) fn read_certs(path: &Path) -> Result<Vec<CertificateDer<'static>>> {
	let file = fs::File::open(path).map_err(Error::Open)?;
	let mut reader = io::BufReader::new(file);
	CertificateDer::pem_reader_iter(&mut reader)
		.collect::<std::result::Result<_, _>>()
		.map_err(Error::Read)
}

/// Load every configured custom root, rejecting a path that contains no certificates.
fn read_roots(paths: &[PathBuf]) -> Result<Vec<CertificateDer<'static>>> {
	let mut roots = Vec::new();
	for path in paths {
		let certs = read_certs(path)?;
		if certs.is_empty() {
			return Err(Error::EmptyRoots(path.clone()));
		}
		roots.extend(certs);
	}
	Ok(roots)
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
	/// replaces them; set `--client-tls-system-roots` to trust both (e.g. to reach a
	/// local relay with a private CA and a remote one with a public CA).
	/// Files are hot reloaded for new connections, retaining the last valid roots
	/// if a rotation is temporarily missing or malformed.
	#[serde(skip_serializing_if = "Vec::is_empty")]
	#[arg(id = "client-tls-root", long = "client-tls-root", env = "MOQ_CLIENT_TLS_ROOT")]
	#[serde_as(as = "serde_with::OneOrMany<_>")]
	pub root: Vec<PathBuf>,

	/// Also trust the platform's native root certificates.
	///
	/// Defaults to enabled only when no `--client-tls-root` is given. Set it
	/// explicitly to trust the system roots alongside any custom roots, or set it
	/// to false to trust only the custom roots. Trusting neither (no custom root
	/// and system roots disabled) is rejected, since verification could never pass.
	#[serde(skip_serializing_if = "Option::is_none")]
	#[arg(
		id = "client-tls-system-roots",
		long = "client-tls-system-roots",
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
	#[arg(
		id = "client-tls-fingerprint",
		long = "client-tls-fingerprint",
		env = "MOQ_CLIENT_TLS_FINGERPRINT"
	)]
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
		id = "client-tls-disable-verify",
		long = "client-tls-disable-verify",
		env = "MOQ_CLIENT_TLS_DISABLE_VERIFY",
		default_missing_value = "true",
		num_args = 0..=1,
		require_equals = true,
		value_parser = clap::value_parser!(bool),
	)]
	pub disable_verify: Option<bool>,

	/// Override the TLS SNI and certificate verification hostname for outbound connections.
	///
	/// When unset, the connect URL's host is used (default behavior). Useful when dialing a
	/// raw IP address but needing to present/verify a DNS name the server certificate covers.
	#[serde(skip_serializing_if = "Option::is_none")]
	#[arg(
		id = "client-tls-host-name",
		long = "client-tls-host-name",
		env = "MOQ_CLIENT_TLS_HOST_NAME"
	)]
	pub host_name: Option<String>,

	/// Deprecated `--tls-*` spellings, folded into the canonical fields above with
	/// a warning. Private and hidden so they stay off the public surface; not a
	/// TOML field (config files use the canonical names).
	#[command(flatten)]
	#[serde(skip)]
	deprecated: Deprecated,
}

/// Holds the deprecated bare `--tls-*` flag spellings (renamed to `--client-tls-*`).
/// Flattened into [`Client`] so they keep parsing; folded into the canonical
/// fields by [`Client::build`] with a deprecation warning. No env (the env names
/// were never renamed) and no TOML.
#[derive(Clone, Default, Debug, clap::Args)]
struct Deprecated {
	#[arg(long = "tls-root", hide = true)]
	root: Vec<PathBuf>,

	#[arg(
		long = "tls-system-roots",
		hide = true,
		default_missing_value = "true",
		num_args = 0..=1,
		require_equals = true,
		value_parser = clap::value_parser!(bool),
	)]
	system_roots: Option<bool>,

	#[arg(long = "tls-fingerprint", hide = true)]
	fingerprint: Vec<String>,

	#[arg(
		long = "tls-disable-verify",
		hide = true,
		default_missing_value = "true",
		num_args = 0..=1,
		require_equals = true,
		value_parser = clap::value_parser!(bool),
	)]
	disable_verify: Option<bool>,
}

/// The last valid contents of a set of custom root files.
///
/// Quiche consumes concrete DER roots per connection, while rustls consumes a
/// verifier. Keeping the parsed roots here gives both paths identical
/// last-known-good behavior when a file is briefly absent or incomplete during
/// rotation.
#[derive(Clone)]
pub(crate) struct CustomRoots {
	paths: Vec<PathBuf>,
	current: Arc<RwLock<Vec<CertificateDer<'static>>>>,
}

impl CustomRoots {
	fn new(paths: Vec<PathBuf>) -> Result<Self> {
		let current = read_roots(&paths)?;
		Ok(Self {
			paths,
			current: Arc::new(RwLock::new(current)),
		})
	}

	fn load(&self) -> Result<Vec<CertificateDer<'static>>> {
		read_roots(&self.paths)
	}

	fn replace(&self, roots: Vec<CertificateDer<'static>>) {
		*self.current.write().unwrap_or_else(std::sync::PoisonError::into_inner) = roots;
	}

	pub(crate) fn current(&self) -> Vec<CertificateDer<'static>> {
		self.current
			.read()
			.unwrap_or_else(std::sync::PoisonError::into_inner)
			.clone()
	}

	/// Refresh from disk, retaining and returning the last valid roots on failure.
	#[cfg(feature = "quiche")]
	pub(crate) fn refresh(&self) -> Vec<CertificateDer<'static>> {
		match self.load() {
			Ok(roots) => {
				self.replace(roots.clone());
				roots
			}
			Err(err) => {
				tracing::warn!(%err, "failed to reload client root certificates; retaining previous roots");
				self.current()
			}
		}
	}
}

#[cfg(feature = "watch")]
struct ReloadState<T: ?Sized + Send + Sync + 'static> {
	current: RwLock<Arc<T>>,
	build: Box<dyn Fn() -> Result<Arc<T>> + Send + Sync>,
	role: &'static str,
}

#[cfg(feature = "watch")]
impl<T: ?Sized + Send + Sync + 'static> ReloadState<T> {
	fn reload(&self) {
		match (self.build)() {
			Ok(next) => {
				*self.current.write().unwrap_or_else(std::sync::PoisonError::into_inner) = next;
				tracing::info!(role = self.role, "reloaded TLS root certificates");
			}
			Err(err) => {
				tracing::warn!(%err, role = self.role, "failed to reload TLS root certificates; retaining previous roots");
			}
		}
	}

	fn current(&self) -> Arc<T> {
		self.current
			.read()
			.unwrap_or_else(std::sync::PoisonError::into_inner)
			.clone()
	}
}

/// A verifier whose implementation is replaced after its root files change.
#[cfg(feature = "watch")]
struct Reloading<T: ?Sized + Send + Sync + 'static> {
	state: Arc<ReloadState<T>>,
	// Holds the OS watcher alive for exactly as long as the TLS config.
	_watcher: Option<notify::RecommendedWatcher>,
}

#[cfg(feature = "watch")]
impl<T: ?Sized + Send + Sync + 'static> Reloading<T> {
	fn new(
		paths: &[PathBuf],
		initial: Arc<T>,
		role: &'static str,
		build: impl Fn() -> Result<Arc<T>> + Send + Sync + 'static,
	) -> Self {
		let state = Arc::new(ReloadState {
			current: RwLock::new(initial),
			build: Box::new(build),
			role,
		});

		let reload = state.clone();
		let watcher = match crate::watch::callback(paths, move || reload.reload()) {
			Ok(watcher) => Some(watcher),
			Err(err) => {
				tracing::error!(%err, role, "failed to watch TLS root certificates; hot reload disabled");
				None
			}
		};

		Self {
			state,
			_watcher: watcher,
		}
	}

	fn current(&self) -> Arc<T> {
		self.state.current()
	}
}

#[cfg(feature = "watch")]
impl<T: ?Sized + Send + Sync + 'static> std::fmt::Debug for Reloading<T> {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("Reloading").field("role", &self.state.role).finish()
	}
}

/// The resolved server-certificate verification policy.
///
/// Computed once by [Client::verification] and shared by every backend (the
/// rustls-based quinn/noq via [Client::build], and quiche directly) so they
/// agree on precedence, the system-roots default, and which flag combinations
/// are valid.
#[derive(Clone)]
pub(crate) enum Verification {
	/// No verification at all. Insecure; only via `--client-tls-disable-verify`.
	Disabled,

	/// Pin the leaf certificate by SHA-256. The CA chain is not consulted, so
	/// this is mutually exclusive with any roots.
	Fingerprints(Vec<[u8; 32]>),

	/// Standard CA verification. When `system` is set the platform/default trust
	/// store is trusted too; each backend resolves that its own way (the rustls
	/// backends use the OS platform verifier, quiche loads the native roots).
	/// `custom` are extra PEM roots trusted in addition.
	Roots { custom: CustomRoots, system: bool },
}

impl Client {
	/// Log a warning for each deprecated `--tls-*` flag in use. Called once from
	/// [`Self::verification`], which every backend runs, so a deprecated flag warns once.
	pub(crate) fn warn_deprecated(&self) {
		if !self.deprecated.root.is_empty() {
			tracing::warn!("--tls-root is deprecated; use --client-tls-root");
		}
		if self.deprecated.system_roots.is_some() {
			tracing::warn!("--tls-system-roots is deprecated; use --client-tls-system-roots");
		}
		if !self.deprecated.fingerprint.is_empty() {
			tracing::warn!("--tls-fingerprint is deprecated; use --client-tls-fingerprint");
		}
		if self.deprecated.disable_verify.is_some() {
			tracing::warn!("--tls-disable-verify is deprecated; use --client-tls-disable-verify");
		}
	}

	/// Roots from the canonical field plus the deprecated `--tls-root` spelling.
	pub(crate) fn effective_root(&self) -> Vec<PathBuf> {
		let mut root = self.root.clone();
		root.extend(self.deprecated.root.iter().cloned());
		root
	}

	/// Fingerprints from the canonical field plus the deprecated `--tls-fingerprint`.
	pub(crate) fn effective_fingerprint(&self) -> Vec<String> {
		let mut fp = self.fingerprint.clone();
		fp.extend(self.deprecated.fingerprint.iter().cloned());
		fp
	}

	/// `system_roots`, preferring the canonical flag over the deprecated alias.
	pub(crate) fn effective_system_roots(&self) -> Option<bool> {
		self.system_roots.or(self.deprecated.system_roots)
	}

	/// `disable_verify`, preferring the canonical flag over the deprecated alias.
	pub(crate) fn effective_disable_verify(&self) -> Option<bool> {
		self.disable_verify.or(self.deprecated.disable_verify)
	}

	/// Resolve the verification policy from the configured flags.
	///
	/// Precedence and rules (shared by all backends):
	/// - `--client-tls-disable-verify` disables verification, and combining it with any
	///   trust material is rejected rather than silently ignoring that material.
	/// - `--client-tls-fingerprint` pins the leaf and bypasses the CA chain; combining
	///   it with `--client-tls-root` or `--client-tls-system-roots` is rejected rather than
	///   silently ignoring one of them.
	/// - Otherwise, verify against the system roots (default) plus any custom
	///   roots. The system roots are dropped once a custom root is given unless
	///   `--client-tls-system-roots` re-enables them.
	///
	/// Every combination that would quietly drop one setting is an error. Silently
	/// weakening trust is the worst outcome here: someone moving off
	/// `disable_verify` by adding a fingerprint would otherwise still accept every
	/// certificate, with the UI showing the pin as configured.
	pub(crate) fn verification(&self) -> Result<Verification> {
		self.warn_deprecated();

		let fingerprints = self.fingerprints()?;
		let roots = self.effective_root();
		let system_roots = self.effective_system_roots();

		if self.effective_disable_verify().unwrap_or_default() {
			if !fingerprints.is_empty() || !roots.is_empty() || system_roots == Some(true) {
				return Err(Error::DisableVerifyWithTrust);
			}
			return Ok(Verification::Disabled);
		}

		if !fingerprints.is_empty() {
			if !roots.is_empty() || system_roots == Some(true) {
				return Err(Error::FingerprintWithRoots);
			}
			return Ok(Verification::Fingerprints(fingerprints));
		}

		// Default to system roots only when no custom root is given, so passing a
		// root replaces them unless the system roots are explicitly re-enabled.
		let system = system_roots.unwrap_or(roots.is_empty());

		let custom = CustomRoots::new(roots)?;

		// WebPKI needs at least one trusted root to ever succeed, so fail fast
		// instead of producing confusing handshake errors later. With system
		// trust enabled the verifier supplies its own roots, so custom roots are
		// optional.
		if !system && custom.current().is_empty() {
			return Err(Error::NoRoots);
		}

		Ok(Verification::Roots { custom, system })
	}

	/// Whether an insecure `http://` certificate-fingerprint bootstrap may be
	/// honored for a connection.
	///
	/// Only when no stronger verification is configured: an explicit
	/// `--client-tls-fingerprint` must never be weakened by an attacker-controlled
	/// plaintext fetch, and there is nothing to bootstrap when verification is
	/// disabled. With CA roots (the default), `http://` is the deliberate
	/// per-connection way to pin a self-signed relay, so it is allowed.
	#[cfg(any(feature = "noq", feature = "quinn", feature = "quiche"))]
	pub(crate) fn allows_http_bootstrap(&self) -> bool {
		self.effective_fingerprint().is_empty() && !self.effective_disable_verify().unwrap_or_default()
	}

	/// Parse the configured fingerprints into fixed-size SHA-256 digests.
	fn fingerprints(&self) -> Result<Vec<[u8; 32]>> {
		self.effective_fingerprint()
			.iter()
			.map(|fp| parse_fingerprint(fp))
			.collect()
	}

	/// Build a [`rustls::ClientConfig`] from this configuration.
	///
	/// Resolves the verification policy, optionally attaches a client identity
	/// for mTLS, and installs the matching verifier.
	pub fn build(&self) -> Result<rustls::ClientConfig> {
		let provider = crypto::provider();
		let verification = self.verification()?;
		let reloadable_roots = cfg!(feature = "watch")
			&& matches!(&verification, Verification::Roots { custom, .. } if !custom.paths.is_empty());

		// Allow TLS 1.2 in addition to 1.3 for WebSocket compatibility.
		// QUIC always negotiates TLS 1.3 regardless of this setting.
		let builder = rustls::ClientConfig::builder_with_provider(provider.clone())
			.with_protocol_versions(&[&rustls::version::TLS13, &rustls::version::TLS12])?;

		let verifier: Arc<dyn rustls::client::danger::ServerCertVerifier> = match verification {
			Verification::Disabled => {
				tracing::warn!(
					"TLS server certificate verification is disabled; A man-in-the-middle attack is possible."
				);
				Arc::new(NoCertificateVerification(provider))
			}
			Verification::Fingerprints(fingerprints) => {
				let fingerprints = fingerprints.into_iter().map(|fp| fp.to_vec()).collect();
				Arc::new(FingerprintVerifier::new(provider, fingerprints))
			}
			Verification::Roots { custom, system } => Self::root_server_verifier(custom, system, provider)?,
		};

		let builder = builder.dangerous().with_custom_certificate_verifier(verifier);
		let mut tls = self.with_client_auth(builder)?;

		// A resumed session skips certificate verification. Disable resumption when
		// roots can change underneath this config so removing a CA takes effect on
		// every subsequent handshake.
		if reloadable_roots {
			tls.resumption = rustls::client::Resumption::disabled();
		}

		Ok(tls)
	}

	/// Build a reloadable verifier for custom roots and system/default trust.
	///
	/// Uses the OS-native platform verifier (Keychain/SecTrust, Windows
	/// CryptoAPI, or the native store on Linux) everywhere it works, optionally
	/// extended with `custom` PEM roots. Android's platform verifier needs JNI
	/// setup (see [`init_android`]); until that has run we trust the bundled
	/// Mozilla roots so verification still works out of the box.
	fn root_server_verifier(
		custom: CustomRoots,
		system: bool,
		provider: crypto::Provider,
	) -> Result<Arc<dyn rustls::client::danger::ServerCertVerifier>> {
		let initial = Self::build_root_server_verifier(&custom.current(), system, &provider)?;

		#[cfg(feature = "watch")]
		if !custom.paths.is_empty() {
			let paths = custom.paths.clone();
			let reload = custom.clone();
			let reload_provider = provider.clone();
			let verifier = ReloadingServerVerifier::new(&paths, initial, move || {
				let roots = reload.load()?;
				let verifier = Self::build_root_server_verifier(&roots, system, &reload_provider)?;
				reload.replace(roots);
				Ok(verifier)
			});
			return Ok(Arc::new(verifier));
		}

		Ok(initial)
	}

	fn build_root_server_verifier(
		custom: &[CertificateDer<'static>],
		system: bool,
		provider: &crypto::Provider,
	) -> Result<Arc<dyn rustls::client::danger::ServerCertVerifier>> {
		if !system {
			let roots = root_store(custom)?;
			let verifier =
				rustls::client::WebPkiServerVerifier::builder_with_provider(Arc::new(roots), provider.clone())
					.build()
					.map_err(Error::ServerVerifier)?;
			return Ok(verifier);
		}

		// Android's platform verifier needs JNI init (see `init_android`) and,
		// unlike the other platforms, can't be extended with custom roots. So use
		// it only once initialized and with no custom roots; otherwise trust the
		// bundled Mozilla roots (plus any custom roots) so verification still works.
		#[cfg(target_os = "android")]
		{
			if ANDROID_INITIALIZED.load(std::sync::atomic::Ordering::Acquire) && custom.is_empty() {
				let verifier = rustls_platform_verifier::Verifier::new(provider.clone())?;
				return Ok(Arc::new(verifier));
			}

			let mut roots = rustls::RootCertStore::empty();
			roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
			for cert in custom {
				roots.add(cert.clone()).map_err(Error::AddRoot)?;
			}
			let verifier =
				rustls::client::WebPkiServerVerifier::builder_with_provider(Arc::new(roots), provider.clone())
					.build()
					.map_err(Error::ServerVerifier)?;
			Ok(verifier)
		}

		#[cfg(not(target_os = "android"))]
		{
			let verifier = if custom.is_empty() {
				rustls_platform_verifier::Verifier::new(provider.clone())?
			} else {
				rustls_platform_verifier::Verifier::new_with_extra_roots(custom.iter().cloned(), provider.clone())?
			};
			Ok(Arc::new(verifier))
		}
	}

	/// Attach the optional mTLS client identity, finishing the rustls builder.
	fn with_client_auth(
		&self,
		builder: rustls::ConfigBuilder<rustls::ClientConfig, rustls::client::WantsClientCert>,
	) -> Result<rustls::ClientConfig> {
		Ok(match (&self.cert, &self.key) {
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
		})
	}
}

/// Build a [`rustls::RootCertStore`] from a list of custom PEM roots.
fn root_store(custom: &[CertificateDer<'static>]) -> Result<rustls::RootCertStore> {
	let mut roots = rustls::RootCertStore::empty();
	for cert in custom {
		roots.add(cert.clone()).map_err(Error::AddRoot)?;
	}
	Ok(roots)
}

/// Whether [`init_android`] has successfully wired up the platform verifier.
#[cfg(target_os = "android")]
static ANDROID_INITIALIZED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Initialize Android platform certificate verification.
///
/// On Android the OS trust store is only reachable through the JVM, so the
/// platform verifier needs a JNI handle to the application `Context` before it
/// can be used. Call this once at startup (e.g. from `JNI_OnLoad`) with an
/// attached [`jni::Env`] for the calling thread and the application `Context`.
/// The `moq-ffi` bindings call it automatically, so most consumers never touch
/// this directly.
///
/// Until it succeeds, clients fall back to the bundled Mozilla roots, so a
/// missing or failed init degrades to webpki verification rather than failing.
#[cfg(target_os = "android")]
pub fn init_android(env: &mut jni::Env, context: jni::objects::JObject) -> Result<()> {
	rustls_platform_verifier::android::init_with_env(env, context).map_err(Error::AndroidInit)?;
	ANDROID_INITIALIZED.store(true, std::sync::atomic::Ordering::Release);
	Ok(())
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
	/// Plain-TLS listeners built via [`Self::server_config`] also use these roots
	/// for optional mTLS. Root files are hot reloaded for new handshakes on the
	/// rustls-based backends; quiche servers require a restart because their TLS
	/// hook fixes client-auth roots when the listener is built.
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
	/// Disable cached client authentication when client roots can reload.
	#[cfg(feature = "watch")]
	pub(crate) fn disable_resumption(&self, tls: &mut rustls::ServerConfig) {
		if !self.root.is_empty() {
			tls.session_storage = Arc::new(rustls::server::NoServerSessionStorage {});
			tls.send_tls13_tickets = 0;
		}
	}

	/// Load all configured root CAs into a [`rustls::RootCertStore`].
	pub fn load_roots(&self) -> Result<rustls::RootCertStore> {
		root_store(&read_roots(&self.root)?)
	}

	/// Build the optional-client-auth verifier, reloading configured roots in place.
	#[cfg(any(feature = "quinn", feature = "noq", feature = "quiche"))]
	pub(crate) fn client_verifier(
		&self,
		provider: crypto::Provider,
	) -> Result<Arc<dyn rustls::server::danger::ClientCertVerifier>> {
		let initial = Self::build_client_verifier(&self.root, &provider)?;

		#[cfg(feature = "watch")]
		{
			let paths = self.root.clone();
			let reload_paths = paths.clone();
			let reload_provider = provider.clone();
			let verifier = ReloadingClientVerifier::new(&paths, initial, move || {
				Self::build_client_verifier(&reload_paths, &reload_provider)
			});
			Ok(Arc::new(verifier))
		}

		#[cfg(not(feature = "watch"))]
		Ok(initial)
	}

	#[cfg(any(feature = "quinn", feature = "noq", feature = "quiche"))]
	fn build_client_verifier(
		paths: &[PathBuf],
		provider: &crypto::Provider,
	) -> Result<Arc<dyn rustls::server::danger::ClientCertVerifier>> {
		let roots = root_store(&read_roots(paths)?)?;
		rustls::server::WebPkiClientVerifier::builder_with_provider(Arc::new(roots), provider.clone())
			.allow_unauthenticated()
			.build()
			.map_err(Error::ClientVerifier)
	}

	/// Build a [`rustls::ServerConfig`] for a plain-TLS (non-QUIC) server, e.g. an
	/// RTMPS or HTTPS listener fronting the QUIC endpoint, reusing the QUIC
	/// backend's certificate handling: on-disk `cert`/`key` pairs, `generate`
	/// self-signed certs, and optional mTLS `root` client CAs.
	///
	/// `alpn` sets the advertised ALPN protocols (e.g.
	/// `vec![b"h2".to_vec(), b"http/1.1".to_vec()]`); pass an empty list for a
	/// protocol like RTMPS that doesn't use ALPN.
	#[cfg(any(feature = "noq", feature = "quinn", feature = "quiche"))]
	pub fn server_config(&self, alpn: Vec<Vec<u8>>) -> Result<Arc<rustls::ServerConfig>> {
		server_config(self, alpn)
	}
}

/// Build a [`rustls::ServerConfig`] from a [`Server`] for a plain-TLS listener.
#[cfg(any(feature = "noq", feature = "quinn", feature = "quiche"))]
fn server_config(config: &Server, alpn: Vec<Vec<u8>>) -> Result<Arc<rustls::ServerConfig>> {
	let provider = crypto::provider();

	let certs = ServeCerts::new(provider.clone());
	certs.load_certs(config)?;
	let certs = Arc::new(certs);

	// TCP can negotiate TLS 1.2 as well as 1.3, unlike QUIC which is 1.3-only.
	let builder =
		rustls::ServerConfig::builder_with_provider(provider.clone()).with_safe_default_protocol_versions()?;

	let mut tls = if config.root.is_empty() {
		builder.with_no_client_auth().with_cert_resolver(certs)
	} else {
		let verifier = config.client_verifier(provider)?;
		builder.with_client_cert_verifier(verifier).with_cert_resolver(certs)
	};

	tls.alpn_protocols = alpn;
	config.disable_resumption(&mut tls);
	Ok(Arc::new(tls))
}

/// A peer's validated client-certificate chain from the mTLS handshake.
///
/// Returned by [`crate::Request::peer_identity`] when the peer presented a
/// certificate that chained to a configured [`Server::root`]. Owns the chain
/// (leaf first) so callers can inspect it, e.g. [`expiry`](Self::expiry),
/// without re-parsing the type-erased QUIC identity.
#[derive(Clone)]
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

	/// Wrap a certificate chain already exposed by a QUIC backend.
	#[cfg(feature = "quiche")]
	pub(crate) fn from_chain(chain: Vec<CertificateDer<'static>>) -> Self {
		Self { chain }
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

/// The certificates a server is currently serving.
///
/// Only a QUIC backend serves TLS of its own, so nothing else populates this.
#[cfg(any(feature = "noq", feature = "quinn", feature = "quiche"))]
#[derive(Debug, Default)]
pub(crate) struct Info {
	pub(crate) certs: Vec<Arc<rustls::sign::CertifiedKey>>,
	pub(crate) fingerprints: Vec<String>,
}

/// A live handle to the certificates a [`crate::Server`] is serving.
///
/// Cheap to clone, and every read reflects the latest hot reload of the files on
/// disk, so a caller can build one at startup and hold it for the process
/// lifetime. Obtained from [`crate::Server::certificates`].
#[derive(Clone, Debug)]
pub struct Certificates {
	#[cfg(any(feature = "noq", feature = "quinn", feature = "quiche"))]
	info: Arc<RwLock<Info>>,
}

impl Certificates {
	#[cfg(any(feature = "noq", feature = "quinn", feature = "quiche"))]
	pub(crate) fn new(info: Arc<RwLock<Info>>) -> Self {
		Self { info }
	}

	/// An empty set, used when no TLS-bearing backend is configured.
	pub(crate) fn empty() -> Self {
		Self {
			#[cfg(any(feature = "noq", feature = "quinn", feature = "quiche"))]
			info: Arc::new(RwLock::new(Info::default())),
		}
	}

	/// The SHA-256 fingerprints of the certificates being served right now, hex
	/// encoded, one per certificate and in configuration order.
	///
	/// Empty when the server has no TLS-bearing backend. Re-read this per use
	/// rather than caching it: a cert rotation on disk changes the values.
	pub fn fingerprints(&self) -> Vec<String> {
		#[cfg(any(feature = "noq", feature = "quinn", feature = "quiche"))]
		{
			// A panicking writer can't leave the cert list half-updated (it is
			// replaced wholesale), so a poisoned lock is still safe to read.
			let info = self.info.read().unwrap_or_else(std::sync::PoisonError::into_inner);
			info.fingerprints.clone()
		}
		#[cfg(not(any(feature = "noq", feature = "quinn", feature = "quiche")))]
		Vec::new()
	}
}

/// Delegates each server-certificate check to the latest valid root verifier.
#[cfg(feature = "watch")]
#[derive(Debug)]
struct ReloadingServerVerifier {
	inner: Reloading<dyn rustls::client::danger::ServerCertVerifier>,
}

#[cfg(feature = "watch")]
impl ReloadingServerVerifier {
	fn new(
		paths: &[PathBuf],
		initial: Arc<dyn rustls::client::danger::ServerCertVerifier>,
		build: impl Fn() -> Result<Arc<dyn rustls::client::danger::ServerCertVerifier>> + Send + Sync + 'static,
	) -> Self {
		Self {
			inner: Reloading::new(paths, initial, "client", build),
		}
	}
}

#[cfg(feature = "watch")]
impl rustls::client::danger::ServerCertVerifier for ReloadingServerVerifier {
	fn verify_server_cert(
		&self,
		end_entity: &CertificateDer<'_>,
		intermediates: &[CertificateDer<'_>],
		server_name: &ServerName<'_>,
		ocsp_response: &[u8],
		now: UnixTime,
	) -> std::result::Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
		self.inner
			.current()
			.verify_server_cert(end_entity, intermediates, server_name, ocsp_response, now)
	}

	fn verify_tls12_signature(
		&self,
		message: &[u8],
		cert: &CertificateDer<'_>,
		dss: &rustls::DigitallySignedStruct,
	) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
		self.inner.current().verify_tls12_signature(message, cert, dss)
	}

	fn verify_tls13_signature(
		&self,
		message: &[u8],
		cert: &CertificateDer<'_>,
		dss: &rustls::DigitallySignedStruct,
	) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
		self.inner.current().verify_tls13_signature(message, cert, dss)
	}

	fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
		self.inner.current().supported_verify_schemes()
	}

	fn requires_raw_public_keys(&self) -> bool {
		self.inner.current().requires_raw_public_keys()
	}
}

/// Delegates each client-certificate check to the latest valid root verifier.
#[cfg(all(feature = "watch", any(feature = "quinn", feature = "noq", feature = "quiche")))]
#[derive(Debug)]
struct ReloadingClientVerifier {
	inner: Reloading<dyn rustls::server::danger::ClientCertVerifier>,
}

#[cfg(all(feature = "watch", any(feature = "quinn", feature = "noq", feature = "quiche")))]
impl ReloadingClientVerifier {
	fn new(
		paths: &[PathBuf],
		initial: Arc<dyn rustls::server::danger::ClientCertVerifier>,
		build: impl Fn() -> Result<Arc<dyn rustls::server::danger::ClientCertVerifier>> + Send + Sync + 'static,
	) -> Self {
		Self {
			inner: Reloading::new(paths, initial, "server", build),
		}
	}
}

#[cfg(all(feature = "watch", any(feature = "quinn", feature = "noq", feature = "quiche")))]
impl rustls::server::danger::ClientCertVerifier for ReloadingClientVerifier {
	fn offer_client_auth(&self) -> bool {
		self.inner.current().offer_client_auth()
	}

	fn client_auth_mandatory(&self) -> bool {
		self.inner.current().client_auth_mandatory()
	}

	fn root_hint_subjects(&self) -> &[rustls::DistinguishedName] {
		// A live verifier cannot return a borrowed slice from a replaceable value.
		// Empty hints ask clients to offer any configured identity, which rustls
		// then verifies against the current roots.
		&[]
	}

	fn verify_client_cert(
		&self,
		end_entity: &CertificateDer<'_>,
		intermediates: &[CertificateDer<'_>],
		now: UnixTime,
	) -> std::result::Result<rustls::server::danger::ClientCertVerified, rustls::Error> {
		self.inner.current().verify_client_cert(end_entity, intermediates, now)
	}

	fn verify_tls12_signature(
		&self,
		message: &[u8],
		cert: &CertificateDer<'_>,
		dss: &rustls::DigitallySignedStruct,
	) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
		self.inner.current().verify_tls12_signature(message, cert, dss)
	}

	fn verify_tls13_signature(
		&self,
		message: &[u8],
		cert: &CertificateDer<'_>,
		dss: &rustls::DigitallySignedStruct,
	) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
		self.inner.current().verify_tls13_signature(message, cert, dss)
	}

	fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
		self.inner.current().supported_verify_schemes()
	}

	fn requires_raw_public_keys(&self) -> bool {
		self.inner.current().requires_raw_public_keys()
	}
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
	/// Disabling verification cannot be combined with trust material that it would
	/// otherwise ignore.
	#[test]
	fn disable_verify_rejects_trust_material() {
		let insecure = Client {
			disable_verify: Some(true),
			..Default::default()
		};
		assert!(matches!(insecure.verification(), Ok(Verification::Disabled)));

		let with_fingerprint = Client {
			disable_verify: Some(true),
			fingerprint: vec!["ab".repeat(32)],
			..Default::default()
		};
		assert!(matches!(
			with_fingerprint.verification(),
			Err(Error::DisableVerifyWithTrust)
		));

		let with_root = Client {
			disable_verify: Some(true),
			root: vec!["/tmp/root.pem".into()],
			..Default::default()
		};
		assert!(matches!(with_root.verification(), Err(Error::DisableVerifyWithTrust)));

		let with_system_roots = Client {
			disable_verify: Some(true),
			system_roots: Some(true),
			..Default::default()
		};
		assert!(matches!(
			with_system_roots.verification(),
			Err(Error::DisableVerifyWithTrust)
		));

		let without_system_roots = Client {
			disable_verify: Some(true),
			system_roots: Some(false),
			..Default::default()
		};
		assert!(matches!(
			without_system_roots.verification(),
			Ok(Verification::Disabled)
		));
	}

	use super::*;
	use rustls::client::danger::ServerCertVerifier;
	use rustls::pki_types::ServerName;
	#[cfg(feature = "watch")]
	use rustls::server::danger::ClientCertVerifier;

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

	#[test]
	fn build_rejects_fingerprint_with_roots() {
		let cert = self_signed();
		let fingerprint = hex::encode(crypto::sha256(&crypto::provider(), cert.as_ref()));

		// Fingerprint pinning bypasses the CA chain, so combining it with roots
		// is rejected rather than silently ignoring one of them.
		let with_system = Client {
			fingerprint: vec![fingerprint.clone()],
			system_roots: Some(true),
			..Default::default()
		};
		assert!(matches!(with_system.build(), Err(Error::FingerprintWithRoots)));

		// The conflict is detected before any root file is read, so the path
		// need not exist.
		let with_custom = Client {
			fingerprint: vec![fingerprint],
			root: vec![PathBuf::from("/does-not-exist.pem")],
			..Default::default()
		};
		assert!(matches!(with_custom.build(), Err(Error::FingerprintWithRoots)));
	}

	/// Write a self-signed cert to a temp PEM file, returning the keep-alive
	/// handle alongside its path.
	fn self_signed_root() -> (tempfile::NamedTempFile, PathBuf) {
		use std::io::Write;
		let key = rcgen::KeyPair::generate().unwrap();
		let params = rcgen::CertificateParams::new(vec!["localhost".to_string()]).unwrap();
		let cert = params.self_signed(&key).unwrap();
		let mut file = tempfile::NamedTempFile::new().unwrap();
		file.write_all(cert.pem().as_bytes()).unwrap();
		let path = file.path().to_path_buf();
		(file, path)
	}

	#[cfg(all(feature = "watch", any(feature = "quinn", feature = "noq", feature = "quiche")))]
	fn signed_certificates() -> (
		String,
		CertificateDer<'static>,
		PrivateKeyDer<'static>,
		CertificateDer<'static>,
		PrivateKeyDer<'static>,
	) {
		use rcgen::{BasicConstraints, ExtendedKeyUsagePurpose, IsCa, Issuer};

		let ca_key = rcgen::KeyPair::generate().unwrap();
		let mut ca_params = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
		ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
		let ca = ca_params.self_signed(&ca_key).unwrap();
		let issuer = Issuer::from_params(&ca_params, &ca_key);

		let server_key = rcgen::KeyPair::generate().unwrap();
		let server_key_der = PrivatePkcs8KeyDer::from(server_key.serialize_der());
		let mut server_params = rcgen::CertificateParams::new(vec!["localhost".to_string()]).unwrap();
		server_params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
		let server = server_params.signed_by(&server_key, &issuer).unwrap();

		let client_key = rcgen::KeyPair::generate().unwrap();
		let client_key_der = PrivatePkcs8KeyDer::from(client_key.serialize_der());
		let mut client_params = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
		client_params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
		let client = client_params.signed_by(&client_key, &issuer).unwrap();

		(
			ca.pem(),
			server.into(),
			server_key_der.into(),
			client.into(),
			client_key_der.into(),
		)
	}

	#[cfg(all(feature = "watch", any(feature = "quinn", feature = "noq", feature = "quiche")))]
	fn handshake_kinds(
		client: Arc<rustls::ClientConfig>,
		server: Arc<rustls::ServerConfig>,
	) -> std::result::Result<(rustls::HandshakeKind, rustls::HandshakeKind), rustls::Error> {
		let name = ServerName::try_from("localhost").unwrap();
		let mut client = rustls::ClientConnection::new(client, name).unwrap();
		let mut server = rustls::ServerConnection::new(server).unwrap();

		for _ in 0..100 {
			let mut client_data = Vec::new();
			client.write_tls(&mut client_data).unwrap();
			if !client_data.is_empty() {
				server.read_tls(&mut client_data.as_slice()).unwrap();
				server.process_new_packets()?;
			}

			let mut server_data = Vec::new();
			server.write_tls(&mut server_data).unwrap();
			if !server_data.is_empty() {
				client.read_tls(&mut server_data.as_slice()).unwrap();
				client.process_new_packets()?;
			}

			if !client.is_handshaking() && !server.is_handshaking() && !client.wants_write() && !server.wants_write() {
				return Ok((client.handshake_kind().unwrap(), server.handshake_kind().unwrap()));
			}
		}

		panic!("TLS handshake did not settle");
	}

	#[cfg(all(feature = "watch", any(feature = "quinn", feature = "noq", feature = "quiche")))]
	#[test]
	fn reloadable_roots_disable_session_resumption() {
		use std::io::Write;

		let (ca, server_cert, server_key, _, _) = signed_certificates();
		let mut root_file = tempfile::NamedTempFile::new().unwrap();
		root_file.write_all(ca.as_bytes()).unwrap();
		let roots = read_roots(&[root_file.path().to_path_buf()]).unwrap();
		let provider = crypto::provider();

		let control = rustls::ClientConfig::builder_with_provider(provider.clone())
			.with_safe_default_protocol_versions()
			.unwrap()
			.with_root_certificates(root_store(&roots).unwrap())
			.with_no_client_auth();
		let reloadable = Client {
			root: vec![root_file.path().to_path_buf()],
			..Default::default()
		}
		.build()
		.unwrap();
		let server = rustls::ServerConfig::builder_with_provider(provider)
			.with_safe_default_protocol_versions()
			.unwrap()
			.with_no_client_auth()
			.with_single_cert(vec![server_cert], server_key)
			.unwrap();
		let server = Arc::new(server);

		let control = Arc::new(control);
		assert_eq!(
			handshake_kinds(control.clone(), server.clone()).unwrap().0,
			rustls::HandshakeKind::Full
		);
		assert_eq!(
			handshake_kinds(control, server.clone()).unwrap().0,
			rustls::HandshakeKind::Resumed
		);

		let reloadable = Arc::new(reloadable);
		assert_eq!(
			handshake_kinds(reloadable.clone(), server.clone()).unwrap().0,
			rustls::HandshakeKind::Full
		);
		assert_eq!(
			handshake_kinds(reloadable, server).unwrap().0,
			rustls::HandshakeKind::Full
		);
	}

	#[cfg(all(feature = "watch", any(feature = "quinn", feature = "noq", feature = "quiche")))]
	#[test]
	fn reloadable_client_roots_disable_server_resumption() {
		use std::io::Write;

		let (ca_a, server_cert, server_key, client_cert, client_key) = signed_certificates();
		let (ca_b, _, _, _, _) = signed_certificates();
		let mut root_file = tempfile::NamedTempFile::new().unwrap();
		root_file.write_all(ca_a.as_bytes()).unwrap();
		let paths = vec![root_file.path().to_path_buf()];
		let provider = crypto::provider();

		let build_client = |cert, key| {
			rustls::ClientConfig::builder_with_provider(provider.clone())
				.with_safe_default_protocol_versions()
				.unwrap()
				.dangerous()
				.with_custom_certificate_verifier(Arc::new(NoCertificateVerification(provider.clone())))
				.with_client_auth_cert(vec![cert], key)
				.unwrap()
		};
		let control_client = Arc::new(build_client(client_cert.clone(), client_key.clone_key()));
		let reloadable_client = Arc::new(build_client(client_cert, client_key));

		let verifier = Server::build_client_verifier(&paths, &provider).unwrap();
		let control = rustls::ServerConfig::builder_with_provider(provider.clone())
			.with_safe_default_protocol_versions()
			.unwrap()
			.with_client_cert_verifier(verifier)
			.with_single_cert(vec![server_cert.clone()], server_key.clone_key())
			.unwrap();

		let initial = Server::build_client_verifier(&paths, &provider).unwrap();
		let reload_paths = paths.clone();
		let reload_provider = provider.clone();
		let verifier = Arc::new(ReloadingClientVerifier::new(&paths, initial, move || {
			Server::build_client_verifier(&reload_paths, &reload_provider)
		}));
		let reload = verifier.inner.state.clone();
		let mut reloadable = rustls::ServerConfig::builder_with_provider(provider)
			.with_safe_default_protocol_versions()
			.unwrap()
			.with_client_cert_verifier(verifier)
			.with_single_cert(vec![server_cert], server_key)
			.unwrap();
		Server {
			root: paths,
			..Default::default()
		}
		.disable_resumption(&mut reloadable);

		let control = Arc::new(control);
		assert_eq!(
			handshake_kinds(control_client.clone(), control.clone()).unwrap().1,
			rustls::HandshakeKind::Full
		);
		assert_eq!(
			handshake_kinds(control_client, control).unwrap().1,
			rustls::HandshakeKind::Resumed
		);

		let reloadable = Arc::new(reloadable);
		assert_eq!(
			handshake_kinds(reloadable_client.clone(), reloadable.clone())
				.unwrap()
				.1,
			rustls::HandshakeKind::Full
		);

		std::fs::write(root_file.path(), ca_b).unwrap();
		reload.reload();
		assert!(handshake_kinds(reloadable_client, reloadable).is_err());
	}

	#[cfg(all(feature = "watch", any(feature = "quinn", feature = "noq", feature = "quiche")))]
	#[test]
	fn custom_roots_reload_for_new_client_and_server_handshakes() {
		use std::io::Write;

		let (ca_a, server_a, _, client_a, _) = signed_certificates();
		let (ca_b, server_b, _, client_b, _) = signed_certificates();
		let mut root_file = tempfile::NamedTempFile::new().unwrap();
		root_file.write_all(ca_a.as_bytes()).unwrap();
		let paths = vec![root_file.path().to_path_buf()];
		let provider = crypto::provider();

		let custom = CustomRoots::new(paths.clone()).unwrap();
		let initial = Client::build_root_server_verifier(&custom.current(), false, &provider).unwrap();
		let reload_custom = custom.clone();
		let reload_provider = provider.clone();
		let server_verifier = ReloadingServerVerifier::new(&paths, initial, move || {
			let roots = reload_custom.load()?;
			let verifier = Client::build_root_server_verifier(&roots, false, &reload_provider)?;
			reload_custom.replace(roots);
			Ok(verifier)
		});

		let initial = Server::build_client_verifier(&paths, &provider).unwrap();
		let reload_paths = paths.clone();
		let reload_provider = provider.clone();
		let client_verifier = ReloadingClientVerifier::new(&paths, initial, move || {
			Server::build_client_verifier(&reload_paths, &reload_provider)
		});

		let name = ServerName::try_from("localhost").unwrap();
		let now = UnixTime::now();
		assert!(
			server_verifier
				.verify_server_cert(&server_a, &[], &name, &[], now)
				.is_ok()
		);
		assert!(
			server_verifier
				.verify_server_cert(&server_b, &[], &name, &[], now)
				.is_err()
		);
		assert!(client_verifier.verify_client_cert(&client_a, &[], now).is_ok());
		assert!(client_verifier.verify_client_cert(&client_b, &[], now).is_err());

		std::fs::write(root_file.path(), ca_b).unwrap();
		server_verifier.inner.state.reload();
		client_verifier.inner.state.reload();

		assert!(
			server_verifier
				.verify_server_cert(&server_a, &[], &name, &[], now)
				.is_err()
		);
		assert!(client_verifier.verify_client_cert(&client_a, &[], now).is_err());

		// A malformed replacement must not erase the last valid verifier.
		std::fs::write(root_file.path(), "not a PEM certificate").unwrap();
		server_verifier.inner.state.reload();
		client_verifier.inner.state.reload();
		assert!(
			server_verifier
				.verify_server_cert(&server_b, &[], &name, &[], now)
				.is_ok()
		);
		assert!(client_verifier.verify_client_cert(&client_b, &[], now).is_ok());
	}

	#[cfg(all(feature = "quiche", feature = "watch"))]
	#[test]
	fn custom_root_refresh_retains_last_valid_bundle() {
		use std::io::Write;

		let (ca_a, _, _, _, _) = signed_certificates();
		let (ca_b, _, _, _, _) = signed_certificates();
		let mut root_file = tempfile::NamedTempFile::new().unwrap();
		root_file.write_all(ca_a.as_bytes()).unwrap();
		let roots = CustomRoots::new(vec![root_file.path().to_path_buf()]).unwrap();
		let initial = roots.current();

		std::fs::write(root_file.path(), "not a PEM certificate").unwrap();
		assert_eq!(roots.refresh(), initial);

		std::fs::write(root_file.path(), ca_b).unwrap();
		let rotated = roots.refresh();
		assert_ne!(rotated, initial);
		assert_eq!(roots.current(), rotated);
	}

	#[test]
	fn build_uses_platform_verifier_by_default() {
		// No custom roots, system trust on: resolves to the OS platform verifier
		// (bundled Mozilla roots on Android) and must build cleanly everywhere.
		assert!(Client::default().build().is_ok());
	}

	#[test]
	fn build_with_custom_roots_only() {
		// A custom root with system trust left at its default disables the system
		// roots, verifying against the custom PEM alone.
		let (_keep, path) = self_signed_root();
		let config = Client {
			root: vec![path],
			..Default::default()
		};
		assert!(config.build().is_ok());
	}

	#[test]
	fn build_with_custom_and_system_roots() {
		// Custom roots layered on top of system trust: exercises the platform
		// verifier's extra-roots path (or the bundled roots plus custom on Android).
		let (_keep, path) = self_signed_root();
		let config = Client {
			root: vec![path],
			system_roots: Some(true),
			..Default::default()
		};
		assert!(config.build().is_ok());
	}
}

// ── ServeCerts ──────────────────────────────────────────────────────

#[cfg(any(feature = "quinn", feature = "noq", feature = "quiche"))]
#[derive(Debug)]
pub(crate) struct ServeCerts {
	pub info: Arc<RwLock<Info>>,
	provider: crypto::Provider,
}

#[cfg(any(feature = "quinn", feature = "noq", feature = "quiche"))]
impl ServeCerts {
	pub fn new(provider: crypto::Provider) -> Self {
		Self {
			info: Arc::new(RwLock::new(Info::default())),
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

#[cfg(any(feature = "quinn", feature = "noq", feature = "quiche"))]
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
