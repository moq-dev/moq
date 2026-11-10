//! Validation and filtering for crypto providers.
//!
//! This module defines the validation rules for crypto providers used with dimpl,
//! based on the documented support in lib.rs.

use arrayvec::ArrayVec;

use super::{Aad, CryptoProvider, Nonce, SupportedDtls12CipherSuite, SupportedKxGroup};
use crate::vendor::dimpl::buffer::{Buf, TmpBuf};
use crate::vendor::dimpl::types::{HashAlgorithm, NamedGroup, SignatureAlgorithm};
use crate::vendor::dimpl::{ConfigError, CryptoProviderValidationError, Error};

fn provider_error(error: CryptoProviderValidationError) -> Error {
	Error::ConfigError(ConfigError::CryptoProvider(error))
}

impl CryptoProvider {
	/// Returns an iterator over validated cipher suites supported by dimpl.
	///
	/// Only cipher suites documented in lib.rs are returned:
	/// - `TLS_ECDHE_ECDSA_WITH_AES_128_GCM_SHA256`
	/// - `TLS_ECDHE_ECDSA_WITH_AES_256_GCM_SHA384`
	/// - `TLS_ECDHE_ECDSA_WITH_CHACHA20_POLY1305_SHA256`
	pub fn supported_cipher_suites(&self) -> impl Iterator<Item = &'static dyn SupportedDtls12CipherSuite> {
		self.cipher_suites
			.iter()
			.copied()
			.filter(|cs| cs.suite().is_supported())
	}

	/// Returns an iterator over validated key exchange groups supported by dimpl.
	///
	/// Only key exchange groups documented in lib.rs are returned:
	/// - X25519
	/// - P-256 (secp256r1)
	/// - P-384 (secp384r1)
	pub fn supported_kx_groups(&self) -> impl Iterator<Item = &'static dyn SupportedKxGroup> {
		self.kx_groups.iter().copied().filter(|kx| {
			matches!(
				kx.name(),
				NamedGroup::X25519 | NamedGroup::Secp256r1 | NamedGroup::Secp384r1
			)
		})
	}

	///
	/// Combines provider filtering with signature algorithm compatibility.
	pub fn supported_cipher_suites_for_signature_algorithm(
		&self,
		sig_alg: SignatureAlgorithm,
	) -> impl Iterator<Item = &'static dyn SupportedDtls12CipherSuite> {
		self.supported_cipher_suites()
			.filter(move |cs| cs.suite().signature_algorithm() == Some(sig_alg))
	}

	/// Check if provider supports ECDH-based cipher suites.
	///
	/// Returns true if any supported cipher suite uses ECDH key exchange.
	pub fn has_ecdh(&self) -> bool {
		self.supported_cipher_suites().any(|cs| cs.suite().has_ecc())
	}

	/// Validates the provider configuration for use with dimpl.
	///
	/// This ensures the provider meets dimpl's requirements:
	/// - At least one supported cipher suite
	/// - ECDH cipher suites have matching key exchange groups
	/// - Hash providers support required algorithms
	/// - HMAC provider supports required operations
	///
	/// Returns `Error::ConfigError` if validation fails.
	pub fn validate(&self) -> Result<(), Error> {
		self.validate_cipher_suites()?;
		self.validate_kx_groups()?;
		let validated_hashes = self.validate_hash_providers()?;
		self.validate_prf(&validated_hashes)?;
		self.validate_signature_verifier(&validated_hashes)?;
		self.validate_hmac_provider()?;
		self.validate_kx_exchange()?;
		Ok(())
	}

	/// Validate that at least one cipher suite is supported.
	fn validate_cipher_suites(&self) -> Result<(), Error> {
		let cipher_count = self.supported_cipher_suites().count();
		if cipher_count == 0 {
			return Err(provider_error(CryptoProviderValidationError::NoCipherSuites));
		}
		Ok(())
	}

	/// Validate that ECDH cipher suites have matching key exchange groups.
	fn validate_kx_groups(&self) -> Result<(), Error> {
		if self.has_ecdh() {
			let kx_count = self.supported_kx_groups().count();
			if kx_count == 0 {
				return Err(provider_error(
					CryptoProviderValidationError::EcdhCipherSuitesWithoutKeyExchangeGroups,
				));
			}
		}
		Ok(())
	}

	/// Validate that hash providers support required algorithms.
	/// Returns the list of validated hash algorithms.
	fn validate_hash_providers(&self) -> Result<Vec<HashAlgorithm>, Error> {
		// Collect unique hash algorithms from supported cipher suites
		let required_hashes: Vec<HashAlgorithm> = self
			.cipher_suites
			.iter()
			.map(|cs| cs.suite().hash_algorithm())
			.collect();

		// Test each required hash algorithm with known test vectors
		for hash_alg in &required_hashes {
			let mut hasher = self.hash_provider.create_hash(*hash_alg);

			// Test with empty input - use known hash values
			hasher.update(&[]);
			let mut result = Buf::new();
			hasher.clone_and_finalize(&mut result);

			let maybe_expected = HASH_TEST_VECTORS.iter().find(|(h, _)| *h == *hash_alg).map(|(_, v)| v);

			let Some(expected) = maybe_expected else {
				return Err(provider_error(CryptoProviderValidationError::MissingHashTestVector(
					*hash_alg,
				)));
			};

			if result.as_ref() != *expected {
				return Err(provider_error(CryptoProviderValidationError::HashProviderIncorrect(
					*hash_alg,
				)));
			}
		}

		Ok(required_hashes)
	}

	/// Validate that PRF (via HMAC) works for every supported hash algorithm.
	fn validate_prf(&self, validated_hashes: &[HashAlgorithm]) -> Result<(), Error> {
		let secret = b"test_secret";
		let label = "test label";
		let seed = b"test_seed";
		let output_len = 32;

		for &hash_alg in validated_hashes {
			let mut result = Buf::new();
			let mut scratch = Buf::new();
			super::prf_hkdf::prf_tls12(
				self.hmac_provider,
				secret,
				label,
				seed,
				&mut result,
				output_len,
				&mut scratch,
				hash_alg,
			)
			.map_err(|e| {
				provider_error(CryptoProviderValidationError::PrfFailed {
					hash: hash_alg,
					source: e,
				})
			})?;

			let maybe_expected = PRF_TEST_VECTORS.iter().find(|(h, _)| *h == hash_alg).map(|(_, v)| v);

			let Some(expected) = maybe_expected else {
				return Err(provider_error(CryptoProviderValidationError::MissingPrfTestVector(
					hash_alg,
				)));
			};

			if result.as_ref() != *expected {
				return Err(provider_error(CryptoProviderValidationError::PrfIncorrect(hash_alg)));
			}
		}

		Ok(())
	}

	/// Validate that signature verifier works for every supported cipher suite.
	fn validate_signature_verifier(&self, _validated_hashes: &[HashAlgorithm]) -> Result<(), Error> {
		// Test signature verification for each supported cipher suite
		for cs in self.supported_cipher_suites() {
			let hash_alg = cs.suite().hash_algorithm();
			let sig_alg = match cs.suite().signature_algorithm() {
				Some(alg) => alg,
				// PSK suites have no signature - skip validation
				None => continue,
			};

			let (cert_der, signature, test_data) = match (hash_alg, sig_alg) {
				(HashAlgorithm::SHA256, SignatureAlgorithm::ECDSA) => {
					(VALIDATION_P256_CERT, VALIDATION_P256_SHA256_SIG, VALIDATION_TEST_DATA)
				}
				(HashAlgorithm::SHA384, SignatureAlgorithm::ECDSA) => {
					(VALIDATION_P384_CERT, VALIDATION_P384_SHA384_SIG, VALIDATION_TEST_DATA)
				}
				_ => {
					return Err(provider_error(
						CryptoProviderValidationError::NoSignatureValidationVector {
							hash: hash_alg,
							signature: sig_alg,
						},
					));
				}
			};

			// Verify the signature
			self.signature_verification
				.verify_signature(cert_der, test_data, signature, hash_alg, sig_alg)
				.map_err(|e| {
					provider_error(CryptoProviderValidationError::SignatureVerificationFailed {
						hash: hash_alg,
						signature: sig_alg,
						source: e,
					})
				})?;
		}

		Ok(())
	}

	/// Validate key exchange round-trip for each supported group.
	fn validate_kx_exchange(&self) -> Result<(), Error> {
		for kx in self.supported_kx_groups() {
			let group = kx.name();

			let alice = kx.start_exchange(Buf::new()).map_err(|e| {
				provider_error(CryptoProviderValidationError::KeyExchangeStartFailed { group, source: e })
			})?;
			let bob = kx.start_exchange(Buf::new()).map_err(|e| {
				provider_error(CryptoProviderValidationError::KeyExchangeStartFailed { group, source: e })
			})?;

			let alice_pub = alice.pub_key().to_vec();
			let bob_pub = bob.pub_key().to_vec();

			let mut alice_secret = Buf::new();
			alice.complete(&bob_pub, &mut alice_secret).map_err(|e| {
				provider_error(CryptoProviderValidationError::KeyExchangeCompleteFailed { group, source: e })
			})?;

			let mut bob_secret = Buf::new();
			bob.complete(&alice_pub, &mut bob_secret).map_err(|e| {
				provider_error(CryptoProviderValidationError::KeyExchangeCompleteFailed { group, source: e })
			})?;

			if alice_secret.as_ref() != bob_secret.as_ref() {
				return Err(provider_error(
					CryptoProviderValidationError::KeyExchangeMismatchedSharedSecret(group),
				));
			}
		}
		Ok(())
	}

	/// Validate that HMAC provider supports required operations.
	///
	/// We require HMAC-SHA256 for DTLS cookie computation.
	fn validate_hmac_provider(&self) -> Result<(), Error> {
		// Test HMAC-SHA256 with known test vector (RFC 2104 test case)
		// HMAC-SHA256(key="key", data="The quick brown fox jumps over the lazy dog")
		let key = b"key";
		let data = b"The quick brown fox jumps over the lazy dog";

		let result = self
			.hmac_provider
			.hmac_sha256(key, data)
			.map_err(|e| provider_error(CryptoProviderValidationError::HmacFailed(e)))?;

		// Verify against known HMAC-SHA256 test vector
		if result.as_slice() != HMAC_SHA256_TEST_VECTOR {
			return Err(provider_error(CryptoProviderValidationError::HmacIncorrect));
		}

		Ok(())
	}
}

