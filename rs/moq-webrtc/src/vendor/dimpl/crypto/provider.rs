//! Cryptographic provider traits for pluggable crypto backends.
//!
//! This module defines the trait-based interface for cryptographic operations
//! in dimpl, allowing users to provide custom crypto implementations.
//!
//! # Overview
//!
//! The crypto provider system is inspired by rustls's design and uses a component-based
//! approach where the [`CryptoProvider`] struct holds static references to various
//! trait objects, each representing a specific cryptographic capability.
//!
//! # Architecture
//!
//! The provider system is organized into these main components:
//!
//! - **Cipher Suites** ([`SupportedDtls12CipherSuite`]): Factory for AEAD ciphers
//! - **Key Exchange Groups** ([`SupportedKxGroup`]): Factory for ECDHE key exchanges
//! - **Signature Verification** ([`SignatureVerifier`]): Verify signatures in certificates
//! - **Key Provider** ([`KeyProvider`]): Parse and load private keys
//! - **Secure Random** ([`SecureRandom`]): Cryptographically secure RNG
//! - **Hash Provider** ([`HashProvider`]): Factory for hash contexts
//! - **HMAC Provider** ([`HmacProvider`]): Compute HMAC signatures (also drives PRF and HKDF)
//!
//! # Using a Custom Provider
//!
//! To use a custom crypto provider, create one and pass it to the [`Config`](crate::Config):
//!
//! ```
//! # #[cfg(all(feature = "aws-lc-rs", feature = "rcgen"))]
//! # fn main() {
//! use std::sync::Arc;
//! use std::time::Instant;
//! use dimpl::{Config, Dtls, certificate};
//! use dimpl::crypto::aws_lc_rs;
//!
//! let cert = certificate::generate_self_signed_certificate().unwrap();
//! // Use the default aws-lc-rs provider (implicit)
//! let config = Arc::new(Config::default());
//!
//! // Or explicitly set the provider
//! let config = Arc::new(
//!     Config::builder()
//!         .with_crypto_provider(aws_lc_rs::default_provider())
//!         .build()
//!         .unwrap()
//! );
//!
//! // Or use your own custom provider
//! // let config = Arc::new(
//! //     Config::builder()
//! //         .with_crypto_provider(my_custom_provider())
//! //         .build()
//! //         .unwrap()
//! // );
//!
//! let dtls = Dtls::new_12(config, cert, Instant::now());
//! # }
//! # #[cfg(not(all(feature = "aws-lc-rs", feature = "rcgen")))]
//! # fn main() {}
//! ```
//!
//! # Implementing a Custom Provider
//!
//! To implement a custom provider, you need to:
//!
//! 1. Implement the required traits for your crypto backend
//! 2. Create static instances of your implementations
//! 3. Build a [`CryptoProvider`] struct with references to those statics
//!
//! ## Example: Custom Cipher Suite
//!
//! ```
//! use dimpl::CryptoError;
//! use dimpl::crypto::{SupportedDtls12CipherSuite, Cipher, Dtls12CipherSuite, HashAlgorithm};
//! use dimpl::crypto::{Buf, TmpBuf};
//! use dimpl::crypto::{Aad, Nonce};
//!
//! #[derive(Debug)]
//! struct MyCipher;
//!
//! impl MyCipher {
//!     fn new(_key: &[u8]) -> Result<Self, CryptoError> {
//!         Ok(Self)
//!     }
//! }
//!
//! impl Cipher for MyCipher {
//!     fn encrypt(&mut self, _: &mut Buf, _: Aad, _: Nonce) -> Result<(), CryptoError> {
//!         Ok(())
//!     }
//!     fn decrypt(&mut self, _: &mut TmpBuf, _: Aad, _: Nonce) -> Result<(), CryptoError> {
//!         Ok(())
//!     }
//! }
//!
//! #[derive(Debug)]
//! struct MyDtls12CipherSuite;
//!
//! impl SupportedDtls12CipherSuite for MyDtls12CipherSuite {
//!     fn suite(&self) -> Dtls12CipherSuite {
//!         Dtls12CipherSuite::ECDHE_ECDSA_AES128_GCM_SHA256
//!     }
//!
//!     fn hash_algorithm(&self) -> HashAlgorithm {
//!         HashAlgorithm::SHA256
//!     }
//!
//!     fn key_lengths(&self) -> (usize, usize, usize) {
//!         (0, 16, 4) // (mac_key_len, enc_key_len, fixed_iv_len)
//!     }
//!
//!     fn explicit_nonce_len(&self) -> usize {
//!         8 // AES-GCM: 8-byte explicit nonce per record
//!     }
//!
//!     fn tag_len(&self) -> usize {
//!         16 // 128-bit authentication tag
//!     }
//!
//!     fn create_cipher(&self, key: &[u8]) -> Result<Box<dyn Cipher>, CryptoError> {
//!         // Create your cipher implementation here
//!         Ok(Box::new(MyCipher::new(key)?))
//!     }
//! }
//!
//! static MY_CIPHER_SUITE: MyDtls12CipherSuite = MyDtls12CipherSuite;
//! static ALL_CIPHER_SUITES: &[&dyn SupportedDtls12CipherSuite] = &[&MY_CIPHER_SUITE];
//! ```
//!
//! # Requirements
//!
//! For DTLS 1.2, implementations must support:
//!
//! - **Cipher suites**: ECDHE_ECDSA with AES-128-GCM, AES-256-GCM, or CHACHA20_POLY1305
//! - **Key exchange**: ECDHE with X25519, P-256, or P-384 curves
//! - **Signatures**: ECDSA with P-256/SHA-256 or P-384/SHA-384
//! - **Hash**: SHA-256 and SHA-384
//! - **HMAC**: HMAC-SHA256 and HMAC-SHA384 (used for PRF, HKDF, and cookies)
//!
//! # Thread Safety
//!
//! All provider traits require `Send + Sync + UnwindSafe + RefUnwindSafe` to ensure
//! safe usage across threads and panic boundaries.

