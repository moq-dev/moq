//! rind's cryptographic provider for dimpl.
//!
//! Ported from kixel-ai/rind's `rind-dimpl` 0.1.12, trimmed to DTLS 1.2, with
//! dimpl's PSK AES-128-CCM-8 suite kept from the provider it replaces.

mod digest;
mod kx;
mod sign;
mod suite;

use super::CryptoProvider;

#[cfg(test)]
pub(crate) use digest::HMAC;

/// Returns a dimpl [`CryptoProvider`] backed by rind, or the CPU feature rind
/// requires that this CPU lacks.
///
/// It negotiates DTLS 1.2 ECDHE-ECDSA with AES-128-GCM, AES-256-GCM, and
/// ChaCha20-Poly1305 over X25519, P-256, and P-384, plus PSK with
/// AES-128-CCM-8, and signs with ECDSA P-256 or P-384 keys in PKCS#8 or SEC1
/// DER. It prefers ChaCha20-Poly1305 where rind runs AES without CPU
/// instructions.
pub fn default_provider() -> Result<CryptoProvider, rind::cpu::MissingFeature> {
	rind::cpu::check()?;
	let cipher_suites = if rind::cpu::has_aes_hardware() {
		suite::DTLS12
	} else {
		suite::DTLS12_CHACHA20_FIRST
	};
	Ok(CryptoProvider {
		kx_groups: kx::ALL,
		signature_verification: &sign::VERIFIER,
		key_provider: &sign::KEYS,
		secure_random: &digest::RANDOM,
		hash_provider: &digest::HASH,
		hmac_provider: &digest::HMAC,
		cipher_suites,
	})
}
