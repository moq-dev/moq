//! Low-level DTLS message parsing and serialization types.
//!
//! This module exposes enums and helpers used by the public API for negotiating
//! cipher suites and signature algorithms, as well as parsing wire formats.
//! Only the public items are documented here; the rest are internal helpers.

mod certificate;
mod certificate_request;
mod certificate_verify;
mod client_hello;
mod client_key_exchange;
mod digitally_signed;
mod extension;
mod extensions;
mod finished;
mod handshake;
mod hello_verify;
mod id;
mod named_group;
mod record;
mod server_hello;
mod server_key_exchange;
mod wrapped;

use std::fmt;

use arrayvec::ArrayVec;
pub use certificate::Certificate;
pub use certificate_request::CertificateRequest;
pub use certificate_verify::CertificateVerify;
pub use client_hello::ClientHello;
pub use client_key_exchange::{ClientKeyExchange, ClientPskKeys, ExchangeKeys};
pub use digitally_signed::DigitallySigned;
pub use extension::{Extension, ExtensionType};
pub use extensions::ECPointFormatsExtension;
pub use extensions::SignatureAlgorithmsExtension;
pub use extensions::SupportedGroupsExtension;
pub use extensions::UseSrtpExtension;
pub use extensions::use_srtp::{SrtpProfileId, SrtpProfileVec};
pub use finished::Finished;
pub use handshake::{Body, Handshake, Header, MessageType};
pub use hello_verify::HelloVerifyRequest;
pub use id::{Cookie, SessionId};
pub use named_group::CurveType;
pub use record::DTLSRecord;

// Re-export shared types for backwards compatibility
pub use crate::types::{
    CompressionMethod, ContentType, HashAlgorithm, NamedGroup, NamedGroupVec, ProtocolVersion,
    Random, Sequence, SignatureAlgorithm,
};
pub use server_hello::ServerHello;
pub use server_key_exchange::{PskParams, ServerKeyExchange, ServerKeyExchangeParams};
pub use wrapped::{Asn1Cert, DistinguishedName};

use nom::IResult;
use nom::number::complete::{be_u8, be_u16};

pub type CipherSuiteVec = ArrayVec<Dtls12CipherSuite, { Dtls12CipherSuite::supported().len() }>;

/// Supported TLS 1.2 cipher suites for DTLS.
#[repr(transparent)]
#[derive(Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Dtls12CipherSuite(u16);

impl Dtls12CipherSuite {
    /// ECDHE with ECDSA authentication, AES-256-GCM, SHA-384.
    pub const ECDHE_ECDSA_AES256_GCM_SHA384: Self = Self(0xC02C);
    /// ECDHE with ECDSA authentication, AES-128-GCM, SHA-256.
    pub const ECDHE_ECDSA_AES128_GCM_SHA256: Self = Self(0xC02B);
    /// ECDHE with ECDSA authentication, ChaCha20-Poly1305, SHA-256.
    pub const ECDHE_ECDSA_CHACHA20_POLY1305_SHA256: Self = Self(0xCCA9);
    /// PSK with AES-128-CCM-8 (8-byte tag), SHA-256.
    pub const PSK_AES128_CCM_8: Self = Self(0xC0A8);

    /// Convert the 16-bit IANA value to a `Dtls12CipherSuite`.
    pub const fn from_u16(value: u16) -> Self {
        Self(value)
    }

    /// Return the 16-bit IANA value for this cipher suite.
    pub const fn as_u16(&self) -> u16 {
        self.0
    }

    /// Returns true if this is not a known DTLS 1.2 cipher suite wire value.
    pub const fn is_unknown(&self) -> bool {
        !matches!(*self, Self(0xC02B..=0xC02C | 0xC0A8 | 0xCCA9))
    }

    /// Parse a `Dtls12CipherSuite` from network byte order.
    pub fn parse(input: &[u8]) -> IResult<&[u8], Dtls12CipherSuite> {
        let (input, value) = be_u16(input)?;
        Ok((input, Dtls12CipherSuite::from_u16(value)))
    }

