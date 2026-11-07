//! Secure random number generation using aws-lc-rs.

use super::super::SecureRandom;
use crate::{CryptoError, CryptoOperation};

/// Secure random number generator implementation.
#[derive(Debug)]
pub(super) struct AwsLcSecureRandom;

impl SecureRandom for AwsLcSecureRandom {
    fn fill(&self, buf: &mut [u8]) -> Result<(), CryptoError> {
        use aws_lc_rs::rand::SecureRandom as _;
        let rng = aws_lc_rs::rand::SystemRandom::new();
        rng.fill(buf)
            .map_err(|_| CryptoError::OperationFailed(CryptoOperation::FillRandom))
    }
}

/// Static instance of the secure random generator.
pub(super) static SECURE_RANDOM: AwsLcSecureRandom = AwsLcSecureRandom;
