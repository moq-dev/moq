//! HMAC implementation using aws-lc-rs.

use aws_lc_rs::hmac;

use super::super::HmacProvider;
use crate::CryptoError;
use crate::types::HashAlgorithm;

/// Get HMAC algorithm from hash algorithm.
fn hmac_algorithm(hash: HashAlgorithm) -> Result<hmac::Algorithm, CryptoError> {
    match hash {
        HashAlgorithm::SHA256 => Ok(hmac::HMAC_SHA256),
        HashAlgorithm::SHA384 => Ok(hmac::HMAC_SHA384),
        _ => Err(CryptoError::UnsupportedHmacHash(hash)),
    }
}

/// HMAC provider implementation.
#[derive(Debug)]
pub(crate) struct AwsLcHmacProvider;

impl HmacProvider for AwsLcHmacProvider {
    fn hmac(
        &self,
        hash: HashAlgorithm,
        key: &[u8],
        data: &[u8],
        out: &mut [u8],
    ) -> Result<usize, CryptoError> {
        let algorithm = hmac_algorithm(hash)?;
        let hmac_key = hmac::Key::new(algorithm, key);
        let tag = hmac::sign(&hmac_key, data);
        let len = tag.as_ref().len();
        out[..len].copy_from_slice(tag.as_ref());
        Ok(len)
    }
}

/// Static instance of the HMAC provider.
pub(crate) static HMAC_PROVIDER: AwsLcHmacProvider = AwsLcHmacProvider;