use std::fmt::Debug;
use std::panic::{RefUnwindSafe, UnwindSafe};
use std::sync::OnceLock;

use crate::buffer::{Buf, TmpBuf};
use crate::crypto::{Aad, Nonce};
use crate::dtls12::message::Dtls12CipherSuite;
use crate::types::{Dtls13CipherSuite, HashAlgorithm, NamedGroup, SignatureAlgorithm};
use crate::{CertificateError, CryptoError};

/// OID for the P-256 elliptic curve (secp256r1 / prime256v1).
#[cfg(feature = "_crypto-common")]
pub const OID_P256: spki::ObjectIdentifier =
    spki::ObjectIdentifier::new_unwrap("1.2.840.10045.3.1.7");

/// OID for the P-384 elliptic curve (secp384r1).
#[cfg(feature = "_crypto-common")]
pub const OID_P384: spki::ObjectIdentifier = spki::ObjectIdentifier::new_unwrap("1.3.132.0.34");

// ============================================================================
// Marker Trait
// ============================================================================

/// Marker trait for types that are safe to use in crypto provider components.
///
/// This trait combines the common bounds required for crypto provider trait objects:
/// - [`Send`] + [`Sync`]: Thread-safe
/// - [`Debug`]: Support debugging
/// - [`UnwindSafe`] + [`RefUnwindSafe`]: Panic-safe
///
/// This trait is automatically implemented for all types that satisfy these bounds.
pub trait CryptoSafe: Send + Sync + Debug + UnwindSafe + RefUnwindSafe {}

/// Blanket implementation: any type satisfying the bounds implements [`CryptoSafe`].
impl<T: Send + Sync + Debug + UnwindSafe + RefUnwindSafe> CryptoSafe for T {}

// ============================================================================
// Instance Traits (Level 2 - created by factories)
// ============================================================================

/// AEAD cipher for in-place encryption/decryption.
pub trait Cipher: CryptoSafe {
    /// Encrypt plaintext in-place, appending authentication tag.
    fn encrypt(&mut self, plaintext: &mut Buf, aad: Aad, nonce: Nonce) -> Result<(), CryptoError>;

    /// Decrypt ciphertext in-place, verifying and removing authentication tag.
    fn decrypt(
        &mut self,
        ciphertext: &mut TmpBuf,
        aad: Aad,
        nonce: Nonce,
    ) -> Result<(), CryptoError>;
}

/// Stateful hash context for incremental hashing.
pub trait HashContext: CryptoSafe {
    /// Update the hash with new data.
    fn update(&mut self, data: &[u8]);

    /// Clone the context and finalize it, writing the hash to `out`.
    /// The original context can continue to be updated.
    fn clone_and_finalize(&self, out: &mut Buf);
}

