//! Cryptographic provider traits for pluggable crypto backends.
//!
//! This module defines the trait-based interface for cryptographic operations
//! in str0m, allowing users to provide custom crypto implementations for SRTP,
//! SHA-1 and SHA-256 hashing, SHA1 HMAC (for STUN), and DTLS.
//!
//! Implementors of a crypto provider only need to depend on this module.

use std::fmt;
use std::fmt::Debug;
use std::panic::{RefUnwindSafe, UnwindSafe};
use std::sync::OnceLock;
use std::time::Instant;

use subtle::ConstantTimeEq;

use crate::crypto::dtls::*;
use crate::crypto::error::CryptoError;

// ============================================================================
// CryptoProvider
// ============================================================================

/// Cryptographic provider for SRTP, hashing, SHA1 HMAC, and DTLS operations.
///
/// This struct holds references to all cryptographic components needed
/// for WebRTC operations. Users can provide custom implementations of each component
/// to replace the default OpenSSL-based provider.
///
/// # Design
///
/// The provider uses static trait object references (`&'static dyn Trait`) which
/// provides zero runtime overhead for trait dispatch. This design is inspired by
/// dimpl's CryptoProvider and ensures efficient crypto operations.
#[derive(Debug, Clone)]
pub struct CryptoProvider {
    /// SRTP provider for creating cipher instances and key derivation.
    pub srtp_provider: &'static dyn SrtpProvider,
    /// SHA1 HMAC provider for STUN message integrity.
    pub sha1_hmac_provider: &'static dyn Sha1HmacProvider,
    /// SHA-1 hash provider for legacy certificate fingerprints.
    pub sha1_provider: &'static dyn Sha1Provider,
    /// SHA-256 hash provider.
    pub sha256_provider: &'static dyn Sha256Provider,
    /// DTLS provider for creating DTLS instances.
    pub dtls_provider: &'static dyn DtlsProvider,
}

/// CryptoProvider contains only static references to thread-safe traits,
/// so it's safe to use across panic boundaries.
impl UnwindSafe for CryptoProvider {}
impl RefUnwindSafe for CryptoProvider {}

/// Static storage for the default crypto provider.
static DEFAULT: OnceLock<CryptoProvider> = OnceLock::new();

impl CryptoProvider {
    /// Install this provider as the process-wide default.
    pub fn install_process_default(self) {
        let _ = DEFAULT.set(self);
    }

    /// Install a default crypto provider for the process.
    ///
    /// # Panics
    ///
    /// Panics if called more than once.
    pub fn install_default(provider: CryptoProvider) {
        DEFAULT
            .set(provider)
            .expect("CryptoProvider::install_default() called more than once");
    }

    /// Get the default crypto provider, if one has been installed.
    pub fn get_default() -> Option<&'static CryptoProvider> {
        DEFAULT.get()
    }
}

impl fmt::Display for CryptoProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "CryptoProvider")
    }
}

// ============================================================================
// Marker Trait
// ============================================================================

pub use crate::{CryptoSafe, Sha1HmacProvider};

// ============================================================================
// Main Provider Traits
// ============================================================================

/// SRTP provider for creating cipher instances and key derivation.
pub trait SrtpProvider: CryptoSafe {
    /// Factory for AES-128-CM-SHA1-80 ciphers.
    fn aes_128_cm_sha1_80(&self) -> &'static dyn SupportedAes128CmSha1_80;

    /// Factory for AEAD-AES-128-GCM ciphers.
    fn aead_aes_128_gcm(&self) -> &'static dyn SupportedAeadAes128Gcm;

    /// Factory for AEAD-AES-256-GCM ciphers.
    fn aead_aes_256_gcm(&self) -> &'static dyn SupportedAeadAes256Gcm;

    /// Perform AES-128-ECB round for key derivation.
    fn srtp_aes_128_ecb_round(&self, key: &[u8], input: &[u8], output: &mut [u8]);

    /// Perform AES-256-ECB round for key derivation.
    fn srtp_aes_256_ecb_round(&self, key: &[u8], input: &[u8], output: &mut [u8]);
}

/// SHA-1 hash provider for legacy certificate fingerprints.
pub trait Sha1Provider: CryptoSafe {
    /// Compute SHA-1 hash of the input data.
    fn sha1(&self, data: &[u8]) -> [u8; 20];
}

/// SHA-256 hash provider.
pub trait Sha256Provider: CryptoSafe {
    /// Compute SHA-256 hash of the input data.
    fn sha256(&self, data: &[u8]) -> [u8; 32];
}

