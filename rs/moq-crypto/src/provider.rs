//! rind's rustls cryptography provider.

use std::sync::{Arc, LazyLock};

/// A shared rustls crypto provider.
pub type Provider = Arc<rustls::crypto::CryptoProvider>;

/// Return rind's rustls provider.
///
/// The process default is not consulted, so TLS works whether or not one was installed.
pub fn provider() -> Provider {
	static PROVIDER: LazyLock<Provider> = LazyLock::new(|| Arc::new(rind_rustls::default_provider()));
	PROVIDER.clone()
}

/// Install rind as rustls' process default, for consumers such as reqwest that read it.
///
/// Keeps a provider the process already installed, which it is entitled to.
pub fn install_default() {
	let _ = Arc::unwrap_or_clone(provider()).install_default();
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn provider_is_shared() {
		assert!(Arc::ptr_eq(&provider(), &provider()));
		install_default();
		// A second install keeps the first provider.
		install_default();
	}
}