    /// Length in bytes of verify_data for Finished MACs.
    pub fn verify_data_length(&self) -> usize {
        match *self {
            // AES-GCM suites
            Dtls12CipherSuite::ECDHE_ECDSA_AES256_GCM_SHA384
            | Dtls12CipherSuite::ECDHE_ECDSA_AES128_GCM_SHA256
            | Dtls12CipherSuite::ECDHE_ECDSA_CHACHA20_POLY1305_SHA256
            | Dtls12CipherSuite::PSK_AES128_CCM_8 => 12,

            _ => 12, // Default length for unknown cipher suites
        }
    }

    /// The key exchange algorithm family for this cipher suite.
    pub fn as_key_exchange_algorithm(&self) -> KeyExchangeAlgorithm {
        match *self {
            // All ECDHE ciphers
            Dtls12CipherSuite::ECDHE_ECDSA_AES256_GCM_SHA384
            | Dtls12CipherSuite::ECDHE_ECDSA_AES128_GCM_SHA256
            | Dtls12CipherSuite::ECDHE_ECDSA_CHACHA20_POLY1305_SHA256 => {
                KeyExchangeAlgorithm::EECDH
            }

            Dtls12CipherSuite::PSK_AES128_CCM_8 => KeyExchangeAlgorithm::PSK,

            _ => KeyExchangeAlgorithm::Unknown,
        }
    }

    /// Whether this cipher suite uses ECC-based key exchange.
    pub fn has_ecc(&self) -> bool {
        matches!(
            *self,
            Dtls12CipherSuite::ECDHE_ECDSA_AES256_GCM_SHA384
                | Dtls12CipherSuite::ECDHE_ECDSA_AES128_GCM_SHA256
                | Dtls12CipherSuite::ECDHE_ECDSA_CHACHA20_POLY1305_SHA256
        )
    }

    /// Whether this cipher suite uses PSK (Pre-Shared Key) key exchange.
    pub fn is_psk(&self) -> bool {
        matches!(*self, Dtls12CipherSuite::PSK_AES128_CCM_8)
    }

    /// All supported cipher suites in server preference order.
    pub const fn all() -> &'static [Dtls12CipherSuite] {
        &[
            Dtls12CipherSuite::ECDHE_ECDSA_AES256_GCM_SHA384,
            Dtls12CipherSuite::ECDHE_ECDSA_AES128_GCM_SHA256,
            Dtls12CipherSuite::ECDHE_ECDSA_CHACHA20_POLY1305_SHA256,
            Dtls12CipherSuite::PSK_AES128_CCM_8,
        ]
    }

    /// Cipher suites compatible with a given certificate's signature algorithm.
    pub fn compatible_with_certificate(
        cert_type: SignatureAlgorithm,
    ) -> &'static [Dtls12CipherSuite] {
        match cert_type {
            SignatureAlgorithm::ECDSA => &[
                Dtls12CipherSuite::ECDHE_ECDSA_AES256_GCM_SHA384,
                Dtls12CipherSuite::ECDHE_ECDSA_AES128_GCM_SHA256,
                Dtls12CipherSuite::ECDHE_ECDSA_CHACHA20_POLY1305_SHA256,
            ],
            _ => panic!("Need either RSA or ECDSA certificate"),
        }
    }

    fn need_encrypt_then_mac(&self) -> bool {
        // We do not support and ciphers such as:
        // ECDHE-RSA-AES128-SHA
        // ECDHE-RSA-AES256-SHA
        // DHE-RSA-AES128-SHA256
        false
    }

    /// The hash algorithm used by this cipher suite.
    pub fn hash_algorithm(&self) -> HashAlgorithm {
        match *self {
            Dtls12CipherSuite::ECDHE_ECDSA_AES256_GCM_SHA384 => HashAlgorithm::SHA384,
            Dtls12CipherSuite::ECDHE_ECDSA_AES128_GCM_SHA256
            | Dtls12CipherSuite::ECDHE_ECDSA_CHACHA20_POLY1305_SHA256
            | Dtls12CipherSuite::PSK_AES128_CCM_8 => HashAlgorithm::SHA256,
            _ => HashAlgorithm::UNKNOWN_DERIVED,
        }
    }

    /// The signature algorithm associated with the suite's key exchange.
    ///
    /// Returns `None` for PSK cipher suites (no signature authentication).
    pub fn signature_algorithm(&self) -> Option<SignatureAlgorithm> {
        match *self {
            Dtls12CipherSuite::ECDHE_ECDSA_AES256_GCM_SHA384
            | Dtls12CipherSuite::ECDHE_ECDSA_AES128_GCM_SHA256
            | Dtls12CipherSuite::ECDHE_ECDSA_CHACHA20_POLY1305_SHA256 => {
                Some(SignatureAlgorithm::ECDSA)
            }
            Dtls12CipherSuite::PSK_AES128_CCM_8 => None,
            _ => Some(SignatureAlgorithm::UNKNOWN_DERIVED),
        }
    }

    /// Returns true if this cipher suite is supported by this implementation.
    pub fn is_supported(&self) -> bool {
        Self::supported().contains(self)
    }

    /// Supported DTLS 1.2 cipher suites in server preference order.
    pub const fn supported() -> &'static [Dtls12CipherSuite] {
        Self::all()
    }
}