/// Signing key for generating digital signatures.
pub trait SigningKey: CryptoSafe {
    /// Sign data using the specified hash algorithm and return the signature.
    fn sign(
        &mut self,
        data: &[u8],
        hash_alg: HashAlgorithm,
        out: &mut Buf,
    ) -> Result<(), CryptoError>;

    /// Signature algorithm used by this key.
    fn algorithm(&self) -> SignatureAlgorithm;

    /// Default hash algorithm for this key.
    fn hash_algorithm(&self) -> HashAlgorithm;

    /// Hash algorithms this key can sign with.
    ///
    /// Used during negotiation to intersect with the peer's offered
    /// algorithms. Backends that lock the hash at key-load time (e.g.
    /// aws-lc-rs) return only the locked hash; backends that support
    /// arbitrary prehash signing (e.g. RustCrypto) may return several.
    fn supported_hash_algorithms(&self) -> &[HashAlgorithm];
}

/// Active key exchange instance (ephemeral keypair for one handshake).
pub trait ActiveKeyExchange: CryptoSafe {
    /// Get the public key for this exchange.
    fn pub_key(&self) -> &[u8];

    /// Complete exchange with peer's public key, returning shared secret.
    fn complete(self: Box<Self>, peer_pub: &[u8], out: &mut Buf) -> Result<(), CryptoError>;

    /// Get the named group for this exchange.
    fn group(&self) -> NamedGroup;
}

// ============================================================================
// Factory Traits (Level 1 - used by CryptoProvider)
// ============================================================================

/// Cipher suite support (factory for Cipher instances).
pub trait SupportedDtls12CipherSuite: CryptoSafe {
    /// The cipher suite this supports.
    fn suite(&self) -> Dtls12CipherSuite;

    /// Hash algorithm used by this suite.
    fn hash_algorithm(&self) -> HashAlgorithm;

    /// Key material lengths: (mac_key_len, enc_key_len, fixed_iv_len).
    fn key_lengths(&self) -> (usize, usize, usize);

    /// Length in bytes of the per-record explicit nonce (carried in the record body).
    ///
    /// AES-GCM suites carry an 8-byte explicit nonce; ChaCha20-Poly1305 carries none.
    fn explicit_nonce_len(&self) -> usize;

    /// AEAD authentication tag length in bytes.
    fn tag_len(&self) -> usize;

    /// Minimum length, in bytes, of a protected record's encrypted fragment.
    ///
    /// For AEAD suites this equals explicit nonce + authentication tag; a CBC
    /// suite would override this to `IV + MAC + 1` (one padding byte). Records
    /// shorter than this cannot be valid regardless of cipher mode and are
    /// rejected at the record boundary.
    fn min_protected_fragment_len(&self) -> usize {
        self.explicit_nonce_len() + self.tag_len()
    }

    /// Create a cipher instance with the given key.
    fn create_cipher(&self, key: &[u8]) -> Result<Box<dyn Cipher>, CryptoError>;
}

/// Key exchange group support (factory for ActiveKeyExchange).
pub trait SupportedKxGroup: CryptoSafe {
    /// Named group for this key exchange group.
    fn name(&self) -> NamedGroup;

    /// Start a new key exchange, generating ephemeral keypair.
    /// The provided `buf` will be used to store the public key.
    fn start_exchange(&self, buf: Buf) -> Result<Box<dyn ActiveKeyExchange>, CryptoError>;
}

/// Signature verification against certificates.
pub trait SignatureVerifier: CryptoSafe {
    /// Verify a signature on data using a DER-encoded X.509 certificate.
    fn verify_signature(
        &self,
        cert_der: &[u8],
        data: &[u8],
        signature: &[u8],
        hash_alg: HashAlgorithm,
        sig_alg: SignatureAlgorithm,
    ) -> Result<(), CryptoError>;
}

