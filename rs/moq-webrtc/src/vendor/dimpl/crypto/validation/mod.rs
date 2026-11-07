//! Validation and filtering for crypto providers.
//!
//! This module defines the validation rules for crypto providers used with dimpl,
//! based on the documented support in lib.rs.

use arrayvec::ArrayVec;

use super::{Aad, CryptoProvider, Nonce, SupportedDtls12CipherSuite, SupportedKxGroup};
use crate::buffer::{Buf, TmpBuf};
use crate::types::{Dtls13CipherSuite, HashAlgorithm, NamedGroup, SignatureAlgorithm};
use crate::{ConfigError, CryptoProviderValidationError, Error};

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
    pub fn supported_cipher_suites(
        &self,
    ) -> impl Iterator<Item = &'static dyn SupportedDtls12CipherSuite> {
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
        self.supported_cipher_suites()
            .any(|cs| cs.suite().has_ecc())
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
        self.validate_dtls13_cipher_suites()?;
        self.validate_dtls13_aead()?;
        self.validate_dtls13_encrypt_sn()?;
        self.validate_kx_exchange()?;
        Ok(())
    }

    /// Validate that at least one cipher suite is supported.
    fn validate_cipher_suites(&self) -> Result<(), Error> {
        let cipher_count = self.supported_cipher_suites().count();
        if cipher_count == 0 {
            return Err(provider_error(
                CryptoProviderValidationError::NoCipherSuites,
            ));
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

            let maybe_expected = HASH_TEST_VECTORS
                .iter()
                .find(|(h, _)| *h == *hash_alg)
                .map(|(_, v)| v);

            let Some(expected) = maybe_expected else {
                return Err(provider_error(
                    CryptoProviderValidationError::MissingHashTestVector(*hash_alg),
                ));
            };

            if result.as_ref() != *expected {
                return Err(provider_error(
                    CryptoProviderValidationError::HashProviderIncorrect(*hash_alg),
                ));
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

            let maybe_expected = PRF_TEST_VECTORS
                .iter()
                .find(|(h, _)| *h == hash_alg)
                .map(|(_, v)| v);

            let Some(expected) = maybe_expected else {
                return Err(provider_error(
                    CryptoProviderValidationError::MissingPrfTestVector(hash_alg),
                ));
            };

            if result.as_ref() != *expected {
                return Err(provider_error(CryptoProviderValidationError::PrfIncorrect(
                    hash_alg,
                )));
            }
        }

        Ok(())
    }

    /// Validate that signature verifier works for every supported cipher suite.
    fn validate_signature_verifier(
        &self,
        _validated_hashes: &[HashAlgorithm],
    ) -> Result<(), Error> {
        // Test signature verification for each supported cipher suite
        for cs in self.supported_cipher_suites() {
            let hash_alg = cs.suite().hash_algorithm();
            let sig_alg = match cs.suite().signature_algorithm() {
                Some(alg) => alg,
                // PSK suites have no signature — skip validation
                None => continue,
            };

            let (cert_der, signature, test_data) = match (hash_alg, sig_alg) {
                (HashAlgorithm::SHA256, SignatureAlgorithm::ECDSA) => (
                    VALIDATION_P256_CERT,
                    VALIDATION_P256_SHA256_SIG,
                    VALIDATION_TEST_DATA,
                ),
                (HashAlgorithm::SHA384, SignatureAlgorithm::ECDSA) => (
                    VALIDATION_P384_CERT,
                    VALIDATION_P384_SHA384_SIG,
                    VALIDATION_TEST_DATA,
                ),
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

    /// Validate that DTLS 1.3 cipher suites and HKDF provider are configured.
    fn validate_dtls13_cipher_suites(&self) -> Result<(), Error> {
        if self.dtls13_cipher_suites.is_empty() {
            return Err(provider_error(
                CryptoProviderValidationError::NoDtls13CipherSuites,
            ));
        }

        // Verify HKDF (via HMAC) works for each DTLS 1.3 cipher suite's hash algorithm
        for cs in self.dtls13_cipher_suites {
            let hash = cs.suite().hash_algorithm();
            let hash_len = hash.output_len();
            let zeros = [0u8; 48];
            let zeros = &zeros[..hash_len];
            let mut out = Buf::new();
            super::prf_hkdf::hkdf_extract(self.hmac_provider, hash, zeros, zeros, &mut out)
                .map_err(|e| {
                    provider_error(CryptoProviderValidationError::HkdfFailed {
                        suite: cs.suite(),
                        source: e,
                    })
                })?;
            if out.is_empty() {
                return Err(provider_error(
                    CryptoProviderValidationError::HkdfEmptyOutput(cs.suite()),
                ));
            }
        }

        Ok(())
    }

    /// Validate AEAD encrypt/decrypt for each DTLS 1.3 cipher suite.
    fn validate_dtls13_aead(&self) -> Result<(), Error> {
        for cs in self.dtls13_cipher_suites {
            let suite = cs.suite();
            let tv = AEAD_TEST_VECTORS
                .iter()
                .find(|tv| tv.suite == suite)
                .ok_or_else(|| {
                    provider_error(CryptoProviderValidationError::NoAeadTestVector(suite))
                })?;

            let nonce = Nonce(tv.nonce);
            let mut aad_vec = ArrayVec::new();
            // unwrap: AAD is at most 12 bytes, well within capacity 13
            aad_vec.try_extend_from_slice(tv.aad).unwrap();
            let aad = Aad(aad_vec);

            // Encrypt
            let mut cipher = cs.create_cipher(tv.key).map_err(|e| {
                provider_error(CryptoProviderValidationError::AeadCreateFailed { suite, source: e })
            })?;
            let mut buf = Buf::new();
            buf.extend_from_slice(tv.plaintext);
            cipher.encrypt(&mut buf, aad.clone(), nonce).map_err(|e| {
                provider_error(CryptoProviderValidationError::AeadEncryptFailed {
                    suite,
                    source: e,
                })
            })?;
            if buf.as_ref() != tv.ciphertext_tag {
                return Err(provider_error(
                    CryptoProviderValidationError::AeadEncryptWrongOutput(suite),
                ));
            }

            // Decrypt with a fresh cipher instance
            let mut cipher = cs.create_cipher(tv.key).map_err(|e| {
                provider_error(CryptoProviderValidationError::AeadCreateFailed { suite, source: e })
            })?;
            let mut ct = Vec::from(tv.ciphertext_tag);
            let mut tmp = TmpBuf::new(&mut ct);
            cipher.decrypt(&mut tmp, aad, nonce).map_err(|e| {
                provider_error(CryptoProviderValidationError::AeadDecryptFailed {
                    suite,
                    source: e,
                })
            })?;
            if tmp.as_ref() != tv.plaintext {
                return Err(provider_error(
                    CryptoProviderValidationError::AeadDecryptWrongOutput(suite),
                ));
            }
        }
        Ok(())
    }

    /// Validate record number encryption for each DTLS 1.3 cipher suite.
    fn validate_dtls13_encrypt_sn(&self) -> Result<(), Error> {
        for cs in self.dtls13_cipher_suites {
            let suite = cs.suite();
            let tv = SN_TEST_VECTORS
                .iter()
                .find(|tv| tv.suite == suite)
                .ok_or_else(|| {
                    provider_error(
                        CryptoProviderValidationError::NoRecordNumberEncryptionTestVector(suite),
                    )
                })?;

            let mask = cs.encrypt_sn(tv.sn_key, &tv.sample);
            if mask[..tv.check_len] != tv.expected_mask[..tv.check_len] {
                return Err(provider_error(
                    CryptoProviderValidationError::RecordNumberEncryptionWrongMask(suite),
                ));
            }
        }
        Ok(())
    }

    /// Validate key exchange round-trip for each supported group.
    fn validate_kx_exchange(&self) -> Result<(), Error> {
        for kx in self.supported_kx_groups() {
            let group = kx.name();

            let alice = kx.start_exchange(Buf::new()).map_err(|e| {
                provider_error(CryptoProviderValidationError::KeyExchangeStartFailed {
                    group,
                    source: e,
                })
            })?;
            let bob = kx.start_exchange(Buf::new()).map_err(|e| {
                provider_error(CryptoProviderValidationError::KeyExchangeStartFailed {
                    group,
                    source: e,
                })
            })?;

            let alice_pub = alice.pub_key().to_vec();
            let bob_pub = bob.pub_key().to_vec();

            let mut alice_secret = Buf::new();
            alice.complete(&bob_pub, &mut alice_secret).map_err(|e| {
                provider_error(CryptoProviderValidationError::KeyExchangeCompleteFailed {
                    group,
                    source: e,
                })
            })?;

            let mut bob_secret = Buf::new();
            bob.complete(&alice_pub, &mut bob_secret).map_err(|e| {
                provider_error(CryptoProviderValidationError::KeyExchangeCompleteFailed {
                    group,
                    source: e,
                })
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
            0xe3, 0xb0, 0xc4, 0x42, 0x98, 0xfc, 0x1c, 0x14, 0x9a, 0xfb, 0xf4, 0xc8, 0x99, 0x6f,
            0xb9, 0x24, 0x27, 0xae, 0x41, 0xe4, 0x64, 0x9b, 0x93, 0x4c, 0xa4, 0x95, 0x99, 0x1b,
            0x78, 0x52, 0xb8, 0x55,
        ],
    ),
    (
        HashAlgorithm::SHA384,
        &[
            0x38, 0xb0, 0x60, 0xa7, 0x51, 0xac, 0x96, 0x38, 0x4c, 0xd9, 0x32, 0x7e, 0xb1, 0xb1,
            0xe3, 0x6a, 0x21, 0xfd, 0xb7, 0x11, 0x14, 0xbe, 0x07, 0x43, 0x4c, 0x0c, 0xc7, 0xbf,
            0x63, 0xf6, 0xe1, 0xda, 0x27, 0x4e, 0xde, 0xbf, 0xe7, 0x6f, 0x65, 0xfb, 0xd5, 0x1a,
            0xd2, 0xf1, 0x48, 0x98, 0xb9, 0x5b,
        ],
    ),
];

// Test vectors for TLS 1.2 PRF
// Generated using: PRF(secret="test_secret", label="test label", seed="test_seed", output_len=32)
const PRF_TEST_VECTORS: &[(HashAlgorithm, &[u8])] = &[
    (
        HashAlgorithm::SHA256,
        &[
            0xc7, 0x49, 0xce, 0xdf, 0xad, 0xaf, 0x3d, 0xf1, 0x18, 0x2c, 0xa2, 0x25, 0xab, 0xe9,
            0x4e, 0x0c, 0x19, 0xc3, 0x81, 0x49, 0x57, 0xbd, 0xdc, 0x28, 0x55, 0x78, 0x73, 0xdb,
            0xb7, 0x9f, 0xce, 0x29,
        ],
    ),
    (
        HashAlgorithm::SHA384,
        &[
            0x74, 0x9a, 0xf3, 0x03, 0x23, 0x9e, 0x3f, 0x65, 0x4e, 0x9a, 0xd1, 0xb1, 0xd1, 0x22,
            0x31, 0x02, 0x1a, 0xd2, 0x17, 0x26, 0x04, 0x75, 0x21, 0xf4, 0x66, 0xad, 0xcd, 0x37,
            0x2b, 0xe4, 0x7e, 0x8b,
        ],
    ),
];

// Test vector for HMAC-SHA256
// HMAC-SHA256(key="key", data="The quick brown fox jumps over the lazy dog")
// Computed using standard HMAC-SHA256 implementation
const HMAC_SHA256_TEST_VECTOR: &[u8] = &[
    0xf7, 0xbc, 0x83, 0xf4, 0x30, 0x53, 0x84, 0x24, 0xb1, 0x32, 0x98, 0xe6, 0xaa, 0x6f, 0xb1, 0x43,
    0xef, 0x4d, 0x59, 0xa1, 0x49, 0x46, 0x17, 0x59, 0x97, 0x47, 0x9d, 0xbc, 0x2d, 0x1a, 0x3c, 0xd8,
];

// Test certificates and signatures for signature verification validation
const VALIDATION_TEST_DATA: &[u8] = include_bytes!("test_data.bin");
const VALIDATION_P256_CERT: &[u8] = include_bytes!("p256_cert.der");
const VALIDATION_P256_SHA256_SIG: &[u8] = include_bytes!("p256_sha256_sig.der");
const VALIDATION_P384_CERT: &[u8] = include_bytes!("p384_cert.der");
const VALIDATION_P384_SHA384_SIG: &[u8] = include_bytes!("p384_sha384_sig.der");

// AEAD test vectors for DTLS 1.3 cipher suites
struct AeadTestVector {
    suite: Dtls13CipherSuite,
    key: &'static [u8],
    nonce: [u8; 12],
    plaintext: &'static [u8],
    aad: &'static [u8],
    ciphertext_tag: &'static [u8], // ciphertext || tag
}

// NIST SP 800-38D Test Case 3 plaintext (shared by AES-128 and AES-256)
const GCM_PLAINTEXT: &[u8] = &[
    0xd9, 0x31, 0x32, 0x25, 0xf8, 0x84, 0x06, 0xe5, 0xa5, 0x59, 0x09, 0xc5, 0xaf, 0xf5, 0x26, 0x9a,
    0x86, 0xa7, 0xa9, 0x53, 0x15, 0x34, 0xf7, 0xda, 0x2e, 0x4c, 0x30, 0x3d, 0x8a, 0x31, 0x8a, 0x72,
    0x1c, 0x3c, 0x0c, 0x95, 0x95, 0x68, 0x09, 0x53, 0x2f, 0xcf, 0x0e, 0x24, 0x49, 0xa6, 0xb5, 0x25,
    0xb1, 0x6a, 0xed, 0xf5, 0xaa, 0x0d, 0xe6, 0x57, 0xba, 0x63, 0x7b, 0x39, 0x1a, 0xaf, 0xd2, 0x55,
];

const AEAD_TEST_VECTORS: &[AeadTestVector] = &[
    // NIST SP 800-38D Test Case 3: AES-128-GCM, empty AAD
    AeadTestVector {
        suite: Dtls13CipherSuite::AES_128_GCM_SHA256,
        key: &[
            0xfe, 0xff, 0xe9, 0x92, 0x86, 0x65, 0x73, 0x1c, 0x6d, 0x6a, 0x8f, 0x94, 0x67, 0x30,
            0x83, 0x08,
        ],
        nonce: [
            0xca, 0xfe, 0xba, 0xbe, 0xfa, 0xce, 0xdb, 0xad, 0xde, 0xca, 0xf8, 0x88,
        ],
        plaintext: GCM_PLAINTEXT,
        aad: &[],
        ciphertext_tag: &[
            // Ciphertext
            0x42, 0x83, 0x1e, 0xc2, 0x21, 0x77, 0x74, 0x24, 0x4b, 0x72, 0x21, 0xb7, 0x84, 0xd0,
            0xd4, 0x9c, 0xe3, 0xaa, 0x21, 0x2f, 0x2c, 0x02, 0xa4, 0xe0, 0x35, 0xc1, 0x7e, 0x23,
            0x29, 0xac, 0xa1, 0x2e, 0x21, 0xd5, 0x14, 0xb2, 0x54, 0x66, 0x93, 0x1c, 0x7d, 0x8f,
            0x6a, 0x5a, 0xac, 0x84, 0xaa, 0x05, 0x1b, 0xa3, 0x0b, 0x39, 0x6a, 0x0a, 0xac, 0x97,
            0x3d, 0x58, 0xe0, 0x91, 0x47, 0x3f, 0x59, 0x85, // Tag
            0x4d, 0x5c, 0x2a, 0xf3, 0x27, 0xcd, 0x64, 0xa6, 0x2c, 0xf3, 0x5a, 0xbd, 0x2b, 0xa6,
            0xfa, 0xb4,
        ],
    },
    // NIST SP 800-38D Test Case 15: AES-256-GCM, empty AAD
    AeadTestVector {
        suite: Dtls13CipherSuite::AES_256_GCM_SHA384,
        key: &[
            0xfe, 0xff, 0xe9, 0x92, 0x86, 0x65, 0x73, 0x1c, 0x6d, 0x6a, 0x8f, 0x94, 0x67, 0x30,
            0x83, 0x08, 0xfe, 0xff, 0xe9, 0x92, 0x86, 0x65, 0x73, 0x1c, 0x6d, 0x6a, 0x8f, 0x94,
            0x67, 0x30, 0x83, 0x08,
        ],
        nonce: [
            0xca, 0xfe, 0xba, 0xbe, 0xfa, 0xce, 0xdb, 0xad, 0xde, 0xca, 0xf8, 0x88,
        ],
        plaintext: GCM_PLAINTEXT,
        aad: &[],
        ciphertext_tag: &[
            // Ciphertext
            0x52, 0x2d, 0xc1, 0xf0, 0x99, 0x56, 0x7d, 0x07, 0xf4, 0x7f, 0x37, 0xa3, 0x2a, 0x84,
            0x42, 0x7d, 0x64, 0x3a, 0x8c, 0xdc, 0xbf, 0xe5, 0xc0, 0xc9, 0x75, 0x98, 0xa2, 0xbd,
            0x25, 0x55, 0xd1, 0xaa, 0x8c, 0xb0, 0x8e, 0x48, 0x59, 0x0d, 0xbb, 0x3d, 0xa7, 0xb0,
            0x8b, 0x10, 0x56, 0x82, 0x88, 0x38, 0xc5, 0xf6, 0x1e, 0x63, 0x93, 0xba, 0x7a, 0x0a,
            0xbc, 0xc9, 0xf6, 0x62, 0x89, 0x80, 0x15, 0xad, // Tag
            0xb0, 0x94, 0xda, 0xc5, 0xd9, 0x34, 0x71, 0xbd, 0xec, 0x1a, 0x50, 0x22, 0x70, 0xe3,
            0xcc, 0x6c,
        ],
    },
    // RFC 7539 §2.8.2: ChaCha20-Poly1305
    AeadTestVector {
        suite: Dtls13CipherSuite::CHACHA20_POLY1305_SHA256,
        key: &[
            0x80, 0x81, 0x82, 0x83, 0x84, 0x85, 0x86, 0x87, 0x88, 0x89, 0x8a, 0x8b, 0x8c, 0x8d,
            0x8e, 0x8f, 0x90, 0x91, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99, 0x9a, 0x9b,
            0x9c, 0x9d, 0x9e, 0x9f,
        ],
        nonce: [
            0x07, 0x00, 0x00, 0x00, 0x40, 0x41, 0x42, 0x43, 0x44, 0x45, 0x46, 0x47,
        ],
        plaintext: &[
            // "Ladies and Gentlemen of the class of '99: If I could offer you
            //  only one tip for the future, sunscreen would be it."
            0x4c, 0x61, 0x64, 0x69, 0x65, 0x73, 0x20, 0x61, 0x6e, 0x64, 0x20, 0x47, 0x65, 0x6e,
            0x74, 0x6c, 0x65, 0x6d, 0x65, 0x6e, 0x20, 0x6f, 0x66, 0x20, 0x74, 0x68, 0x65, 0x20,
            0x63, 0x6c, 0x61, 0x73, 0x73, 0x20, 0x6f, 0x66, 0x20, 0x27, 0x39, 0x39, 0x3a, 0x20,
            0x49, 0x66, 0x20, 0x49, 0x20, 0x63, 0x6f, 0x75, 0x6c, 0x64, 0x20, 0x6f, 0x66, 0x66,
            0x65, 0x72, 0x20, 0x79, 0x6f, 0x75, 0x20, 0x6f, 0x6e, 0x6c, 0x79, 0x20, 0x6f, 0x6e,
            0x65, 0x20, 0x74, 0x69, 0x70, 0x20, 0x66, 0x6f, 0x72, 0x20, 0x74, 0x68, 0x65, 0x20,
            0x66, 0x75, 0x74, 0x75, 0x72, 0x65, 0x2c, 0x20, 0x73, 0x75, 0x6e, 0x73, 0x63, 0x72,
            0x65, 0x65, 0x6e, 0x20, 0x77, 0x6f, 0x75, 0x6c, 0x64, 0x20, 0x62, 0x65, 0x20, 0x69,
            0x74, 0x2e,
        ],
        aad: &[
            0x50, 0x51, 0x52, 0x53, 0xc0, 0xc1, 0xc2, 0xc3, 0xc4, 0xc5, 0xc6, 0xc7,
        ],
        ciphertext_tag: &[
            // Ciphertext
            0xd3, 0x1a, 0x8d, 0x34, 0x64, 0x8e, 0x60, 0xdb, 0x7b, 0x86, 0xaf, 0xbc, 0x53, 0xef,
            0x7e, 0xc2, 0xa4, 0xad, 0xed, 0x51, 0x29, 0x6e, 0x08, 0xfe, 0xa9, 0xe2, 0xb5, 0xa7,
            0x36, 0xee, 0x62, 0xd6, 0x3d, 0xbe, 0xa4, 0x5e, 0x8c, 0xa9, 0x67, 0x12, 0x82, 0xfa,
            0xfb, 0x69, 0xda, 0x92, 0x72, 0x8b, 0x1a, 0x71, 0xde, 0x0a, 0x9e, 0x06, 0x0b, 0x29,
            0x05, 0xd6, 0xa5, 0xb6, 0x7e, 0xcd, 0x3b, 0x36, 0x92, 0xdd, 0xbd, 0x7f, 0x2d, 0x77,
            0x8b, 0x8c, 0x98, 0x03, 0xae, 0xe3, 0x28, 0x09, 0x1b, 0x58, 0xfa, 0xb3, 0x24, 0xe4,
            0xfa, 0xd6, 0x75, 0x94, 0x55, 0x85, 0x80, 0x8b, 0x48, 0x31, 0xd7, 0xbc, 0x3f, 0xf4,
            0xde, 0xf0, 0x8e, 0x4b, 0x7a, 0x9d, 0xe5, 0x76, 0xd2, 0x65, 0x86, 0xce, 0xc6, 0x4b,
            0x61, 0x16, // Tag
            0x1a, 0xe1, 0x0b, 0x59, 0x4f, 0x09, 0xe2, 0x6a, 0x7e, 0x90, 0x2e, 0xcb, 0xd0, 0x60,
            0x06, 0x91,
        ],
    },
];

// Record number encryption (encrypt_sn) test vectors
struct SnTestVector {
    suite: Dtls13CipherSuite,
    sn_key: &'static [u8],
    sample: [u8; 16],
    expected_mask: [u8; 16],
    check_len: usize, // number of leading bytes to verify
}

const SN_TEST_VECTORS: &[SnTestVector] = &[
    // NIST FIPS 197 Appendix B: AES-128-ECB
    SnTestVector {
        suite: Dtls13CipherSuite::AES_128_GCM_SHA256,
        sn_key: &[
            0x2b, 0x7e, 0x15, 0x16, 0x28, 0xae, 0xd2, 0xa6, 0xab, 0xf7, 0x15, 0x88, 0x09, 0xcf,
            0x4f, 0x3c,
        ],
        sample: [
            0x32, 0x43, 0xf6, 0xa8, 0x88, 0x5a, 0x30, 0x8d, 0x31, 0x31, 0x98, 0xa2, 0xe0, 0x37,
            0x07, 0x34,
        ],
        expected_mask: [
            0x39, 0x25, 0x84, 0x1d, 0x02, 0xdc, 0x09, 0xfb, 0xdc, 0x11, 0x85, 0x97, 0x19, 0x6a,
            0x0b, 0x32,
        ],
        check_len: 16,
    },
    // NIST FIPS 197 Appendix C.3: AES-256-ECB
    SnTestVector {
        suite: Dtls13CipherSuite::AES_256_GCM_SHA384,
        sn_key: &[
            0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d,
            0x0e, 0x0f, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b,
            0x1c, 0x1d, 0x1e, 0x1f,
        ],
        sample: [
            0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd,
            0xee, 0xff,
        ],
        expected_mask: [
            0x8e, 0xa2, 0xb7, 0xca, 0x51, 0x67, 0x45, 0xbf, 0xea, 0xfc, 0x49, 0x90, 0x4b, 0x49,
            0x60, 0x89,
        ],
        check_len: 16,
    },
    // RFC 9001 §A.5: ChaCha20 header protection
    // Only first 5 bytes are checked (aws-lc-rs zeros bytes 5-15, rust_crypto fills all 16)
    SnTestVector {
        suite: Dtls13CipherSuite::CHACHA20_POLY1305_SHA256,
        sn_key: &[
            0x25, 0xa2, 0x82, 0xb9, 0xe8, 0x2f, 0x06, 0xf2, 0x1f, 0x48, 0x89, 0x17, 0xa4, 0xfc,
            0x8f, 0x1b, 0x73, 0x57, 0x36, 0x85, 0x60, 0x85, 0x97, 0xd0, 0xef, 0xcb, 0x07, 0x6b,
            0x0a, 0xb7, 0xa7, 0xa4,
        ],
        sample: [
            0x5e, 0x5c, 0xd5, 0x5c, 0x41, 0xf6, 0x90, 0x80, 0x57, 0x5d, 0x79, 0x99, 0xc2, 0x5a,
            0x5b, 0xfb,
        ],
        expected_mask: [
            0xae, 0xfe, 0xfe, 0x7d, 0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00,
        ],
        check_len: 5,
    },
];

#[cfg(test)]
#[cfg(feature = "aws-lc-rs")]
mod tests_aws_lc_rs {
    use super::*;
    use crate::crypto::aws_lc_rs;
    use crate::dtls12::message::Dtls12CipherSuite;

    #[test]
    fn test_default_provider_validates() {
        let provider = aws_lc_rs::default_provider();
        assert!(provider.validate().is_ok());
    }

    #[test]
    fn test_default_provider_has_cipher_suites() {
        let provider = aws_lc_rs::default_provider();
        let count = provider.supported_cipher_suites().count();
        // ECDHE: AES-128, AES-256, ChaCha20
        // PSK: CCM-8
        assert_eq!(count, 4);
    }

    #[test]
    fn test_default_provider_has_kx_groups() {
        let provider = aws_lc_rs::default_provider();
        let count = provider.supported_kx_groups().count();
        assert_eq!(count, 3); // X25519, P-256, and P-384
    }

    #[test]
    fn test_default_provider_has_ecdh() {
        let provider = aws_lc_rs::default_provider();
        assert!(provider.has_ecdh());
    }

    #[test]
    fn test_supported_cipher_suites_for_signature_algorithm() {
        let provider = aws_lc_rs::default_provider();
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

#[cfg(test)]
#[cfg(feature = "rust-crypto")]
mod tests_rust_crypto {
    use super::*;
    use crate::crypto::rust_crypto;
    use crate::dtls12::message::Dtls12CipherSuite;

    #[test]
    fn test_default_provider_validates() {
        let provider = rust_crypto::default_provider();
        assert!(provider.validate().is_ok());
    }

    #[test]
    fn test_default_provider_has_cipher_suites() {
        let provider = rust_crypto::default_provider();
        let count = provider.supported_cipher_suites().count();
        // ECDHE: AES-128, AES-256, ChaCha20
        // PSK: CCM-8
        assert_eq!(count, 4);
    }

    #[test]
    fn test_default_provider_has_kx_groups() {
        let provider = rust_crypto::default_provider();
        let count = provider.supported_kx_groups().count();
        assert_eq!(count, 3); // X25519, P-256, and P-384
    }

    #[test]
    fn test_default_provider_has_ecdh() {
        let provider = rust_crypto::default_provider();
        assert!(provider.has_ecdh());
    }

    #[test]
    fn test_supported_cipher_suites_for_signature_algorithm() {
        let provider = rust_crypto::default_provider();
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
