//! AWS-LC-RS cryptographic provider implementation for dimpl.
//!
//! This module provides the default cryptographic backend for dimpl using
//! [aws-lc-rs](https://github.com/aws/aws-lc-rs), Amazon's cryptographic library
//! based on AWS-LC (a fork of BoringSSL).
//!
//! # Feature Flag
//!
//! This module is only available when the `aws-lc-rs` feature is enabled. The `aws-lc-rs`
//! feature is included in the default features, so it's enabled by default. To use
//! dimpl without this module, disable default features:
//!
//! ```toml
//! dimpl = { version = "...", default-features = false }
//! ```
//!
//! # Usage
//!
//! The default provider is used automatically when no custom provider is specified:
//!
//! ```
//! # #[cfg(feature = "rcgen")]
//! # fn main() {
//! use std::sync::Arc;
//! use std::time::Instant;
//! use dimpl::{Config, Dtls, certificate};
//!
//! let cert = certificate::generate_self_signed_certificate().unwrap();
//! // Implicitly uses aws-lc-rs default provider
//! let config = Arc::new(Config::default());
//! let dtls = Dtls::new_12(config, cert, Instant::now());
//! # }
//! # #[cfg(not(feature = "rcgen"))]
//! # fn main() {}
//! ```
//!
//! Or explicitly:
//!
//! ```
//! # #[cfg(feature = "rcgen")]
//! # fn main() {
//! use std::sync::Arc;
//! use std::time::Instant;
//! use dimpl::{Config, Dtls, certificate};
//! use dimpl::crypto::aws_lc_rs;
//!
//! let cert = certificate::generate_self_signed_certificate().unwrap();
//! let config = Arc::new(
//!     Config::builder()
//!         .with_crypto_provider(aws_lc_rs::default_provider())
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

/// Get the default aws-lc-rs based crypto provider.
///
/// This provider implements all cryptographic operations required for DTLS 1.2
/// using the aws-lc-rs library (AWS's cryptographic library based on BoringSSL/AWS-LC).
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
/// Uses `SystemRandom` from aws-lc-rs for cryptographically secure random number generation.
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