const HASH_TEST_VECTORS: &[(HashAlgorithm, &[u8])] = &[
	(
		HashAlgorithm::SHA256,
		&[
			0xe3, 0xb0, 0xc4, 0x42, 0x98, 0xfc, 0x1c, 0x14, 0x9a, 0xfb, 0xf4, 0xc8, 0x99, 0x6f, 0xb9, 0x24, 0x27, 0xae,
			0x41, 0xe4, 0x64, 0x9b, 0x93, 0x4c, 0xa4, 0x95, 0x99, 0x1b, 0x78, 0x52, 0xb8, 0x55,
		],
	),
	(
		HashAlgorithm::SHA384,
		&[
			0x38, 0xb0, 0x60, 0xa7, 0x51, 0xac, 0x96, 0x38, 0x4c, 0xd9, 0x32, 0x7e, 0xb1, 0xb1, 0xe3, 0x6a, 0x21, 0xfd,
			0xb7, 0x11, 0x14, 0xbe, 0x07, 0x43, 0x4c, 0x0c, 0xc7, 0xbf, 0x63, 0xf6, 0xe1, 0xda, 0x27, 0x4e, 0xde, 0xbf,
			0xe7, 0x6f, 0x65, 0xfb, 0xd5, 0x1a, 0xd2, 0xf1, 0x48, 0x98, 0xb9, 0x5b,
		],
	),
];

