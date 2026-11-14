//! rind's cryptographic provider for str0m.
//!
//! Ported from kixel-ai/rind's `rind-str0m`, trimmed to DTLS 1.2, with its DTLS
//! on dimpl's rind provider.

mod dtls;
mod hash;
mod srtp;

use crate::vendor::str0m_proto::crypto::CryptoProvider;

/// Returns a str0m [`CryptoProvider`] backed by rind, or the CPU feature rind
/// requires that this CPU lacks.
///
/// It protects SRTP with AES-128-CM-HMAC-SHA1-80, AEAD-AES-128-GCM, and
/// AEAD-AES-256-GCM, signs STUN with HMAC-SHA1, hashes certificate
/// fingerprints with SHA-1 and SHA-256, and runs DTLS 1.2 through dimpl with a
/// self-signed ECDSA P-256 certificate.
pub fn default_provider() -> Result<CryptoProvider, rind::cpu::MissingFeature> {
	rind::cpu::check()?;
	Ok(CryptoProvider {
		srtp_provider: &srtp::SRTP,
		sha1_hmac_provider: &hash::SHA1_HMAC,
		sha1_provider: &hash::SHA1,
		sha256_provider: &hash::SHA256,
		dtls_provider: &dtls::DTLS,
	})
}
