//! AWS-LC-RS implementation of cryptographic functions.
//! DTLS via dimpl with AWS-LC-RS as crypto backend.

// Avoid rewriting existing code: nested conditions are often more readable than long if-chains.
#![allow(clippy::collapsible_if)]

mod dtls;
mod sha1;
mod sha256;
mod srtp;

use dtls::AwsLcRsDtlsProvider;
use sha1::{AwsLcRsSha1HmacProvider, AwsLcRsSha1Provider};
use sha256::AwsLcRsSha256Provider;
use srtp::AwsLcRsSrtpProvider;
use str0m_proto::crypto::CryptoProvider;

/// Create the default AWS-LC-RS crypto provider.
///
/// This provider implements all cryptographic operations required for WebRTC:
/// - DTLS 1.2 for secure key exchange (using dimpl protocol + AWS-LC-RS)
/// - SRTP for encrypted media
/// - SHA1-HMAC for STUN message integrity
/// - SHA-1 and SHA-256 for certificate fingerprints
///
/// # Supported SRTP Profiles
///
/// - `SRTP_AES128_CM_SHA1_80`
/// - `SRTP_AEAD_AES_128_GCM`
/// - `SRTP_AEAD_AES_256_GCM`
pub fn default_provider() -> CryptoProvider {
    static SRTP: AwsLcRsSrtpProvider = AwsLcRsSrtpProvider;
    static SHA1_HMAC: AwsLcRsSha1HmacProvider = AwsLcRsSha1HmacProvider;
    static SHA1: AwsLcRsSha1Provider = AwsLcRsSha1Provider;
    static SHA256: AwsLcRsSha256Provider = AwsLcRsSha256Provider;
    static DTLS: AwsLcRsDtlsProvider = AwsLcRsDtlsProvider;

    CryptoProvider {
        srtp_provider: &SRTP,
        sha1_hmac_provider: &SHA1_HMAC,
        sha1_provider: &SHA1,
        sha256_provider: &SHA256,
        dtls_provider: &DTLS,
    }
}

/// Create the AWS-LC-RS crypto provider used by the Dimpl DTLS backend.
///
/// This allows callers that need non-default Dimpl configuration to construct
/// `dimpl::Config` directly while retaining the AWS-LC-RS implementation.
pub fn dimpl_crypto_provider() -> dimpl::crypto::CryptoProvider {
    dimpl::crypto::aws_lc_rs::default_provider()
}

#[cfg(test)]
mod tests {
    #[test]
    fn exposed_dimpl_provider_builds_config() {
        dimpl::Config::builder()
            .with_crypto_provider(super::dimpl_crypto_provider())
            .build()
            .expect("AWS-LC-RS Dimpl provider should build a valid config");
    }
}
