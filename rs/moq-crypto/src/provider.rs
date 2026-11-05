//! rind, MoQ's one cryptography provider: the CPU check and its rustls provider.
//!
//! rind's primitives panic on a CPU missing a feature they require, so each entry point
//! here refuses such a CPU with [`UnsupportedCpu`] instead. The transport crates report
//! this one type, and rind stays out of their public APIs.

use std::sync::{Arc, OnceLock};

/// A shared rustls crypto provider.
pub type Provider = Arc<rustls::crypto::CryptoProvider>;

/// This CPU lacks a feature that rind requires, named in the message.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("unsupported CPU: {0}")]
pub struct UnsupportedCpu(rind::cpu::MissingFeature);

/// Refuse a CPU that rind cannot run on, naming the missing feature.
///
/// Call this before using rind's primitives directly; [`provider`] and
/// [`install_default`] run it themselves.
pub fn check() -> Result<(), UnsupportedCpu> {
	rind::cpu::check().map_err(UnsupportedCpu)
}

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
		check().unwrap();
		install_default().unwrap();
		// A second install keeps the first provider and still succeeds.
		install_default().unwrap();
	}
}