// Test vectors for TLS 1.2 PRF
// Generated using: PRF(secret="test_secret", label="test label", seed="test_seed", output_len=32)
const PRF_TEST_VECTORS: &[(HashAlgorithm, &[u8])] = &[
	(
		HashAlgorithm::SHA256,
		&[
			0xc7, 0x49, 0xce, 0xdf, 0xad, 0xaf, 0x3d, 0xf1, 0x18, 0x2c, 0xa2, 0x25, 0xab, 0xe9, 0x4e, 0x0c, 0x19, 0xc3,
			0x81, 0x49, 0x57, 0xbd, 0xdc, 0x28, 0x55, 0x78, 0x73, 0xdb, 0xb7, 0x9f, 0xce, 0x29,
		],
	),
	(
		HashAlgorithm::SHA384,
		&[
			0x74, 0x9a, 0xf3, 0x03, 0x23, 0x9e, 0x3f, 0x65, 0x4e, 0x9a, 0xd1, 0xb1, 0xd1, 0x22, 0x31, 0x02, 0x1a, 0xd2,
			0x17, 0x26, 0x04, 0x75, 0x21, 0xf4, 0x66, 0xad, 0xcd, 0x37, 0x2b, 0xe4, 0x7e, 0x8b,
		],
	),
];

// Test vector for HMAC-SHA256
// HMAC-SHA256(key="key", data="The quick brown fox jumps over the lazy dog")
// Computed using standard HMAC-SHA256 implementation
const HMAC_SHA256_TEST_VECTOR: &[u8] = &[
	0xf7, 0xbc, 0x83, 0xf4, 0x30, 0x53, 0x84, 0x24, 0xb1, 0x32, 0x98, 0xe6, 0xaa, 0x6f, 0xb1, 0x43, 0xef, 0x4d, 0x59,
	0xa1, 0x49, 0x46, 0x17, 0x59, 0x97, 0x47, 0x9d, 0xbc, 0x2d, 0x1a, 0x3c, 0xd8,
];

