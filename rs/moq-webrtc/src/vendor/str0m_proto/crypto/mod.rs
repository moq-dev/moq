mod provider;
pub use provider::{AeadAes128Gcm, AeadAes128GcmCipher, AeadAes256Gcm, AeadAes256GcmCipher};
pub use provider::{Aes128CmSha1_80, Aes128CmSha1_80Cipher, CryptoProvider, CryptoSafe};
pub use provider::{Sha1HmacProvider, Sha1Provider, Sha256Provider};
pub use provider::{SrtpProvider, SupportedAeadAes128Gcm};
pub use provider::{SupportedAeadAes256Gcm, SupportedAes128CmSha1_80};

mod error;
pub use error::{CryptoError, DtlsError};

/// DTLS related types and traits.
pub mod dtls {
	pub use super::provider::{DtlsInstance, DtlsProvider};

	pub use crate::vendor::dimpl::CryptoError as DtlsCryptoError;
	pub use crate::vendor::dimpl::CryptoOperation as DtlsCryptoOperation;
	pub use crate::vendor::dimpl::DtlsCertificate as DtlsCert;
	pub use crate::vendor::dimpl::KeyingMaterial;
	pub use crate::vendor::dimpl::ProtocolVersion;
	pub use crate::vendor::dimpl::SrtpProfile;
	// Note: crate::vendor::dimpl::Error is renamed to DtlsImplError to avoid conflict with str0m's DtlsError
	pub use crate::vendor::dimpl::{Error as DtlsImplError, Output as DtlsOutput};
}