/// Which DTLS version(s) to use for the handshake.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum DtlsVersion {
    /// Use only DTLS 1.2.
    #[default]
    Dtls12,
    /// Use only DTLS 1.3.
    Dtls13,
    /// Auto-detect: the first incoming ClientHello determines the version.
    Auto,
}

impl fmt::Display for DtlsVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DtlsVersion::Dtls12 => write!(f, "DTLS 1.2"),
            DtlsVersion::Dtls13 => write!(f, "DTLS 1.3"),
            DtlsVersion::Auto => write!(f, "DTLS Auto-detect"),
        }
    }
}

/// Factory for DTLS instances and certificates.
pub trait DtlsProvider: CryptoSafe {
    /// Generate a new self-signed DTLS certificate.
    ///
    /// Returns `None` if this provider does not support certificate generation.
    /// In that case, the user must supply a certificate via `RtcConfig::set_dtls_cert`.
    fn generate_certificate(&self) -> Option<DtlsCert>;

    /// Create a new DTLS instance with the given certificate.
    ///
    /// mtu: Backends that can size their record layer according to the provided MTU should do so;
    /// backends that cannot ignore it. `None` means use the backend's default.
    fn new_dtls(
        &self,
        cert: &DtlsCert,
        now: Instant,
        dtls_version: DtlsVersion,
        mtu: Option<usize>,
    ) -> Result<Box<dyn DtlsInstance>, CryptoError>;

    /// Whether the provider is used in a test context.
    fn is_test(&self) -> bool {
        cfg!(feature = "_internal_test_exports")
    }
}

// ============================================================================
// DTLS Instance Trait
// ============================================================================

/// DTLS instance operations (matches dimpl's Dtls API surface).
pub trait DtlsInstance: CryptoSafe {
    /// Set whether this instance is active (client) or passive (server).
    fn set_active(&mut self, active: bool);

    /// Handle an incoming DTLS packet.
    fn handle_packet(&mut self, packet: &[u8]) -> Result<(), DtlsImplError>;

    /// Poll for output from the DTLS instance.
    ///
    /// The buffer must be large enough to hold the largest possible DTLS record.
    fn poll_output<'a>(&mut self, buf: &'a mut [u8]) -> DtlsOutput<'a>;

    /// Handle a timeout event.
    fn handle_timeout(&mut self, now: Instant) -> Result<(), DtlsImplError>;

    /// Send application data over DTLS.
    ///
    /// Returns the number of bytes accepted for buffering or transmission.
    /// Accepts either the entire slice or none of it; retain unaccepted data
    /// and retry after the handshake progresses. An empty slice returns zero
    /// on success and does not indicate handshake readiness.
    fn send_application_data(&mut self, data: &[u8]) -> Result<usize, DtlsImplError>;

    /// Return true if the instance is operating in the client role.
    fn is_active(&self) -> bool;

    /// Return the negotiated DTLS protocol version. May return `None` before handshake completion.
    fn protocol_version(&self) -> Option<ProtocolVersion>;

    /// Return true after close has started and while close output may still need polling.
    fn is_closing(&self) -> bool {
        false
    }

    /// Return true once close output has drained and the instance is closed.
    fn is_closed(&self) -> bool {
        false
    }

    /// Initiate graceful shutdown by sending a DTLS `close_notify` alert.
    fn close(&mut self) -> Result<(), DtlsImplError>;
}

// ============================================================================
// SRTP Cipher Factory Traits
// ============================================================================

/// Factory for AES-128-CM-SHA1-80 cipher instances.
pub trait SupportedAes128CmSha1_80: CryptoSafe {
    /// Create a cipher instance with the given key.
    fn create_cipher(&self, key: [u8; 16], encrypt: bool) -> Box<dyn Aes128CmSha1_80Cipher>;
}

/// Factory for AEAD-AES-128-GCM cipher instances.
pub trait SupportedAeadAes128Gcm: CryptoSafe {
    /// Create a cipher instance with the given key.
    fn create_cipher(&self, key: [u8; 16], encrypt: bool) -> Box<dyn AeadAes128GcmCipher>;
}

/// Factory for AEAD-AES-256-GCM cipher instances.
pub trait SupportedAeadAes256Gcm: CryptoSafe {
    /// Create a cipher instance with the given key.
    fn create_cipher(&self, key: [u8; 32], encrypt: bool) -> Box<dyn AeadAes256GcmCipher>;
}

// ============================================================================
// SRTP Cipher Instance Traits
// ============================================================================

/// AES-128-CM-SHA1-80 cipher instance for SRTP encryption/decryption.
pub trait Aes128CmSha1_80Cipher: CryptoSafe {
    /// Encrypt input with the given IV.
    fn encrypt(
        &mut self,
        iv: &[u8; 16],
        input: &[u8],
        output: &mut [u8],
    ) -> Result<(), CryptoError>;

