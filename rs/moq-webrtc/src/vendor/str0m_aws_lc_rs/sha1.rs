//! SHA-1 hash and HMAC implementations using AWS-LC-RS.

use aws_lc_rs::{digest, hmac};

use str0m_proto::crypto::{Sha1HmacProvider, Sha1Provider};

#[derive(Debug)]
pub(super) struct AwsLcRsSha1Provider;

impl Sha1Provider for AwsLcRsSha1Provider {
    fn sha1(&self, data: &[u8]) -> [u8; 20] {
        let digest = digest::digest(&digest::SHA1_FOR_LEGACY_USE_ONLY, data);
        let mut output = [0u8; 20];
        output.copy_from_slice(digest.as_ref());
        output
    }
}

// ============================================================================
// SHA1 HMAC Provider Implementation
// ============================================================================

pub(super) struct AwsLcRsSha1HmacProvider;

impl std::fmt::Debug for AwsLcRsSha1HmacProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AwsLcRsSha1HmacProvider").finish()
    }
}

impl Sha1HmacProvider for AwsLcRsSha1HmacProvider {
    fn sha1_hmac(&self, key: &[u8], payloads: &[&[u8]]) -> [u8; 20] {
        let key = hmac::Key::new(hmac::HMAC_SHA1_FOR_LEGACY_USE_ONLY, key);
        let mut ctx = hmac::Context::with_key(&key);

        for payload in payloads {
            ctx.update(payload);
        }

        let tag = ctx.sign();
        let mut result = [0u8; 20];
        result.copy_from_slice(tag.as_ref());
        result
    }
}
