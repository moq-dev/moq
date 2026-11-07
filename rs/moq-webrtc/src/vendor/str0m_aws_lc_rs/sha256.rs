//! AWS-LC-RS SHA-256 implementation.

use aws_lc_rs::digest;

use str0m_proto::crypto::Sha256Provider;

/// AWS-LC-RS-based SHA-256 provider.
#[derive(Debug)]
pub struct AwsLcRsSha256Provider;

impl Sha256Provider for AwsLcRsSha256Provider {
    fn sha256(&self, data: &[u8]) -> [u8; 32] {
        let hash = digest::digest(&digest::SHA256, data);
        let mut result = [0u8; 32];
        result.copy_from_slice(hash.as_ref());
        result
    }
}
