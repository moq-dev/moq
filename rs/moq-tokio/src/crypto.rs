//! rind cryptography shared by native MoQ applications.

use std::sync::Arc;

/// A shared rustls crypto provider.
pub type Provider = Arc<rustls::crypto::CryptoProvider>;

/// Return rind's provider, refusing a CPU missing a required feature.
///
/// Panics with the missing feature on unsupported CPUs. TLS configuration checks
/// support first and returns an error instead. The process default is not consulted.
pub fn provider() -> Provider {
	static PROVIDER: std::sync::OnceLock<Provider> = std::sync::OnceLock::new();
	PROVIDER
		.get_or_init(|| Arc::new(rind_rustls::default_provider().unwrap_or_else(|err| panic!("{err}"))))
		.clone()
}

/// Install rind as rustls' process default.
///
/// Panics with the missing feature on unsupported CPUs, like [`provider`].
pub fn install_default() -> Result<(), Provider> {
	Arc::unwrap_or_clone(provider()).install_default()
}

/// Hash certificate bytes independently of the configured TLS cipher suites.
pub(crate) fn sha256(data: &[u8]) -> rind::digest::Digest {
	rind::digest::digest(&rind::digest::SHA256, data)
}
