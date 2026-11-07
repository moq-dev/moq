//! HMAC implementation using RustCrypto.

use hmac::{Hmac, KeyInit, Mac};
use sha2::{Sha256, Sha384};

use super::super::HmacProvider;
use crate::types::HashAlgorithm;
use crate::{CryptoError, CryptoOperation};

/// HMAC provider implementation.
#[derive(Debug)]
pub(crate) struct RustCryptoHmacProvider;

impl HmacProvider for RustCryptoHmacProvider {
    fn hmac(
        &self,
        hash: HashAlgorithm,
        key: &[u8],
        data: &[u8],
        out: &mut [u8],
    ) -> Result<usize, CryptoError> {
        match hash {
            HashAlgorithm::SHA256 => {
                let mut mac = Hmac::<Sha256>::new_from_slice(key)
                    .map_err(|_| CryptoError::OperationFailed(CryptoOperation::ComputeHmac))?;
                mac.update(data);
                let result = mac.finalize().into_bytes();
                let len = result.len();
                out[..len].copy_from_slice(&result);
                Ok(len)
            }
            HashAlgorithm::SHA384 => {
                let mut mac = Hmac::<Sha384>::new_from_slice(key)
                    .map_err(|_| CryptoError::OperationFailed(CryptoOperation::ComputeHmac))?;
                mac.update(data);
                let result = mac.finalize().into_bytes();
                let len = result.len();
                out[..len].copy_from_slice(&result);
                Ok(len)
            }
            _ => Err(CryptoError::UnsupportedHmacHash(hash)),
        }
    }
}

/// Static instance of the HMAC provider.
pub(crate) static HMAC_PROVIDER: RustCryptoHmacProvider = RustCryptoHmacProvider;