impl fmt::Debug for Dtls12CipherSuite {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Dtls12CipherSuite::ECDHE_ECDSA_AES256_GCM_SHA384 => {
                f.write_str("ECDHE_ECDSA_AES256_GCM_SHA384")
            }
            Dtls12CipherSuite::ECDHE_ECDSA_AES128_GCM_SHA256 => {
                f.write_str("ECDHE_ECDSA_AES128_GCM_SHA256")
            }
            Dtls12CipherSuite::ECDHE_ECDSA_CHACHA20_POLY1305_SHA256 => {
                f.write_str("ECDHE_ECDSA_CHACHA20_POLY1305_SHA256")
            }
            Dtls12CipherSuite::PSK_AES128_CCM_8 => f.write_str("PSK_AES128_CCM_8"),
            _ => f.debug_tuple("Unknown").field(&self.0).finish(),
        }
    }
}

pub type CompressionMethodVec =
    ArrayVec<CompressionMethod, { CompressionMethod::supported().len() }>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(non_camel_case_types)]
#[allow(clippy::upper_case_acronyms)]
pub enum KeyExchangeAlgorithm {
    EECDH,
    PSK,
    Unknown,
}

pub type CertificateTypeVec =
    ArrayVec<ClientCertificateType, { ClientCertificateType::supported().len() }>;

#[repr(transparent)]
#[derive(Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct ClientCertificateType(u8);

impl ClientCertificateType {
    pub const RSA_SIGN: Self = Self(1);
    pub const DSS_SIGN: Self = Self(2);
    pub const RSA_FIXED_DH: Self = Self(3);
    pub const DSS_FIXED_DH: Self = Self(4);
    pub const RSA_EPHEMERAL_DH: Self = Self(5);
    pub const DSS_EPHEMERAL_DH: Self = Self(6);
    pub const FORTEZZA_DMS: Self = Self(20);
    pub const ECDSA_SIGN: Self = Self(64);

    pub const fn from_u8(value: u8) -> Self {
        Self(value)
    }

    /// Returns true if this certificate type is supported by this implementation.
    /// Currently only ECDSA_SIGN is supported.
    pub fn is_supported(&self) -> bool {
        Self::supported().contains(self)
    }

