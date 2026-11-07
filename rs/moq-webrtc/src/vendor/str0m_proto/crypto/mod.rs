mod provider;
pub use provider::{AeadAes128Gcm, AeadAes128GcmCipher, AeadAes256Gcm, AeadAes256GcmCipher};
pub use provider::{Aes128CmSha1_80, Aes128CmSha1_80Cipher, CryptoProvider, CryptoSafe};
pub use provider::{DtlsVersion, Sha1HmacProvider, Sha1Provider, Sha256Provider};
pub use provider::{SrtpProvider, SupportedAeadAes128Gcm};
pub use provider::{SupportedAeadAes256Gcm, SupportedAes128CmSha1_80};

mod error;
pub use error::{CryptoError, DtlsError};

/// DTLS related types and traits.
pub mod dtls {
    pub use super::provider::{DtlsInstance, DtlsProvider};

    pub use dimpl::CryptoError as DtlsCryptoError;
    pub use dimpl::CryptoOperation as DtlsCryptoOperation;
    pub use dimpl::DtlsCertificate as DtlsCert;
    pub use dimpl::KeyingMaterial;
    pub use dimpl::ProtocolVersion;
    pub use dimpl::SrtpProfile;
    // Note: dimpl::Error is renamed to DtlsImplError to avoid conflict with str0m's DtlsError
    pub use dimpl::{Error as DtlsImplError, Output as DtlsOutput};
}