/// Allow-list of supported (signature, hash, curve) combinations for
/// DTLS 1.2 signature verification.
///
/// In DTLS 1.2 the hash algorithm and the certificate's curve are
/// independent choices, so all cross-combinations are valid.
///
///  Signature | Hash    | Curve
/// -----------+---------+-----------
///  ECDSA     | SHA-256 | P-256
///  ECDSA     | SHA-256 | P-384
///  ECDSA     | SHA-384 | P-256
///  ECDSA     | SHA-384 | P-384
const SUPPORTED_VERIFY_SCHEMES: &[(SignatureAlgorithm, HashAlgorithm, NamedGroup)] = &[
    (
        SignatureAlgorithm::ECDSA,
        HashAlgorithm::SHA256,
        NamedGroup::Secp256r1,
    ),
    (
        SignatureAlgorithm::ECDSA,
        HashAlgorithm::SHA256,
        NamedGroup::Secp384r1,
    ),
    (
        SignatureAlgorithm::ECDSA,
        HashAlgorithm::SHA384,
        NamedGroup::Secp256r1,
    ),
    (
        SignatureAlgorithm::ECDSA,
        HashAlgorithm::SHA384,
        NamedGroup::Secp384r1,
    ),
];

/// Check that a (signature, hash, curve) combination is in the allow-list.
pub fn check_verify_scheme(
    sig_alg: SignatureAlgorithm,
    hash_alg: HashAlgorithm,
    group: NamedGroup,
) -> Result<(), CryptoError> {
    if SUPPORTED_VERIFY_SCHEMES
        .iter()
        .any(|(s, h, g)| *s == sig_alg && *h == hash_alg && *g == group)
    {
        Ok(())
    } else {
        Err(CryptoError::UnsupportedSignatureVerification {
            signature: sig_alg,
            hash: hash_alg,
            group,
        })
    }
}

/// Extract the EC curve ([`NamedGroup`]) from a DER-encoded X.509 certificate.
///
/// Used by DTLS 1.3 to verify that the [`SignatureScheme`](crate::types::SignatureScheme)
/// in `CertificateVerify` is consistent with the peer's certificate key.
#[cfg(feature = "_crypto-common")]
pub fn cert_named_group(cert_der: &[u8]) -> Result<NamedGroup, CertificateError> {
    use der::Decode;
    use spki::ObjectIdentifier;
    use x509_cert::Certificate as X509Certificate;

    let cert = X509Certificate::from_der(cert_der).map_err(|_| CertificateError::ParseFailed)?;
    let spki = cert.tbs_certificate().subject_public_key_info();

    let curve_oid: ObjectIdentifier = spki
        .algorithm
        .parameters
        .as_ref()
        .ok_or(CertificateError::MissingEcCurveParameter)?
        .decode_as()
        .map_err(|_| CertificateError::InvalidEcCurveParameter)?;

    match curve_oid {
        OID_P256 => Ok(NamedGroup::Secp256r1),
        OID_P384 => Ok(NamedGroup::Secp384r1),
        _ => Err(CertificateError::UnsupportedEcCurve),
    }
}

/// Private key parser (factory for SigningKey).
pub trait KeyProvider: CryptoSafe {
    /// Parse and load a private key from DER/PEM bytes.
    fn load_private_key(&self, key_der: &[u8]) -> Result<Box<dyn SigningKey>, CryptoError>;
}

/// Secure random number generator.
pub trait SecureRandom: CryptoSafe {
    /// Fill buffer with cryptographically secure random bytes.
    fn fill(&self, buf: &mut [u8]) -> Result<(), CryptoError>;
}

/// Hash provider (factory for HashContext).
pub trait HashProvider: CryptoSafe {
    /// Create a new hash context for the specified algorithm.
    fn create_hash(&self, algorithm: HashAlgorithm) -> Box<dyn HashContext>;
}

/// HMAC provider for computing HMAC signatures.
pub trait HmacProvider: CryptoSafe {
    /// Compute HMAC-SHA256(key, data) and return the result.
    fn hmac_sha256(&self, key: &[u8], data: &[u8]) -> Result<[u8; 32], CryptoError> {
        let mut out = [0u8; 32];
        self.hmac(HashAlgorithm::SHA256, key, data, &mut out)?;
        Ok(out)
    }

    /// Compute HMAC for the given hash algorithm, writing the result to `out`.
    ///
    /// Returns the number of bytes written.
    fn hmac(
        &self,
        hash: HashAlgorithm,
        key: &[u8],
        data: &[u8],
        out: &mut [u8],
    ) -> Result<usize, CryptoError>;
}

// ============================================================================
// DTLS 1.3 Factory Traits
// ============================================================================

