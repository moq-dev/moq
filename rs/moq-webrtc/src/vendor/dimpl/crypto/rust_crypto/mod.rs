//! RustCrypto cryptographic provider implementation for dimpl.
//!
//! This module provides a pure Rust cryptographic backend for dimpl using
//! crates from the [RustCrypto](https://github.com/RustCrypto) organization.
//!
//! # Feature Flag
//!
//! This module is only available when the `rust-crypto` feature is enabled. The `rust-crypto`
//! feature is included in the default features, so it's enabled by default. To use
//! dimpl without this module, disable default features:
//!
//! ```toml
//! dimpl = { version = "...", default-features = false }
//! ```
//!
//! # Usage
//!
//! The rust-crypto provider is used automatically as a fallback when aws-lc-rs is not available
//! or when explicitly specified:
//!
//! ```
//! # #[cfg(feature = "rcgen")]
//! # fn main() {
//! use std::sync::Arc;
//! use std::time::Instant;
//! use dimpl::{Config, Dtls, certificate};
//! use dimpl::crypto::rust_crypto;
//!
//! let cert = certificate::generate_self_signed_certificate().unwrap();
//! let config = Arc::new(
//!     Config::builder()
//!         .with_crypto_provider(rust_crypto::default_provider())
//!         .build()
//!         .unwrap()
//! );
//! let dtls = Dtls::new_12(config, cert, Instant::now());
//! # }
//! # #[cfg(not(feature = "rcgen"))]
//! # fn main() {}
//! ```

mod cipher_suite;
mod hash;
pub(crate) mod hmac;
mod kx_group;
mod random;
mod sign;

use super::CryptoProvider;

/// Get the default RustCrypto-based crypto provider.
///
/// This provider implements all cryptographic operations required for DTLS 1.2
/// using pure Rust crates from the RustCrypto organization.
///
/// # Supported Cipher Suites
///
/// - `TLS_ECDHE_ECDSA_WITH_AES_128_GCM_SHA256` (0xC02B)
/// - `TLS_ECDHE_ECDSA_WITH_AES_256_GCM_SHA384` (0xC02C)
/// - `TLS_ECDHE_ECDSA_WITH_CHACHA20_POLY1305_SHA256` (0xCCA9)
///
/// # Supported Key Exchange Groups
///
/// - `x25519` (X25519 / Curve25519)
/// - `secp256r1` (P-256, NIST Curve)
/// - `secp384r1` (P-384, NIST Curve)
///
/// # Supported Signature Algorithms
///
/// - ECDSA with P-256 and SHA-256
/// - ECDSA with P-384 and SHA-384
///
/// # Supported Hash Algorithms
///
/// - SHA-256
/// - SHA-384
///
/// # Key Formats
///
/// The key provider supports loading private keys in:
/// - PKCS#8 DER format (most common)
/// - SEC1 DER format (OpenSSL EC private key format)
/// - PEM encoded versions of the above
///
/// # Random Number Generation
///
/// Uses `OsRng` from the `rand` crate for cryptographically secure random number generation.
pub fn default_provider() -> CryptoProvider {
    CryptoProvider {
        // Shared components
        kx_groups: kx_group::ALL_KX_GROUPS,
        signature_verification: &sign::SIGNATURE_VERIFIER,
        key_provider: &sign::KEY_PROVIDER,
        secure_random: &random::SECURE_RANDOM,
        hash_provider: &hash::HASH_PROVIDER,
        hmac_provider: &hmac::HMAC_PROVIDER,
        // DTLS 1.2 components
        cipher_suites: cipher_suite::ALL_CIPHER_SUITES,
        // DTLS 1.3 components
        dtls13_cipher_suites: cipher_suite::ALL_DTLS13_CIPHER_SUITES,
    }
}
