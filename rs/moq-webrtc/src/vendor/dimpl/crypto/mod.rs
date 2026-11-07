//! Cryptographic primitives and helpers used by the DTLS engine.

use std::ops::Deref;

// Internal module imports
mod keying;

// Provider traits and implementations
#[cfg(feature = "aws-lc-rs")]
pub mod aws_lc_rs;

#[cfg(feature = "rust-crypto")]
pub mod rust_crypto;

#[cfg(any(feature = "aws-lc-rs", feature = "rust-crypto"))]
pub(crate) mod ccm_cipher;

mod dtls_aead;
pub mod prf_hkdf;
mod provider;
mod validation;

pub use keying::{KeyingMaterial, SrtpProfile};

// Re-export AEAD types needed for Cipher trait implementations (public API)
pub use dtls_aead::{Aad, Nonce};

// Re-export internal AEAD constants/types for crate-internal use
pub(crate) use dtls_aead::Iv;

// Re-export buffer types for provider trait implementations
pub use crate::buffer::{Buf, TmpBuf};

// Re-export all provider traits and types (similar to rustls structure)
// This allows users to do: use dimpl::crypto::{CryptoProvider, SupportedDtls12CipherSuite, ...};
pub use provider::{
    ActiveKeyExchange, Cipher, CryptoProvider, CryptoSafe, HashContext, HashProvider, HmacProvider,
    KeyProvider, SecureRandom, SignatureVerifier, SigningKey, SupportedDtls12CipherSuite,
    SupportedDtls13CipherSuite, SupportedKxGroup, check_verify_scheme,
};
#[cfg(feature = "_crypto-common")]
pub use provider::{OID_P256, OID_P384, cert_named_group};

// Re-export shared types for provider trait implementations
pub use crate::dtls12::message::Dtls12CipherSuite;
pub use crate::types::{
    Dtls13CipherSuite, HashAlgorithm, NamedGroup, SignatureAlgorithm, SignatureScheme,
};

impl Deref for Aad {
    type Target = [u8];
    fn deref(&self) -> &Self::Target {
        &self.0[..]
    }
}

impl Deref for Nonce {
    type Target = [u8];
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