    /// Decrypt input with the given IV.
    fn decrypt(
        &mut self,
        iv: &[u8; 16],
        input: &[u8],
        output: &mut [u8],
    ) -> Result<(), CryptoError>;
}

/// AEAD-AES-128-GCM cipher instance for SRTP encryption/decryption.
pub trait AeadAes128GcmCipher: CryptoSafe {
    /// Encrypt input with the given IV and AAD.
    fn encrypt(
        &mut self,
        iv: &[u8; 12],
        aad: &[u8],
        input: &[u8],
        output: &mut [u8],
    ) -> Result<(), CryptoError>;

    /// Decrypt input with the given IV and AADs.
    fn decrypt(
        &mut self,
        iv: &[u8; 12],
        aads: &[&[u8]],
        input: &[u8],
        output: &mut [u8],
    ) -> Result<usize, CryptoError>;
}

/// AEAD-AES-256-GCM cipher instance for SRTP encryption/decryption.
pub trait AeadAes256GcmCipher: CryptoSafe {
    /// Encrypt input with the given IV and AAD.
    fn encrypt(
        &mut self,
        iv: &[u8; 12],
        aad: &[u8],
        input: &[u8],
        output: &mut [u8],
    ) -> Result<(), CryptoError>;

    /// Decrypt input with the given IV and AADs.
    fn decrypt(
        &mut self,
        iv: &[u8; 12],
        aads: &[&[u8]],
        input: &[u8],
        output: &mut [u8],
    ) -> Result<usize, CryptoError>;
}

// ============================================================================
// SRTP Profile Definitions
// ============================================================================

/// AES-128-CM-SHA1-80 SRTP profile.
///
/// This struct provides constants, types, and protocol functions for the
/// AES-128-CM-SHA1-80 SRTP protection profile (RFC 5764).
#[derive(Debug, Clone, Copy)]
pub struct Aes128CmSha1_80;

impl Aes128CmSha1_80 {
    /// Key length in bytes.
    pub const KEY_LEN: usize = 16;
    /// Salt length in bytes.
    pub const SALT_LEN: usize = 14;
    /// HMAC key length in bytes.
    pub const HMAC_KEY_LEN: usize = 20;
    /// HMAC tag length in bytes.
    pub const HMAC_TAG_LEN: usize = 10;

    /// Compute and append RTP HMAC tag.
    pub fn rtp_hmac(
        sha1_hmac: impl Fn(&[u8], &[&[u8]]) -> [u8; 20],
        key: &[u8],
        buf: &mut [u8],
        srtp_index: u64,
        hmac_start: usize,
    ) {
        let roc = (srtp_index >> 16) as u32;
        let tag = sha1_hmac(key, &[&buf[..hmac_start], &roc.to_be_bytes()]);
        buf[hmac_start..(hmac_start + Self::HMAC_TAG_LEN)]
            .copy_from_slice(&tag[0..Self::HMAC_TAG_LEN]);
    }

    /// Verify RTP HMAC tag.
    pub fn rtp_verify(
        sha1_hmac: impl Fn(&[u8], &[&[u8]]) -> [u8; 20],
        key: &[u8],
        buf: &[u8],
        srtp_index: u64,
        cmp: &[u8],
    ) -> bool {
        let roc = (srtp_index >> 16) as u32;
        let tag = sha1_hmac(key, &[buf, &roc.to_be_bytes()]);
        tag[0..Self::HMAC_TAG_LEN].ct_eq(cmp).into()
    }

    /// Compute RTP IV.
    pub fn rtp_iv(salt: [u8; 14], ssrc: u32, srtp_index: u64) -> [u8; 16] {
        let mut iv = [0; 16];
        let ssrc_be = ssrc.to_be_bytes();
        let srtp_be = srtp_index.to_be_bytes();
        iv[4..8].copy_from_slice(&ssrc_be);
        for i in 0..8 {
            iv[i + 6] ^= srtp_be[i];
        }
        for i in 0..14 {
            iv[i] ^= salt[i];
        }
        iv
    }

    /// Compute and append RTCP HMAC tag.
    pub fn rtcp_hmac(
        sha1_hmac: impl Fn(&[u8], &[&[u8]]) -> [u8; 20],
        key: &[u8],
        buf: &mut [u8],
        hmac_index: usize,
    ) {
        let tag = sha1_hmac(key, &[&buf[0..hmac_index]]);
        buf[hmac_index..(hmac_index + Self::HMAC_TAG_LEN)]
            .copy_from_slice(&tag[0..Self::HMAC_TAG_LEN]);
    }

