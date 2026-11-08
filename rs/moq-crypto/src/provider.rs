//! rind's rustls cryptography provider.
//!
//! rind's primitives panic on a CPU missing a feature they require, so each entry point
//! here refuses such a CPU with [`crate::cpu::UnsupportedCpu`] instead. The [`crate::cpu`]
//! module owns the shared error reported by transport crates, keeping rind out of their APIs.

use std::sync::{Arc, OnceLock};

use crate::cpu::UnsupportedCpu;

/// A shared rustls crypto provider.
pub type Provider = Arc<rustls::crypto::CryptoProvider>;

/// Return rind's rustls provider, refusing a CPU missing a required feature.
///
/// The process default is not consulted, so TLS works whether or not one was installed.
pub fn provider() -> Result<Provider, UnsupportedCpu> {
	// CPU features never change while the process runs, so the first answer is the answer.
	static PROVIDER: OnceLock<Result<Provider, UnsupportedCpu>> = OnceLock::new();
	PROVIDER
		.get_or_init(|| rind_rustls::default_provider().map(Arc::new).map_err(UnsupportedCpu))
		.clone()
}

/// Install rind as rustls' process default, for consumers such as reqwest that read it.
///
/// Succeeds without replacing a provider the process already installed, which it is
/// entitled to; only an unsupported CPU fails.
pub fn install_default() -> Result<(), UnsupportedCpu> {
	let _ = Arc::unwrap_or_clone(provider()?).install_default();
	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn provider_is_shared() {
		// The test hosts run rind, so the provider builds and every call returns the same one.
		let first = provider().unwrap();
		assert!(Arc::ptr_eq(&first, &provider().unwrap()));
		crate::cpu::check().unwrap();
		install_default().unwrap();
		// A second install keeps the first provider and still succeeds.
		install_default().unwrap();
	}
}