/// Cipher suite support for DTLS 1.3 (factory for Cipher instances).
///
/// Unlike DTLS 1.2 cipher suites, TLS 1.3 cipher suites only specify the
/// AEAD algorithm and hash function. Key exchange is negotiated separately.
pub trait SupportedDtls13CipherSuite: CryptoSafe {
    /// The cipher suite this supports.
    fn suite(&self) -> Dtls13CipherSuite;

    /// Hash algorithm used by this suite.
    fn hash_algorithm(&self) -> HashAlgorithm;

    /// AEAD key length in bytes.
    fn key_len(&self) -> usize;

    /// AEAD nonce/IV length in bytes.
    fn iv_len(&self) -> usize;

    /// AEAD tag length in bytes.
    fn tag_len(&self) -> usize;

    /// Minimum length, in bytes, of a protected record's encrypted fragment.
    /// DTLS 1.3 has no explicit nonce in the record, so this equals
    /// [`Self::tag_len`]. Records shorter than this cannot hold a valid
    /// ciphertext + tag and are rejected at the record boundary.
    fn min_protected_fragment_len(&self) -> usize {
        self.tag_len()
    }

    /// Create a cipher instance with the given key.
    fn create_cipher(&self, key: &[u8]) -> Result<Box<dyn Cipher>, CryptoError>;

    /// Compute a mask for record number encryption (RFC 9147 Section 4.2.3).
    ///
    /// The mask is XORed over the sequence number bytes in the header.
    /// `sample` is the first 16 bytes of the ciphertext.
    ///
    /// For AES-based suites: `mask = AES-ECB(sn_key, sample)`.
    ///
    /// For ChaCha20-based suites (RFC 9001 Section 5.4.4):
    /// `counter = sample[0..4]` (LE u32), `nonce = sample[4..16]`,
    /// `mask = ChaCha20(sn_key, counter, nonce, <zero bytes>)`.
    fn encrypt_sn(&self, sn_key: &[u8], sample: &[u8; 16]) -> [u8; 16];
}

// ============================================================================
// Core Provider Struct
// ============================================================================

/// Cryptographic provider for DTLS operations.
///
/// This struct holds references to all cryptographic components needed
/// for DTLS. Users can provide custom implementations of each component
/// to replace the default aws-lc-rs-based provider.
///
/// # Version-Specific Components
///
/// Shared components like `kx_groups`, `signature_verification`, `key_provider`,
/// `secure_random`, `hash_provider`, and `hmac_provider` are used by both versions.
/// PRF (TLS 1.2) and HKDF (TLS 1.3) key derivation are built generically on top
/// of `hmac_provider` — see the [`prf_hkdf`](super::prf_hkdf) module.
///
/// # Design
///
/// The provider uses static trait object references (`&'static dyn Trait`) which
/// provides zero runtime overhead for trait dispatch. This design is inspired by
/// rustls's CryptoProvider and ensures efficient crypto operations.
///
/// # Example
///
/// ```
/// # #[cfg(feature = "aws-lc-rs")]
/// # fn main() {
/// use dimpl::crypto::{CryptoProvider, aws_lc_rs};
///
/// // Use the default provider
/// let provider = aws_lc_rs::default_provider();
///
/// // Or build a custom one (using defaults for demonstration)
/// let custom_provider = CryptoProvider {
///     // Shared components
///     kx_groups: provider.kx_groups,
///     signature_verification: provider.signature_verification,
///     key_provider: provider.key_provider,
///     secure_random: provider.secure_random,
///     hash_provider: provider.hash_provider,
///     hmac_provider: provider.hmac_provider,
///     // DTLS 1.2 components
///     cipher_suites: provider.cipher_suites,
///     // DTLS 1.3 components
///     dtls13_cipher_suites: provider.dtls13_cipher_suites,
/// };
/// # }
/// # #[cfg(not(feature = "aws-lc-rs"))]
/// # fn main() {}
/// ```
#[derive(Debug, Clone)]
pub struct CryptoProvider {
    // =========================================================================
    // Shared components (used by both DTLS 1.2 and DTLS 1.3)
    // =========================================================================
    /// Supported key exchange groups (P-256, P-384, X25519).
    ///
    /// Used for ECDHE key exchange in both DTLS versions.
    pub kx_groups: &'static [&'static dyn SupportedKxGroup],

    /// Signature verification for certificates.
    pub signature_verification: &'static dyn SignatureVerifier,

    /// Key provider for parsing private keys.
    pub key_provider: &'static dyn KeyProvider,

    /// Secure random number generator.
    pub secure_random: &'static dyn SecureRandom,

    /// Hash provider for handshake hashing.
    pub hash_provider: &'static dyn HashProvider,

    /// HMAC provider for computing HMAC signatures.
    pub hmac_provider: &'static dyn HmacProvider,

    // =========================================================================
    // DTLS 1.2 specific components
    // =========================================================================
    /// Supported DTLS 1.2 cipher suites (for negotiation).
    ///
    /// These cipher suites bundle key exchange, authentication, encryption,
    /// and MAC algorithms together (e.g., TLS_ECDHE_ECDSA_WITH_AES_128_GCM_SHA256).
    pub cipher_suites: &'static [&'static dyn SupportedDtls12CipherSuite],

    // =========================================================================
    // DTLS 1.3 specific components
    // =========================================================================
    /// Supported DTLS 1.3 cipher suites (for negotiation).
    ///
    /// TLS 1.3 cipher suites only specify the AEAD and hash algorithms
    /// (e.g., TLS_AES_128_GCM_SHA256). Key exchange is negotiated separately.
    pub dtls13_cipher_suites: &'static [&'static dyn SupportedDtls13CipherSuite],
}

