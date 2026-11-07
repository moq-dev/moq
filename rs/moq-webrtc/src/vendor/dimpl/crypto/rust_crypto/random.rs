//! Secure random number generation using RustCrypto.

use super::super::SecureRandom;
use crate::{CryptoError, CryptoOperation};

/// Secure random number generator implementation.
#[derive(Debug)]
pub(super) struct RustCryptoSecureRandom;

impl SecureRandom for RustCryptoSecureRandom {
    fn fill(&self, buf: &mut [u8]) -> Result<(), CryptoError> {
        use rand::{TryRng, rngs::SysRng};
        SysRng
            .try_fill_bytes(buf)
            .map_err(|_| CryptoError::OperationFailed(CryptoOperation::FillRandom))
    }
}

/// Static instance of the secure random generator.
pub(super) static SECURE_RANDOM: RustCryptoSecureRandom = RustCryptoSecureRandom;