    /// Supported client certificate types.
    pub const fn supported() -> &'static [ClientCertificateType; 1] {
        &[ClientCertificateType::ECDSA_SIGN]
    }

    pub const fn as_u8(&self) -> u8 {
        self.0
    }

    const fn is_unknown(&self) -> bool {
        !matches!(*self, Self(1..=6 | 20 | 64))
    }

    pub fn parse(input: &[u8]) -> IResult<&[u8], ClientCertificateType> {
        let (input, value) = be_u8(input)?;
        Ok((input, ClientCertificateType::from_u8(value)))
    }
}

impl fmt::Debug for ClientCertificateType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_unknown() {
            return f.debug_tuple("Unknown").field(&self.0).finish();
        }

        let name = match *self {
            ClientCertificateType::RSA_SIGN => "RSA_SIGN",
            ClientCertificateType::DSS_SIGN => "DSS_SIGN",
            ClientCertificateType::RSA_FIXED_DH => "RSA_FIXED_DH",
            ClientCertificateType::DSS_FIXED_DH => "DSS_FIXED_DH",
            ClientCertificateType::RSA_EPHEMERAL_DH => "RSA_EPHEMERAL_DH",
            ClientCertificateType::DSS_EPHEMERAL_DH => "DSS_EPHEMERAL_DH",
            ClientCertificateType::FORTEZZA_DMS => "FORTEZZA_DMS",
            ClientCertificateType::ECDSA_SIGN => "ECDSA_SIGN",
            _ => unreachable!("known DTLS 1.2 client certificate type missing Debug label"),
        };

        f.write_str(name)
    }
}

// SignatureAlgorithm and HashAlgorithm are now in crate::types

pub type SignatureAndHashAlgorithmVec =
    ArrayVec<SignatureAndHashAlgorithm, { SignatureAndHashAlgorithm::supported().len() }>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SignatureAndHashAlgorithm {
    pub hash: HashAlgorithm,
    pub signature: SignatureAlgorithm,
}

impl SignatureAndHashAlgorithm {
    pub const fn new(hash: HashAlgorithm, signature: SignatureAlgorithm) -> Self {
        SignatureAndHashAlgorithm { hash, signature }
    }

    pub fn from_u16(value: u16) -> Self {
        let hash = HashAlgorithm::from_u8((value >> 8) as u8);
        let signature = SignatureAlgorithm::from_u8(value as u8);
        SignatureAndHashAlgorithm { hash, signature }
    }

    pub fn as_u16(&self) -> u16 {
        ((self.hash.as_u8() as u16) << 8) | (self.signature.as_u8() as u16)
    }

    pub fn parse(input: &[u8]) -> IResult<&[u8], SignatureAndHashAlgorithm> {
        let (input, value) = be_u16(input)?;
        Ok((input, SignatureAndHashAlgorithm::from_u16(value)))
    }

    /// All recognized signature+hash combinations.
    #[allow(dead_code)]
    pub const fn all() -> &'static [SignatureAndHashAlgorithm; 4] {
        const ALL: &[SignatureAndHashAlgorithm; 4] = &[
            SignatureAndHashAlgorithm::new(HashAlgorithm::SHA256, SignatureAlgorithm::ECDSA),
            SignatureAndHashAlgorithm::new(HashAlgorithm::SHA384, SignatureAlgorithm::ECDSA),
            SignatureAndHashAlgorithm::new(HashAlgorithm::SHA256, SignatureAlgorithm::RSA),
            SignatureAndHashAlgorithm::new(HashAlgorithm::SHA384, SignatureAlgorithm::RSA),
        ];