// Test certificates and signatures for signature verification validation
const VALIDATION_TEST_DATA: &[u8] = include_bytes!("test_data.bin");
const VALIDATION_P256_CERT: &[u8] = include_bytes!("p256_cert.der");
const VALIDATION_P256_SHA256_SIG: &[u8] = include_bytes!("p256_sha256_sig.der");
const VALIDATION_P384_CERT: &[u8] = include_bytes!("p384_cert.der");
const VALIDATION_P384_SHA384_SIG: &[u8] = include_bytes!("p384_sha384_sig.der");

#[cfg(test)]
mod tests_rind {
	use super::*;
	use crate::vendor::dimpl::crypto::rind;
	use crate::vendor::dimpl::dtls12::message::Dtls12CipherSuite;

	#[test]
	fn test_default_provider_validates() {
		let provider = rind::default_provider();
		assert!(provider.validate().is_ok());
	}

	#[test]
	fn test_default_provider_has_cipher_suites() {
		let provider = rind::default_provider();
		let count = provider.supported_cipher_suites().count();
		// ECDHE: AES-128, AES-256, ChaCha20
		// PSK: CCM-8
		assert_eq!(count, 4);
	}

	#[test]
	fn test_default_provider_has_kx_groups() {
		let provider = rind::default_provider();
		let count = provider.supported_kx_groups().count();
		assert_eq!(count, 3); // X25519, P-256, and P-384
	}

	#[test]
	fn test_default_provider_has_ecdh() {
		let provider = rind::default_provider();
		assert!(provider.has_ecdh());
	}

	#[test]
	fn test_supported_cipher_suites_for_signature_algorithm() {
		let provider = rind::default_provider();
		let ecdsa_suites: Vec<_> = provider
			.supported_cipher_suites_for_signature_algorithm(SignatureAlgorithm::ECDSA)
			.map(|cs| cs.suite())
			.collect();

		assert_eq!(ecdsa_suites.len(), 3);
		assert!(ecdsa_suites.contains(&Dtls12CipherSuite::ECDHE_ECDSA_AES128_GCM_SHA256));
		assert!(ecdsa_suites.contains(&Dtls12CipherSuite::ECDHE_ECDSA_AES256_GCM_SHA384));
		assert!(ecdsa_suites.contains(&Dtls12CipherSuite::ECDHE_ECDSA_CHACHA20_POLY1305_SHA256));
	}
}