/// Static storage for the default crypto provider.
///
/// This is set by `install_default()` and retrieved by `get_default()`.
static DEFAULT: OnceLock<CryptoProvider> = OnceLock::new();

impl CryptoProvider {
    /// Install a default crypto provider for the process.
    ///
    /// This sets a global default provider that will be used by
    /// [`Config::builder()`](crate::Config::builder)
    /// when no explicit provider is specified. This is useful for applications that want
    /// to override the default provider per process.
    ///
    /// # Panics
    ///
    /// Panics if called more than once. The default provider can only be set once per process.
    ///
    /// # Example
    ///
    /// ```
    /// # #[cfg(feature = "aws-lc-rs")]
    /// # fn main() {
    /// use dimpl::crypto::{CryptoProvider, aws_lc_rs};
    ///
    /// // Install a default provider (can only be called once per process)
    /// CryptoProvider::install_default(aws_lc_rs::default_provider());
    /// # }
    /// # #[cfg(not(feature = "aws-lc-rs"))]
    /// # fn main() {}
    /// ```
    pub fn install_default(provider: CryptoProvider) {
        DEFAULT
            .set(provider)
            .expect("CryptoProvider::install_default() called more than once");
    }

    /// Get the default crypto provider, if one has been installed.
    ///
    /// Returns `Some(&provider)` if a default provider has been installed via
    /// [`Self::install_default()`], or `None` if no default provider is available.
    ///
    /// This method does not panic. Use [`Config::builder()`](crate::Config::builder) which will handle
    /// the fallback logic automatically.
    ///
    /// # Example
    ///
    /// ```
    /// use dimpl::crypto::CryptoProvider;
    ///
    /// if let Some(provider) = CryptoProvider::get_default() {
    ///     // Use the installed default provider
    /// }
    /// ```
    pub fn get_default() -> Option<&'static CryptoProvider> {
        DEFAULT.get()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(feature = "rcgen")]
    fn cert_named_group_p256() {
        use rcgen::{CertificateParams, KeyPair, PKCS_ECDSA_P256_SHA256};

        let key_pair = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).unwrap();
        let params = CertificateParams::new(Vec::<String>::new()).unwrap();
        let cert = params.self_signed(&key_pair).unwrap();

        let group = cert_named_group(cert.der()).unwrap();
        assert_eq!(group, NamedGroup::Secp256r1);
    }

    #[test]
    #[cfg(feature = "rcgen")]
    fn cert_named_group_p384() {
        use rcgen::{CertificateParams, KeyPair, PKCS_ECDSA_P384_SHA384};

        let key_pair = KeyPair::generate_for(&PKCS_ECDSA_P384_SHA384).unwrap();
        let params = CertificateParams::new(Vec::<String>::new()).unwrap();
        let cert = params.self_signed(&key_pair).unwrap();

        let group = cert_named_group(cert.der()).unwrap();
        assert_eq!(group, NamedGroup::Secp384r1);
    }

    #[test]
    #[cfg(feature = "rcgen")]
    fn cert_named_group_invalid_der() {
        let result = cert_named_group(&[0x00, 0x01, 0x02]);
        assert!(result.is_err());
    }
}