        ALL
    }

    /// Supported signature+hash combinations.
    pub const fn supported() -> &'static [SignatureAndHashAlgorithm; 2] {
        const SUPPORTED: &[SignatureAndHashAlgorithm; 2] = &[
            SignatureAndHashAlgorithm::new(HashAlgorithm::SHA256, SignatureAlgorithm::ECDSA),
            SignatureAndHashAlgorithm::new(HashAlgorithm::SHA384, SignatureAlgorithm::ECDSA),
        ];

        SUPPORTED
    }

    /// Returns true if this signature+hash combination is supported.
    pub fn is_supported(&self) -> bool {
        Self::supported().contains(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rsa_signature_algorithms_are_recognized_but_not_supported() {
        let rsa_sha256 =
            SignatureAndHashAlgorithm::new(HashAlgorithm::SHA256, SignatureAlgorithm::RSA);

        assert!(SignatureAndHashAlgorithm::all().contains(&rsa_sha256));
        assert!(!rsa_sha256.is_supported());
        assert_eq!(SignatureAndHashAlgorithm::supported().len(), 2);
    }

    #[test]
    fn dtls12_cipher_suite_newtype_shape() {
        assert_eq!(std::mem::size_of::<Dtls12CipherSuite>(), 2);
        assert_eq!(Dtls12CipherSuite::default().as_u16(), 0);
        assert!(Dtls12CipherSuite::default().is_unknown());
    }

    #[test]
    fn dtls12_cipher_suite_wire_roundtrip() {
        for suite in Dtls12CipherSuite::all() {
            assert_eq!(Dtls12CipherSuite::from_u16(suite.as_u16()), *suite);
            assert!(!suite.is_unknown());
        }

        let unknown = Dtls12CipherSuite::from_u16(0xFFFF);
        assert_eq!(unknown.as_u16(), 0xFFFF);
        assert!(unknown.is_unknown());
    }

    #[test]
    fn dtls12_cipher_suite_debug_stays_enum_like() {
        assert_eq!(
            format!("{:?}", Dtls12CipherSuite::ECDHE_ECDSA_AES128_GCM_SHA256),
            "ECDHE_ECDSA_AES128_GCM_SHA256"
        );
        assert_eq!(
            format!("{:?}", Dtls12CipherSuite::from_u16(0xFFFF)),
            "Unknown(65535)"
        );
    }

    #[test]
    fn client_certificate_type_newtype_shape() {
        assert_eq!(std::mem::size_of::<ClientCertificateType>(), 1);
        assert_eq!(ClientCertificateType::default().as_u8(), 0);
        assert!(ClientCertificateType::default().is_unknown());
    }

    #[test]
    fn client_certificate_type_wire_roundtrip() {
        for certificate_type in [
            ClientCertificateType::RSA_SIGN,
            ClientCertificateType::DSS_SIGN,
            ClientCertificateType::RSA_FIXED_DH,
            ClientCertificateType::DSS_FIXED_DH,
            ClientCertificateType::RSA_EPHEMERAL_DH,
            ClientCertificateType::DSS_EPHEMERAL_DH,
            ClientCertificateType::FORTEZZA_DMS,
            ClientCertificateType::ECDSA_SIGN,
        ] {
            assert_eq!(
                ClientCertificateType::from_u8(certificate_type.as_u8()),
                certificate_type
            );
            assert!(!certificate_type.is_unknown());
        }

        let unknown = ClientCertificateType::from_u8(0xFF);
        assert_eq!(unknown.as_u8(), 0xFF);
        assert!(unknown.is_unknown());
    }

    #[test]
    fn client_certificate_type_debug_stays_enum_like() {
        assert_eq!(
            format!("{:?}", ClientCertificateType::ECDSA_SIGN),
            "ECDSA_SIGN"
        );
        assert_eq!(
            format!("{:?}", ClientCertificateType::from_u8(0xFF)),
            "Unknown(255)"
        );
    }

    #[test]
    fn unknown_dtls12_cipher_suite_uses_internal_derived_markers() {
        let unknown = Dtls12CipherSuite::from_u16(0xFFFF);
        assert!(unknown.hash_algorithm().is_unknown());
        assert!(
            unknown
                .signature_algorithm()
                .is_some_and(|s| s.is_unknown())
        );
    }
}