    /// Verify RTCP HMAC tag.
    pub fn rtcp_verify(
        sha1_hmac: impl Fn(&[u8], &[&[u8]]) -> [u8; 20],
        key: &[u8],
        buf: &[u8],
        cmp: &[u8],
    ) -> bool {
        let tag = sha1_hmac(key, &[buf]);
        tag[0..Self::HMAC_TAG_LEN].ct_eq(cmp).into()
    }
}

/// AEAD-AES-128-GCM SRTP profile.
///
/// This struct provides constants, types, and protocol functions for the
/// AEAD-AES-128-GCM SRTP protection profile (RFC 7714).
#[derive(Debug, Clone, Copy)]
pub struct AeadAes128Gcm;

impl AeadAes128Gcm {
    /// Key length in bytes.
    pub const KEY_LEN: usize = 16;
    /// Salt length in bytes.
    pub const SALT_LEN: usize = 12;
    /// RTCP AAD length in bytes.
    pub const RTCP_AAD_LEN: usize = 12;
    /// Authentication tag length in bytes.
    pub const TAG_LEN: usize = 16;
    /// IV length in bytes.
    pub const IV_LEN: usize = 12;

    /// Compute RTP IV.
    pub fn rtp_iv(salt: [u8; 12], ssrc: u32, roc: u32, seq: u16) -> [u8; 12] {
        // See: https://www.rfc-editor.org/rfc/rfc7714#section-8.1
        let mut iv = [0; 12];

        let ssrc_be = ssrc.to_be_bytes();
        let roc_be = roc.to_be_bytes();
        let seq_be = seq.to_be_bytes();

        iv[2..6].copy_from_slice(&ssrc_be);
        iv[6..10].copy_from_slice(&roc_be);
        iv[10..12].copy_from_slice(&seq_be);

        for i in 0..12 {
            iv[i] ^= salt[i];
        }

        iv
    }

    /// Compute RTCP IV.
    pub fn rtcp_iv(salt: [u8; 12], ssrc: u32, srtp_index: u32) -> [u8; 12] {
        // See: https://www.rfc-editor.org/rfc/rfc7714#section-9.1
        let mut iv = [0; 12];

        let ssrc_be = ssrc.to_be_bytes();
        let srtp_be = srtp_index.to_be_bytes();

        iv[2..6].copy_from_slice(&ssrc_be);
        iv[8..12].copy_from_slice(&srtp_be);

        for i in 0..12 {
            iv[i] ^= salt[i];
        }

        iv
    }
}

/// AEAD-AES-256-GCM SRTP profile.
///
/// This struct provides constants, types, and protocol functions for the
/// AEAD-AES-256-GCM SRTP protection profile (RFC 7714).
#[derive(Debug, Clone, Copy)]
pub struct AeadAes256Gcm;

impl AeadAes256Gcm {
    /// Key length in bytes.
    pub const KEY_LEN: usize = 32;
    /// Salt length in bytes.
    pub const SALT_LEN: usize = 12;
    /// RTCP AAD length in bytes.
    pub const RTCP_AAD_LEN: usize = 12;
    /// Authentication tag length in bytes.
    pub const TAG_LEN: usize = 16;
    /// IV length in bytes.
    pub const IV_LEN: usize = 12;

    /// Compute RTP IV.
    pub fn rtp_iv(salt: [u8; 12], ssrc: u32, roc: u32, seq: u16) -> [u8; 12] {
        // See: https://www.rfc-editor.org/rfc/rfc7714#section-8.1
        let mut iv = [0; 12];

        let ssrc_be = ssrc.to_be_bytes();
        let roc_be = roc.to_be_bytes();
        let seq_be = seq.to_be_bytes();

        iv[2..6].copy_from_slice(&ssrc_be);
        iv[6..10].copy_from_slice(&roc_be);
        iv[10..12].copy_from_slice(&seq_be);

        for i in 0..12 {
            iv[i] ^= salt[i];
        }

        iv
    }

    /// Compute RTCP IV.
    pub fn rtcp_iv(salt: [u8; 12], ssrc: u32, srtp_index: u32) -> [u8; 12] {
        // See: https://www.rfc-editor.org/rfc/rfc7714#section-9.1
        let mut iv = [0; 12];

        let ssrc_be = ssrc.to_be_bytes();
        let srtp_be = srtp_index.to_be_bytes();

        iv[2..6].copy_from_slice(&ssrc_be);
        iv[8..12].copy_from_slice(&srtp_be);

        for i in 0..12 {
            iv[i] ^= salt[i];
        }

        iv
    }
}
