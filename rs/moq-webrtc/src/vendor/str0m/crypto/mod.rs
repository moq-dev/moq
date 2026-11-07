use crate::vendor::str0m_proto::crypto as provider;

pub use provider::{AeadAes128Gcm, AeadAes128GcmCipher, AeadAes256Gcm, AeadAes256GcmCipher};
pub use provider::{Aes128CmSha1_80, Aes128CmSha1_80Cipher, CryptoProvider, CryptoSafe};
pub use provider::{Sha1HmacProvider, Sha1Provider, Sha256Provider};
pub use provider::{SrtpProvider, SupportedAeadAes128Gcm};
pub use provider::{SupportedAeadAes256Gcm, SupportedAes128CmSha1_80};

/// DTLS related types and traits.
pub mod dtls {
	pub use super::provider::dtls::{DtlsInstance, DtlsProvider};

	pub use crate::vendor::dimpl::DtlsCertificate as DtlsCert;
	pub use crate::vendor::dimpl::KeyingMaterial;
	pub use crate::vendor::dimpl::ProtocolVersion;
	pub use crate::vendor::dimpl::SrtpProfile;
	// Note: crate::vendor::dimpl::Error is renamed to DtlsImplError to avoid conflict with str0m's DtlsError
	pub use crate::vendor::dimpl::{Error as DtlsImplError, Output as DtlsOutput};
}

#[cfg(test)]
#[allow(unused)]
pub(crate) fn test_default_provider() -> &'static CryptoProvider {
	use std::sync::OnceLock;
	static TEST_PROVIDER: OnceLock<CryptoProvider> = OnceLock::new();
	TEST_PROVIDER.get_or_init(from_feature_flags)
}

/// The default crypto provider: aws-lc-rs, the only backend kept in the vendored copy.
pub fn from_feature_flags() -> CryptoProvider {
	crate::vendor::str0m_aws_lc_rs::default_provider()
}

mod finger;
pub use finger::Fingerprint;

pub use crate::vendor::str0m_proto::crypto::{CryptoError, DtlsError};
