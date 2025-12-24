use rustls::crypto::hash::{self, HashAlgorithm};
use std::sync::Arc;

pub type Provider = Arc<rustls::crypto::CryptoProvider>;

pub fn provider() -> Provider {
	if let Some(provider) = rustls::crypto::CryptoProvider::get_default().cloned() {
		return provider;
	}
	#[cfg(all(feature = "aws-lc-rs", not(feature = "ring")))]
	return Arc::new(rustls::crypto::aws_lc_rs::default_provider());
	#[cfg(all(feature = "ring", not(feature = "aws-lc-rs")))]
	return Arc::new(rustls::crypto::ring::default_provider());
	#[allow(unreachable_code)]
	{
		panic!("no CryptoProvider available; install_default() or enable either the aws-lc-rs or ring feature");
	}
}

/// Helper function to compute SHA256 hash using the crypto provider
///
/// This function tries to find a SHA256 hash implementation in the provided
/// crypto provider's cipher suites. If not found, it falls back to direct
/// implementations based on enabled features.
pub fn sha256(provider: &Provider, data: &[u8]) -> hash::Output {
	// Try to find a SHA-256 hash provider from the cipher suites
	let hash_provider = provider.cipher_suites.iter().find_map(|suite| {
		let hash_provider = suite.tls13()?.common.hash_provider;
		if hash_provider.algorithm() == HashAlgorithm::SHA256 {
			Some(hash_provider)
		} else {
			None
		}
	});

	// If a hash provider is found, use it
	if let Some(hash_provider) = hash_provider {
		return hash_provider.hash(data);
	}

	panic!("SHA-256 hash provider not found");
}
