//! Cryptographic primitives and helpers used by the DTLS engine.

use std::ops::Deref;

// Internal module imports
mod keying;

// Provider traits and implementations
pub mod aws_lc_rs;

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
pub use crate::vendor::dimpl::buffer::{Buf, TmpBuf};

// Re-export all provider traits and types (similar to rustls structure)
// This allows users to do: use crate::vendor::dimpl::crypto::{CryptoProvider, SupportedDtls12CipherSuite, ...};
pub use provider::{
	ActiveKeyExchange, Cipher, CryptoProvider, CryptoSafe, HashContext, HashProvider, HmacProvider, KeyProvider,
	SecureRandom, SignatureVerifier, SigningKey, SupportedDtls12CipherSuite, SupportedKxGroup, check_verify_scheme,
};
pub use provider::{OID_P256, OID_P384};

// Re-export shared types for provider trait implementations
pub use crate::vendor::dimpl::dtls12::message::Dtls12CipherSuite;
pub use crate::vendor::dimpl::types::{HashAlgorithm, NamedGroup, SignatureAlgorithm};

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
