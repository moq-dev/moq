//! Shared types used by both DTLS 1.2 and DTLS 1.3.
//!
//! These types represent cryptographic primitives and protocol elements
//! that are common across DTLS versions.

use std::cmp::Ordering;
use std::fmt;
use std::time::Instant;

use arrayvec::ArrayVec;
use nom::IResult;
use nom::bytes::complete::take;
use nom::number::complete::{be_u8, be_u16};

use crate::SeededRng;
use crate::buffer::Buf;
use crate::time_tricks::InstantExt;

pub type NamedGroupVec = ArrayVec<NamedGroup, { NamedGroup::supported().len() }>;

// ============================================================================
// Random
// ============================================================================

/// ClientHello / ServerHello random value (32 bytes on the wire).
///
/// Used by both DTLS 1.2 and DTLS 1.3. Construction differs:
/// - DTLS 1.2: first 4 bytes are `gmt_unix_time` ([`Random::new_with_time`]).
/// - DTLS 1.3: all 32 bytes are random ([`Random::new`]).
///
/// After construction neither version accesses sub-fields — all consumers
/// use [`bytes`](Self::bytes) or [`serialize`](Self::serialize).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Random {
    /// The 32 raw bytes of the random value.
    pub bytes: [u8; 32],
}

impl Random {
    /// All-random (DTLS 1.3 / hybrid style).
    pub fn new(rng: &mut SeededRng) -> Self {
        Self {
            bytes: rng.random(),
        }
    }

    /// Timestamp in first 4 bytes (DTLS 1.2 style).
    pub fn new_with_time(now: Instant, rng: &mut SeededRng) -> Self {
        let gmt_duration = now.to_unix_duration();
        // This is valid until year 2106, at which point I will be beyond caring.
        let gmt_unix_time = gmt_duration.as_secs() as u32;

        let random_bytes: [u8; 28] = rng.random();

        let mut bytes = [0u8; 32];
        bytes[..4].copy_from_slice(&gmt_unix_time.to_be_bytes());
        bytes[4..].copy_from_slice(&random_bytes);

        Self { bytes }
    }

    /// Parse a 32-byte `Random` from wire format.
    pub fn parse(input: &[u8]) -> IResult<&[u8], Random> {
        let (input, data) = take(32_usize)(input)?;
        let mut bytes = [0u8; 32];
        bytes.copy_from_slice(data);
        Ok((input, Random { bytes }))
    }

    /// Serialize this `Random` to wire format.
    pub fn serialize(&self, output: &mut Buf) {
        output.extend_from_slice(&self.bytes);
    }
}

// ============================================================================
// Named Groups (Key Exchange)
// ============================================================================

/// Elliptic curves and key exchange groups (RFC 8422, RFC 8446).
///
/// Used for Elliptic Curve Diffie-Hellman Ephemeral (ECDHE) key exchange.
/// The same named groups are used in both DTLS 1.2 and DTLS 1.3.
#[repr(transparent)]
#[derive(Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct NamedGroup(u16);

#[allow(non_upper_case_globals)]
impl NamedGroup {
    /// sect163k1 (deprecated).
    pub const Sect163k1: Self = Self(1);
    /// sect163r1 (deprecated).
    pub const Sect163r1: Self = Self(2);
    /// sect163r2 (deprecated).
    pub const Sect163r2: Self = Self(3);
    /// sect193r1 (deprecated).
    pub const Sect193r1: Self = Self(4);
    /// sect193r2 (deprecated).
    pub const Sect193r2: Self = Self(5);
    /// sect233k1 (deprecated).
    pub const Sect233k1: Self = Self(6);
    /// sect233r1 (deprecated).
    pub const Sect233r1: Self = Self(7);
    /// sect239k1 (deprecated).
    pub const Sect239k1: Self = Self(8);
    /// sect283k1 (deprecated).
    pub const Sect283k1: Self = Self(9);
    /// sect283r1 (deprecated).
    pub const Sect283r1: Self = Self(10);
    /// sect409k1 (deprecated).
    pub const Sect409k1: Self = Self(11);
    /// sect409r1 (deprecated).
    pub const Sect409r1: Self = Self(12);
    /// sect571k1 (deprecated).
    pub const Sect571k1: Self = Self(13);
    /// sect571r1 (deprecated).
    pub const Sect571r1: Self = Self(14);
    /// secp160k1 (deprecated).
    pub const Secp160k1: Self = Self(15);
    /// secp160r1 (deprecated).
    pub const Secp160r1: Self = Self(16);
    /// secp160r2 (deprecated).
    pub const Secp160r2: Self = Self(17);
    /// secp192k1 (deprecated).
    pub const Secp192k1: Self = Self(18);
    /// secp192r1 (deprecated).
    pub const Secp192r1: Self = Self(19);
    /// secp224k1.
    pub const Secp224k1: Self = Self(20);
    /// secp224r1.
    pub const Secp224r1: Self = Self(21);
    /// secp256k1.
    pub const Secp256k1: Self = Self(22);
    /// secp256r1 / P-256 (supported by dimpl).
    pub const Secp256r1: Self = Self(23);
    /// secp384r1 / P-384 (supported by dimpl).
    pub const Secp384r1: Self = Self(24);
    /// secp521r1 / P-521.
    pub const Secp521r1: Self = Self(25);
    /// X25519 (Curve25519 for ECDHE).
    pub const X25519: Self = Self(29);
    /// X448 (Curve448 for ECDHE).
    pub const X448: Self = Self(30);

    /// Convert a wire format u16 value to a `NamedGroup`.
    pub const fn from_u16(value: u16) -> Self {
        Self(value)
    }

    /// Convert this `NamedGroup` to its wire format u16 value.
    pub const fn as_u16(&self) -> u16 {
        self.0
    }

    /// Returns true if this is not a known TLS named group wire value.
    pub const fn is_unknown(&self) -> bool {
        !matches!(*self, Self(1..=25 | 29..=30))
    }

    /// Parse a `NamedGroup` from wire format.
    pub fn parse(input: &[u8]) -> IResult<&[u8], NamedGroup> {
        let (input, value) = be_u16(input)?;
        Ok((input, NamedGroup::from_u16(value)))
    }

    /// Returns true if this named group is supported by this implementation.
    pub fn is_supported(&self) -> bool {
        Self::supported().contains(self)
    }

    /// All recognized named groups (every non-`Unknown` variant).
    pub const fn all() -> &'static [NamedGroup; 27] {
        &[
            NamedGroup::Sect163k1,
            NamedGroup::Sect163r1,
            NamedGroup::Sect163r2,
            NamedGroup::Sect193r1,
            NamedGroup::Sect193r2,
            NamedGroup::Sect233k1,
            NamedGroup::Sect233r1,
            NamedGroup::Sect239k1,
            NamedGroup::Sect283k1,
            NamedGroup::Sect283r1,
            NamedGroup::Sect409k1,
            NamedGroup::Sect409r1,
            NamedGroup::Sect571k1,
            NamedGroup::Sect571r1,
            NamedGroup::Secp160k1,
            NamedGroup::Secp160r1,
            NamedGroup::Secp160r2,
            NamedGroup::Secp192k1,
            NamedGroup::Secp192r1,
            NamedGroup::Secp224k1,
            NamedGroup::Secp224r1,
            NamedGroup::Secp256k1,
            NamedGroup::Secp256r1,
            NamedGroup::Secp384r1,
            NamedGroup::Secp521r1,
            NamedGroup::X25519,
            NamedGroup::X448,
        ]
    }

    /// Supported named groups in preference order.
    pub const fn supported() -> &'static [NamedGroup; 4] {
        &[
            NamedGroup::X25519,
            NamedGroup::Secp256r1,
            NamedGroup::Secp384r1,
            NamedGroup::Secp521r1,
        ]
    }
}

impl fmt::Debug for NamedGroup {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            NamedGroup::Sect163k1 => f.write_str("Sect163k1"),
            NamedGroup::Sect163r1 => f.write_str("Sect163r1"),
            NamedGroup::Sect163r2 => f.write_str("Sect163r2"),
            NamedGroup::Sect193r1 => f.write_str("Sect193r1"),
            NamedGroup::Sect193r2 => f.write_str("Sect193r2"),
            NamedGroup::Sect233k1 => f.write_str("Sect233k1"),
            NamedGroup::Sect233r1 => f.write_str("Sect233r1"),
            NamedGroup::Sect239k1 => f.write_str("Sect239k1"),
            NamedGroup::Sect283k1 => f.write_str("Sect283k1"),
            NamedGroup::Sect283r1 => f.write_str("Sect283r1"),
            NamedGroup::Sect409k1 => f.write_str("Sect409k1"),
            NamedGroup::Sect409r1 => f.write_str("Sect409r1"),
            NamedGroup::Sect571k1 => f.write_str("Sect571k1"),
            NamedGroup::Sect571r1 => f.write_str("Sect571r1"),
            NamedGroup::Secp160k1 => f.write_str("Secp160k1"),
            NamedGroup::Secp160r1 => f.write_str("Secp160r1"),
            NamedGroup::Secp160r2 => f.write_str("Secp160r2"),
            NamedGroup::Secp192k1 => f.write_str("Secp192k1"),
            NamedGroup::Secp192r1 => f.write_str("Secp192r1"),
            NamedGroup::Secp224k1 => f.write_str("Secp224k1"),
            NamedGroup::Secp224r1 => f.write_str("Secp224r1"),
            NamedGroup::Secp256k1 => f.write_str("Secp256k1"),
            NamedGroup::Secp256r1 => f.write_str("Secp256r1"),
            NamedGroup::Secp384r1 => f.write_str("Secp384r1"),
            NamedGroup::Secp521r1 => f.write_str("Secp521r1"),
            NamedGroup::X25519 => f.write_str("X25519"),
            NamedGroup::X448 => f.write_str("X448"),
            _ => f.debug_tuple("Unknown").field(&self.0).finish(),
        }
    }
}

// ============================================================================
// Hash Algorithms
// ============================================================================

/// Hash algorithms used in DTLS (RFC 5246, RFC 8446).
///
/// Specifies the hash algorithm to be used in digital signatures,
/// PRF/HKDF operations, and transcript hashing.
#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct HashAlgorithm(u8);

impl Default for HashAlgorithm {
    fn default() -> Self {
        Self::None
    }
}

#[allow(non_upper_case_globals)]
impl HashAlgorithm {
    /// No hash (not typically used).
    pub const None: Self = Self(0);
    /// MD5 hash (deprecated, not supported).
    pub const MD5: Self = Self(1);
    /// SHA-1 hash (deprecated, not supported).
    pub const SHA1: Self = Self(2);
    /// SHA-224 hash.
    pub const SHA224: Self = Self(3);
    /// SHA-256 hash (supported by dimpl).
    pub const SHA256: Self = Self(4);
    /// SHA-384 hash (supported by dimpl).
    pub const SHA384: Self = Self(5);
    /// SHA-512 hash.
    pub const SHA512: Self = Self(6);

    pub(crate) const UNKNOWN_DERIVED: Self = Self(u8::MAX);

    /// Convert a wire format u8 value to a `HashAlgorithm`.
    pub const fn from_u8(value: u8) -> Self {
        Self(value)
    }

    /// Convert this `HashAlgorithm` to its wire format u8 value.
    pub const fn as_u8(&self) -> u8 {
        self.0
    }

    /// Returns true if this is not a known DTLS hash algorithm wire value.
    pub const fn is_unknown(&self) -> bool {
        self.0 > Self::SHA512.0
    }

    /// Parse a `HashAlgorithm` from wire format.
    pub fn parse(input: &[u8]) -> IResult<&[u8], HashAlgorithm> {
        let (input, value) = be_u8(input)?;
        Ok((input, HashAlgorithm::from_u8(value)))
    }

    /// Returns the output length in bytes for this hash algorithm.
    pub const fn output_len(&self) -> usize {
        match *self {
            HashAlgorithm::None => 0,
            HashAlgorithm::MD5 => 16,
            HashAlgorithm::SHA1 => 20,
            HashAlgorithm::SHA224 => 28,
            HashAlgorithm::SHA256 => 32,
            HashAlgorithm::SHA384 => 48,
            HashAlgorithm::SHA512 => 64,
            _ => 0,
        }
    }
}

impl fmt::Debug for HashAlgorithm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            HashAlgorithm::None => f.write_str("None"),
            HashAlgorithm::MD5 => f.write_str("MD5"),
            HashAlgorithm::SHA1 => f.write_str("SHA1"),
            HashAlgorithm::SHA224 => f.write_str("SHA224"),
            HashAlgorithm::SHA256 => f.write_str("SHA256"),
            HashAlgorithm::SHA384 => f.write_str("SHA384"),
            HashAlgorithm::SHA512 => f.write_str("SHA512"),
            _ => f.debug_tuple("Unknown").field(&self.0).finish(),
        }
    }
}

// ============================================================================
// Signature Algorithms
// ============================================================================

/// Signature algorithms used in DTLS handshakes.
///
/// Represents the underlying signature primitive (RSA, ECDSA, etc.).
/// Used internally for signing operations across both DTLS versions.
#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct SignatureAlgorithm(u8);

impl Default for SignatureAlgorithm {
    fn default() -> Self {
        Self::Anonymous
    }
}

#[allow(non_upper_case_globals)]
impl SignatureAlgorithm {
    /// Anonymous (no certificate).
    pub const Anonymous: Self = Self(0);
    /// RSA signatures.
    pub const RSA: Self = Self(1);
    /// DSA signatures.
    pub const DSA: Self = Self(2);
    /// ECDSA signatures.
    pub const ECDSA: Self = Self(3);

    pub(crate) const UNKNOWN_DERIVED: Self = Self(u8::MAX);

    /// Convert an 8-bit value into a `SignatureAlgorithm`.
    pub const fn from_u8(value: u8) -> Self {
        Self(value)
    }

    /// Convert this `SignatureAlgorithm` into its 8-bit representation.
    pub const fn as_u8(&self) -> u8 {
        self.0
    }

    /// Returns true if this is not a known DTLS signature algorithm wire value.
    pub const fn is_unknown(&self) -> bool {
        self.0 > Self::ECDSA.0
    }

    /// Parse a `SignatureAlgorithm` from network bytes.
    pub fn parse(input: &[u8]) -> IResult<&[u8], SignatureAlgorithm> {
        let (input, value) = be_u8(input)?;
        Ok((input, SignatureAlgorithm::from_u8(value)))
    }
}

impl fmt::Debug for SignatureAlgorithm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            SignatureAlgorithm::Anonymous => f.write_str("Anonymous"),
            SignatureAlgorithm::RSA => f.write_str("RSA"),
            SignatureAlgorithm::DSA => f.write_str("DSA"),
            SignatureAlgorithm::ECDSA => f.write_str("ECDSA"),
            _ => f.debug_tuple("Unknown").field(&self.0).finish(),
        }
    }
}

// ============================================================================
// Content Type
// ============================================================================

/// DTLS record content types.
///
/// Identifies the type of data in a DTLS record. These values are the same
/// for both DTLS 1.2 and DTLS 1.3.
#[repr(transparent)]
#[derive(Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct ContentType(u8);

#[allow(non_upper_case_globals)]
impl ContentType {
    /// Change Cipher Spec (used in DTLS 1.2, compatibility-only in 1.3).
    pub const ChangeCipherSpec: Self = Self(20);
    /// Alert message.
    pub const Alert: Self = Self(21);
    /// Handshake message.
    pub const Handshake: Self = Self(22);
    /// Application data.
    pub const ApplicationData: Self = Self(23);
    /// ACK (DTLS 1.3 only, RFC 9147 Section 7).
    pub const Ack: Self = Self(26);

    /// Convert a u8 value to a `ContentType`.
    pub const fn from_u8(value: u8) -> Self {
        Self(value)
    }

    /// Convert this `ContentType` to its u8 value.
    pub const fn as_u8(&self) -> u8 {
        self.0
    }

    /// Returns true if this is not a known DTLS record content type.
    pub const fn is_unknown(&self) -> bool {
        !matches!(*self, Self(20..=23 | 26))
    }

    /// Parse a `ContentType` from wire format.
    pub fn parse(input: &[u8]) -> IResult<&[u8], ContentType> {
        let (input, byte) = be_u8(input)?;
        Ok((input, Self::from_u8(byte)))
    }
}

impl fmt::Debug for ContentType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            ContentType::ChangeCipherSpec => f.write_str("ChangeCipherSpec"),
            ContentType::Alert => f.write_str("Alert"),
            ContentType::Handshake => f.write_str("Handshake"),
            ContentType::ApplicationData => f.write_str("ApplicationData"),
            ContentType::Ack => f.write_str("Ack"),
            _ => f.debug_tuple("Unknown").field(&self.0).finish(),
        }
    }
}

// ============================================================================
// Sequence Number
// ============================================================================

/// DTLS record sequence number (epoch + sequence).
///
/// Both DTLS 1.2 and DTLS 1.3 use an epoch and sequence number for
/// replay protection and AEAD nonce construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Sequence {
    /// The epoch (incremented on key change).
    pub epoch: u16,
    /// The sequence number within the epoch (technically u48).
    pub sequence_number: u64,
}

impl Sequence {
    /// Create a new sequence with the given epoch and sequence number 0.
    pub fn new(epoch: u16) -> Self {
        Self {
            epoch,
            sequence_number: 0,
        }
    }
}

impl fmt::Display for Sequence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "[epoch: {}, sequence_number: {}]",
            self.epoch, self.sequence_number,
        )
    }
}

impl Ord for Sequence {
    fn cmp(&self, other: &Self) -> Ordering {
        if self.epoch < other.epoch {
            Ordering::Less
        } else if self.epoch > other.epoch {
            Ordering::Greater
        } else {
            self.sequence_number.cmp(&other.sequence_number)
        }
    }
}

impl PartialOrd for Sequence {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

// ============================================================================
// Signature Schemes (TLS 1.3)
// ============================================================================

/// Signature schemes used in TLS 1.3/DTLS 1.3 (RFC 8446).
///
/// In TLS 1.3, signature schemes combine the signature algorithm with the
/// hash algorithm into a single identifier, unlike TLS 1.2 where they were
/// separate.
#[repr(transparent)]
#[derive(Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct SignatureScheme(u16);

impl SignatureScheme {
    /// ECDSA with P-256 and SHA-256.
    pub const ECDSA_SECP256R1_SHA256: Self = Self(0x0403);
    /// ECDSA with P-384 and SHA-384.
    pub const ECDSA_SECP384R1_SHA384: Self = Self(0x0503);
    /// ECDSA with P-521 and SHA-512.
    pub const ECDSA_SECP521R1_SHA512: Self = Self(0x0603);
    /// Ed25519.
    pub const ED25519: Self = Self(0x0807);
    /// Ed448.
    pub const ED448: Self = Self(0x0808);
    /// RSA-PSS with SHA-256 (rsaEncryption OID).
    pub const RSA_PSS_RSAE_SHA256: Self = Self(0x0804);
    /// RSA-PSS with SHA-384 (rsaEncryption OID).
    pub const RSA_PSS_RSAE_SHA384: Self = Self(0x0805);
    /// RSA-PSS with SHA-512 (rsaEncryption OID).
    pub const RSA_PSS_RSAE_SHA512: Self = Self(0x0806);
    /// RSA-PSS with SHA-256 (id-rsassa-pss OID).
    pub const RSA_PSS_PSS_SHA256: Self = Self(0x0809);
    /// RSA-PSS with SHA-384 (id-rsassa-pss OID).
    pub const RSA_PSS_PSS_SHA384: Self = Self(0x080a);
    /// RSA-PSS with SHA-512 (id-rsassa-pss OID).
    pub const RSA_PSS_PSS_SHA512: Self = Self(0x080b);
    /// RSA PKCS#1 v1.5 with SHA-256 (legacy).
    pub const RSA_PKCS1_SHA256: Self = Self(0x0401);
    /// RSA PKCS#1 v1.5 with SHA-384 (legacy).
    pub const RSA_PKCS1_SHA384: Self = Self(0x0501);
    /// RSA PKCS#1 v1.5 with SHA-512 (legacy).
    pub const RSA_PKCS1_SHA512: Self = Self(0x0601);

    /// Convert a wire format u16 value to a `SignatureScheme`.
    pub const fn from_u16(value: u16) -> Self {
        Self(value)
    }

    /// Convert this `SignatureScheme` to its wire format u16 value.
    pub const fn as_u16(&self) -> u16 {
        self.0
    }

    /// Returns true if this is not a known TLS signature scheme wire value.
    pub const fn is_unknown(&self) -> bool {
        !matches!(
            *self,
            Self(0x0401 | 0x0403 | 0x0501 | 0x0503 | 0x0601 | 0x0603 | 0x0804..=0x080b)
        )
    }

    /// Parse a `SignatureScheme` from wire format.
    pub fn parse(input: &[u8]) -> IResult<&[u8], SignatureScheme> {
        let (input, value) = be_u16(input)?;
        Ok((input, SignatureScheme::from_u16(value)))
    }

    /// Returns true if this signature scheme is supported by this implementation.
    pub fn is_supported(&self) -> bool {
        Self::SUPPORTED.contains(self)
    }

    /// All recognized signature schemes (every non-`Unknown` variant).
    pub const fn all() -> &'static [SignatureScheme] {
        &[
            SignatureScheme::ECDSA_SECP256R1_SHA256,
            SignatureScheme::ECDSA_SECP384R1_SHA384,
            SignatureScheme::ECDSA_SECP521R1_SHA512,
            SignatureScheme::ED25519,
            SignatureScheme::ED448,
            SignatureScheme::RSA_PSS_RSAE_SHA256,
            SignatureScheme::RSA_PSS_RSAE_SHA384,
            SignatureScheme::RSA_PSS_RSAE_SHA512,
            SignatureScheme::RSA_PSS_PSS_SHA256,
            SignatureScheme::RSA_PSS_PSS_SHA384,
            SignatureScheme::RSA_PSS_PSS_SHA512,
            SignatureScheme::RSA_PKCS1_SHA256,
            SignatureScheme::RSA_PKCS1_SHA384,
            SignatureScheme::RSA_PKCS1_SHA512,
        ]
    }

    const SUPPORTED: &[SignatureScheme] = &[
        SignatureScheme::ECDSA_SECP256R1_SHA256,
        SignatureScheme::ECDSA_SECP384R1_SHA384,
    ];

    /// Supported signature schemes in preference order.
    pub fn supported() -> ArrayVec<SignatureScheme, 2> {
        let mut schemes = ArrayVec::new();
        schemes.push(SignatureScheme::ECDSA_SECP256R1_SHA256);
        schemes.push(SignatureScheme::ECDSA_SECP384R1_SHA384);
        schemes
    }

    /// Returns the named group (EC curve) implied by this signature scheme, if any.
    ///
    /// In DTLS 1.3, ECDSA signature schemes encode the expected curve.
    /// Returns `None` for non-ECDSA schemes.
    pub fn named_group(&self) -> Option<NamedGroup> {
        match *self {
            SignatureScheme::ECDSA_SECP256R1_SHA256 => Some(NamedGroup::Secp256r1),
            SignatureScheme::ECDSA_SECP384R1_SHA384 => Some(NamedGroup::Secp384r1),
            _ => None,
        }
    }

    /// Returns the hash algorithm associated with this signature scheme.
    pub fn hash_algorithm(&self) -> HashAlgorithm {
        match *self {
            SignatureScheme::ECDSA_SECP256R1_SHA256
            | SignatureScheme::RSA_PSS_RSAE_SHA256
            | SignatureScheme::RSA_PSS_PSS_SHA256
            | SignatureScheme::RSA_PKCS1_SHA256 => HashAlgorithm::SHA256,
            SignatureScheme::ECDSA_SECP384R1_SHA384
            | SignatureScheme::RSA_PSS_RSAE_SHA384
            | SignatureScheme::RSA_PSS_PSS_SHA384
            | SignatureScheme::RSA_PKCS1_SHA384 => HashAlgorithm::SHA384,
            SignatureScheme::ECDSA_SECP521R1_SHA512
            | SignatureScheme::RSA_PSS_RSAE_SHA512
            | SignatureScheme::RSA_PSS_PSS_SHA512
            | SignatureScheme::RSA_PKCS1_SHA512 => HashAlgorithm::SHA512,
            // Ed25519 and Ed448 have intrinsic hash algorithms
            SignatureScheme::ED25519 | SignatureScheme::ED448 => HashAlgorithm::None,
            _ => HashAlgorithm::UNKNOWN_DERIVED,
        }
    }
}

impl fmt::Debug for SignatureScheme {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            SignatureScheme::ECDSA_SECP256R1_SHA256 => f.write_str("ECDSA_SECP256R1_SHA256"),
            SignatureScheme::ECDSA_SECP384R1_SHA384 => f.write_str("ECDSA_SECP384R1_SHA384"),
            SignatureScheme::ECDSA_SECP521R1_SHA512 => f.write_str("ECDSA_SECP521R1_SHA512"),
            SignatureScheme::ED25519 => f.write_str("ED25519"),
            SignatureScheme::ED448 => f.write_str("ED448"),
            SignatureScheme::RSA_PSS_RSAE_SHA256 => f.write_str("RSA_PSS_RSAE_SHA256"),
            SignatureScheme::RSA_PSS_RSAE_SHA384 => f.write_str("RSA_PSS_RSAE_SHA384"),
            SignatureScheme::RSA_PSS_RSAE_SHA512 => f.write_str("RSA_PSS_RSAE_SHA512"),
            SignatureScheme::RSA_PSS_PSS_SHA256 => f.write_str("RSA_PSS_PSS_SHA256"),
            SignatureScheme::RSA_PSS_PSS_SHA384 => f.write_str("RSA_PSS_PSS_SHA384"),
            SignatureScheme::RSA_PSS_PSS_SHA512 => f.write_str("RSA_PSS_PSS_SHA512"),
            SignatureScheme::RSA_PKCS1_SHA256 => f.write_str("RSA_PKCS1_SHA256"),
            SignatureScheme::RSA_PKCS1_SHA384 => f.write_str("RSA_PKCS1_SHA384"),
            SignatureScheme::RSA_PKCS1_SHA512 => f.write_str("RSA_PKCS1_SHA512"),
            _ => f.debug_tuple("Unknown").field(&self.0).finish(),
        }
    }
}

// ============================================================================
// DTLS 1.3 Cipher Suites
// ============================================================================

/// Cipher suites for DTLS 1.3 (RFC 9147).
///
/// Unlike DTLS 1.2, TLS 1.3 cipher suites only specify the AEAD algorithm
/// and hash function. Key exchange is negotiated separately via key_share.
#[repr(transparent)]
#[derive(Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Dtls13CipherSuite(u16);

impl Dtls13CipherSuite {
    /// TLS_AES_128_GCM_SHA256.
    pub const AES_128_GCM_SHA256: Self = Self(0x1301);
    /// TLS_AES_256_GCM_SHA384.
    pub const AES_256_GCM_SHA384: Self = Self(0x1302);
    /// TLS_CHACHA20_POLY1305_SHA256.
    pub const CHACHA20_POLY1305_SHA256: Self = Self(0x1303);
    /// TLS_AES_128_CCM_SHA256.
    pub const AES_128_CCM_SHA256: Self = Self(0x1304);
    /// TLS_AES_128_CCM_8_SHA256 (shorter tag, for constrained devices).
    pub const AES_128_CCM_8_SHA256: Self = Self(0x1305);

    /// Convert a wire format u16 value to a `Dtls13CipherSuite`.
    pub const fn from_u16(value: u16) -> Self {
        Self(value)
    }

    /// Convert this `Dtls13CipherSuite` to its wire format u16 value.
    pub const fn as_u16(&self) -> u16 {
        self.0
    }

    /// Returns true if this is not a known DTLS 1.3 cipher suite wire value.
    pub const fn is_unknown(&self) -> bool {
        !matches!(*self, Self(0x1301..=0x1305))
    }

    /// Parse a `Dtls13CipherSuite` from wire format.
    pub fn parse(input: &[u8]) -> IResult<&[u8], Dtls13CipherSuite> {
        let (input, value) = be_u16(input)?;
        Ok((input, Dtls13CipherSuite::from_u16(value)))
    }

    /// Returns the hash algorithm used by this cipher suite.
    pub fn hash_algorithm(&self) -> HashAlgorithm {
        match *self {
            Dtls13CipherSuite::AES_128_GCM_SHA256
            | Dtls13CipherSuite::CHACHA20_POLY1305_SHA256
            | Dtls13CipherSuite::AES_128_CCM_SHA256
            | Dtls13CipherSuite::AES_128_CCM_8_SHA256 => HashAlgorithm::SHA256,
            Dtls13CipherSuite::AES_256_GCM_SHA384 => HashAlgorithm::SHA384,
            _ => HashAlgorithm::UNKNOWN_DERIVED,
        }
    }

    /// Returns true if this cipher suite is supported by this implementation.
    pub fn is_supported(&self) -> bool {
        Self::supported().contains(self)
    }

    /// All recognized DTLS 1.3 cipher suites (every non-`Unknown` variant).
    pub const fn all() -> &'static [Dtls13CipherSuite] {
        &[
            Dtls13CipherSuite::AES_128_GCM_SHA256,
            Dtls13CipherSuite::AES_256_GCM_SHA384,
            Dtls13CipherSuite::CHACHA20_POLY1305_SHA256,
            Dtls13CipherSuite::AES_128_CCM_SHA256,
            Dtls13CipherSuite::AES_128_CCM_8_SHA256,
        ]
    }

    /// Supported DTLS 1.3 cipher suites in preference order.
    pub const fn supported() -> &'static [Dtls13CipherSuite] {
        &[
            Dtls13CipherSuite::AES_128_GCM_SHA256,
            Dtls13CipherSuite::AES_256_GCM_SHA384,
            Dtls13CipherSuite::CHACHA20_POLY1305_SHA256,
        ]
    }

    /// Length in bytes of verify_data for Finished messages.
    pub fn verify_data_length(&self) -> usize {
        self.hash_algorithm().output_len()
    }
}

impl fmt::Debug for Dtls13CipherSuite {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Dtls13CipherSuite::AES_128_GCM_SHA256 => f.write_str("AES_128_GCM_SHA256"),
            Dtls13CipherSuite::AES_256_GCM_SHA384 => f.write_str("AES_256_GCM_SHA384"),
            Dtls13CipherSuite::CHACHA20_POLY1305_SHA256 => f.write_str("CHACHA20_POLY1305_SHA256"),
            Dtls13CipherSuite::AES_128_CCM_SHA256 => f.write_str("AES_128_CCM_SHA256"),
            Dtls13CipherSuite::AES_128_CCM_8_SHA256 => f.write_str("AES_128_CCM_8_SHA256"),
            _ => f.debug_tuple("Unknown").field(&self.0).finish(),
        }
    }
}

// ============================================================================
// Protocol Version
// ============================================================================

/// DTLS protocol version identifiers.
///
/// Used in record headers and handshake messages for both DTLS 1.2 and 1.3.
#[repr(transparent)]
#[derive(Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct ProtocolVersion(u16);

impl ProtocolVersion {
    /// DTLS 1.0.
    pub const DTLS1_0: Self = Self(0xFEFF);
    /// DTLS 1.2.
    pub const DTLS1_2: Self = Self(0xFEFD);
    /// DTLS 1.3.
    pub const DTLS1_3: Self = Self(0xFEFC);

    /// Convert a wire format u16 value to a `ProtocolVersion`.
    pub const fn from_u16(value: u16) -> Self {
        Self(value)
    }

    /// Convert this `ProtocolVersion` to its wire format u16 value.
    pub const fn as_u16(&self) -> u16 {
        self.0
    }

    /// Returns true if this is not a known DTLS protocol version wire value.
    pub const fn is_unknown(&self) -> bool {
        !matches!(*self, Self(0xFEFF | 0xFEFD | 0xFEFC))
    }

    /// Parse a `ProtocolVersion` from wire format.
    pub fn parse(input: &[u8]) -> IResult<&[u8], ProtocolVersion> {
        let (input, version) = be_u16(input)?;
        Ok((input, ProtocolVersion::from_u16(version)))
    }

    /// Serialize this `ProtocolVersion` to wire format.
    pub fn serialize(&self, output: &mut Buf) {
        output.extend_from_slice(&self.as_u16().to_be_bytes());
    }
}

impl fmt::Debug for ProtocolVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            ProtocolVersion::DTLS1_0 => f.write_str("DTLS1_0"),
            ProtocolVersion::DTLS1_2 => f.write_str("DTLS1_2"),
            ProtocolVersion::DTLS1_3 => f.write_str("DTLS1_3"),
            _ => f.debug_tuple("Unknown").field(&self.0).finish(),
        }
    }
}

// ============================================================================
// Compression Method
// ============================================================================

/// TLS compression methods.
///
/// Used in ClientHello/ServerHello for both DTLS 1.2 and 1.3.
/// TLS 1.3 only uses Null compression but includes it for compatibility.
#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct CompressionMethod(u8);

impl Default for CompressionMethod {
    fn default() -> Self {
        Self::Null
    }
}

#[allow(non_upper_case_globals)]
impl CompressionMethod {
    /// No compression.
    pub const Null: Self = Self(0x00);
    /// DEFLATE compression.
    pub const Deflate: Self = Self(0x01);

    /// Convert a u8 value to a `CompressionMethod`.
    pub const fn from_u8(value: u8) -> Self {
        Self(value)
    }

    /// Returns true if this compression method is supported by this implementation.
    pub fn is_supported(&self) -> bool {
        Self::supported().contains(self)
    }

    /// All recognized compression methods (every non-`Unknown` variant).
    pub const fn all() -> &'static [CompressionMethod; 2] {
        &[CompressionMethod::Null, CompressionMethod::Deflate]
    }

    /// Supported compression methods.
    ///
    /// Only null compression is supported. TLS 1.3 / DTLS 1.3 (RFC 8446
    /// §4.1.2) mandates exactly one compression method (null). DEFLATE
    /// is recognized by parsing but not accepted.
    pub const fn supported() -> &'static [CompressionMethod; 1] {
        &[CompressionMethod::Null]
    }

    /// Convert this `CompressionMethod` to its u8 value.
    pub const fn as_u8(&self) -> u8 {
        self.0
    }

    /// Returns true if this is not a known TLS compression method wire value.
    pub const fn is_unknown(&self) -> bool {
        self.0 > Self::Deflate.0
    }

    /// Parse a `CompressionMethod` from wire format.
    pub fn parse(input: &[u8]) -> IResult<&[u8], CompressionMethod> {
        let (input, value) = be_u8(input)?;
        Ok((input, CompressionMethod::from_u8(value)))
    }
}

impl fmt::Debug for CompressionMethod {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            CompressionMethod::Null => f.write_str("Null"),
            CompressionMethod::Deflate => f.write_str("Deflate"),
            _ => f.debug_tuple("Unknown").field(&self.0).finish(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_group_newtype_shape() {
        assert_eq!(std::mem::size_of::<NamedGroup>(), 2);
        assert_eq!(NamedGroup::default().as_u16(), 0);
        assert!(NamedGroup::default().is_unknown());
    }

    #[test]
    fn named_group_wire_roundtrip() {
        for group in NamedGroup::all() {
            assert_eq!(NamedGroup::from_u16(group.as_u16()), *group);
            assert!(!group.is_unknown());
        }

        let unknown = NamedGroup::from_u16(0xFFFF);
        assert_eq!(unknown.as_u16(), 0xFFFF);
        assert!(unknown.is_unknown());
    }

    #[test]
    fn named_group_debug_stays_enum_like() {
        assert_eq!(format!("{:?}", NamedGroup::Secp256r1), "Secp256r1");
        assert_eq!(format!("{:?}", NamedGroup::X25519), "X25519");
        assert_eq!(
            format!("{:?}", NamedGroup::from_u16(0xFFFF)),
            "Unknown(65535)"
        );
    }

    #[test]
    fn hash_algorithm_newtype_shape() {
        assert_eq!(std::mem::size_of::<HashAlgorithm>(), 1);
        assert_eq!(HashAlgorithm::default().as_u8(), 0);
        assert_eq!(HashAlgorithm::default(), HashAlgorithm::None);
    }

    #[test]
    fn hash_algorithm_wire_roundtrip() {
        let known = [
            (0, HashAlgorithm::None),
            (1, HashAlgorithm::MD5),
            (2, HashAlgorithm::SHA1),
            (3, HashAlgorithm::SHA224),
            (4, HashAlgorithm::SHA256),
            (5, HashAlgorithm::SHA384),
            (6, HashAlgorithm::SHA512),
        ];

        for (wire, algorithm) in known {
            assert_eq!(HashAlgorithm::from_u8(wire), algorithm);
            assert_eq!(algorithm.as_u8(), wire);
            assert!(!algorithm.is_unknown());
        }

        let unknown = HashAlgorithm::from_u8(7);
        assert_eq!(unknown.as_u8(), 7);
        assert!(unknown.is_unknown());
    }

    #[test]
    fn hash_algorithm_output_len() {
        assert_eq!(HashAlgorithm::None.output_len(), 0);
        assert_eq!(HashAlgorithm::MD5.output_len(), 16);
        assert_eq!(HashAlgorithm::SHA1.output_len(), 20);
        assert_eq!(HashAlgorithm::SHA224.output_len(), 28);
        assert_eq!(HashAlgorithm::SHA256.output_len(), 32);
        assert_eq!(HashAlgorithm::SHA384.output_len(), 48);
        assert_eq!(HashAlgorithm::SHA512.output_len(), 64);
        assert_eq!(HashAlgorithm::from_u8(7).output_len(), 0);
    }

    #[test]
    fn hash_algorithm_debug_stays_enum_like() {
        assert_eq!(format!("{:?}", HashAlgorithm::None), "None");
        assert_eq!(format!("{:?}", HashAlgorithm::SHA256), "SHA256");
        assert_eq!(format!("{:?}", HashAlgorithm::from_u8(7)), "Unknown(7)");
    }

    #[test]
    fn signature_algorithm_newtype_shape() {
        assert_eq!(std::mem::size_of::<SignatureAlgorithm>(), 1);
        assert_eq!(SignatureAlgorithm::default().as_u8(), 0);
        assert_eq!(SignatureAlgorithm::default(), SignatureAlgorithm::Anonymous);
    }

    #[test]
    fn signature_algorithm_wire_roundtrip() {
        let known = [
            (0, SignatureAlgorithm::Anonymous),
            (1, SignatureAlgorithm::RSA),
            (2, SignatureAlgorithm::DSA),
            (3, SignatureAlgorithm::ECDSA),
        ];

        for (wire, algorithm) in known {
            assert_eq!(SignatureAlgorithm::from_u8(wire), algorithm);
            assert_eq!(algorithm.as_u8(), wire);
            assert!(!algorithm.is_unknown());
        }

        let unknown = SignatureAlgorithm::from_u8(4);
        assert_eq!(unknown.as_u8(), 4);
        assert!(unknown.is_unknown());
    }

    #[test]
    fn signature_algorithm_debug_stays_enum_like() {
        assert_eq!(format!("{:?}", SignatureAlgorithm::Anonymous), "Anonymous");
        assert_eq!(format!("{:?}", SignatureAlgorithm::ECDSA), "ECDSA");
        assert_eq!(
            format!("{:?}", SignatureAlgorithm::from_u8(4)),
            "Unknown(4)"
        );
    }

    #[test]
    fn compression_method_newtype_shape() {
        assert_eq!(std::mem::size_of::<CompressionMethod>(), 1);
        assert_eq!(CompressionMethod::default().as_u8(), 0);
        assert_eq!(CompressionMethod::default(), CompressionMethod::Null);
    }

    #[test]
    fn compression_method_wire_roundtrip() {
        let known = [
            (0x00, CompressionMethod::Null),
            (0x01, CompressionMethod::Deflate),
        ];

        for (wire, method) in known {
            assert_eq!(CompressionMethod::from_u8(wire), method);
            assert_eq!(method.as_u8(), wire);
            assert!(!method.is_unknown());
        }

        let unknown = CompressionMethod::from_u8(0x02);
        assert_eq!(unknown.as_u8(), 0x02);
        assert!(unknown.is_unknown());
    }

    #[test]
    fn compression_method_debug_stays_enum_like() {
        assert_eq!(format!("{:?}", CompressionMethod::Null), "Null");
        assert_eq!(format!("{:?}", CompressionMethod::Deflate), "Deflate");
        assert_eq!(
            format!("{:?}", CompressionMethod::from_u8(0x02)),
            "Unknown(2)"
        );
    }

    #[test]
    fn content_type_newtype_shape() {
        assert_eq!(std::mem::size_of::<ContentType>(), 1);
        assert_eq!(ContentType::default().as_u8(), 0);
        assert!(ContentType::default().is_unknown());
    }

    #[test]
    fn content_type_wire_roundtrip() {
        let known = [
            (20, ContentType::ChangeCipherSpec),
            (21, ContentType::Alert),
            (22, ContentType::Handshake),
            (23, ContentType::ApplicationData),
            (26, ContentType::Ack),
        ];

        for (wire, content_type) in known {
            assert_eq!(ContentType::from_u8(wire), content_type);
            assert_eq!(content_type.as_u8(), wire);
            assert!(!content_type.is_unknown());
        }

        let unknown = ContentType::from_u8(24);
        assert_eq!(unknown.as_u8(), 24);
        assert!(unknown.is_unknown());
    }

    #[test]
    fn content_type_debug_stays_enum_like() {
        assert_eq!(
            format!("{:?}", ContentType::ChangeCipherSpec),
            "ChangeCipherSpec"
        );
        assert_eq!(format!("{:?}", ContentType::Handshake), "Handshake");
        assert_eq!(format!("{:?}", ContentType::from_u8(24)), "Unknown(24)");
    }

    #[test]
    fn signature_scheme_newtype_shape() {
        assert_eq!(std::mem::size_of::<SignatureScheme>(), 2);
        assert_eq!(SignatureScheme::default().as_u16(), 0);
        assert!(SignatureScheme::default().is_unknown());
    }

    #[test]
    fn signature_scheme_wire_roundtrip() {
        for scheme in SignatureScheme::all() {
            assert_eq!(SignatureScheme::from_u16(scheme.as_u16()), *scheme);
            assert!(!scheme.is_unknown());
        }

        let unknown = SignatureScheme::from_u16(0xFFFF);
        assert_eq!(unknown.as_u16(), 0xFFFF);
        assert!(unknown.is_unknown());
    }

    #[test]
    fn signature_scheme_debug_stays_enum_like() {
        assert_eq!(
            format!("{:?}", SignatureScheme::ECDSA_SECP256R1_SHA256),
            "ECDSA_SECP256R1_SHA256"
        );
        assert_eq!(
            format!("{:?}", SignatureScheme::from_u16(0xFFFF)),
            "Unknown(65535)"
        );
    }

    #[test]
    fn dtls13_cipher_suite_newtype_shape() {
        assert_eq!(std::mem::size_of::<Dtls13CipherSuite>(), 2);
        assert_eq!(Dtls13CipherSuite::default().as_u16(), 0);
        assert!(Dtls13CipherSuite::default().is_unknown());
    }

    #[test]
    fn dtls13_cipher_suite_wire_roundtrip() {
        for suite in Dtls13CipherSuite::all() {
            assert_eq!(Dtls13CipherSuite::from_u16(suite.as_u16()), *suite);
            assert!(!suite.is_unknown());
        }

        let unknown = Dtls13CipherSuite::from_u16(0xFFFF);
        assert_eq!(unknown.as_u16(), 0xFFFF);
        assert!(unknown.is_unknown());
    }

    #[test]
    fn dtls13_cipher_suite_debug_stays_enum_like() {
        assert_eq!(
            format!("{:?}", Dtls13CipherSuite::AES_128_GCM_SHA256),
            "AES_128_GCM_SHA256"
        );
        assert_eq!(
            format!("{:?}", Dtls13CipherSuite::from_u16(0xFFFF)),
            "Unknown(65535)"
        );
    }

    #[test]
    fn protocol_version_newtype_shape() {
        assert_eq!(std::mem::size_of::<ProtocolVersion>(), 2);
        assert_eq!(ProtocolVersion::default().as_u16(), 0);
        assert!(ProtocolVersion::default().is_unknown());
    }

    #[test]
    fn protocol_version_wire_roundtrip() {
        let known = [
            (0xFEFF, ProtocolVersion::DTLS1_0),
            (0xFEFD, ProtocolVersion::DTLS1_2),
            (0xFEFC, ProtocolVersion::DTLS1_3),
        ];

        for (wire, version) in known {
            assert_eq!(ProtocolVersion::from_u16(wire), version);
            assert_eq!(version.as_u16(), wire);
            assert!(!version.is_unknown());
        }

        let unknown = ProtocolVersion::from_u16(0xFFFF);
        assert_eq!(unknown.as_u16(), 0xFFFF);
        assert!(unknown.is_unknown());
    }

    #[test]
    fn protocol_version_debug_stays_enum_like() {
        assert_eq!(format!("{:?}", ProtocolVersion::DTLS1_2), "DTLS1_2");
        assert_eq!(
            format!("{:?}", ProtocolVersion::from_u16(0xFFFF)),
            "Unknown(65535)"
        );
    }

    #[test]
    fn random_parse() {
        let data = [
            0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A, 0x0B, 0x0C, 0x0D, 0x0E,
            0x0F, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1A, 0x1B, 0x1C,
            0x1D, 0x1E, 0x1F, 0x20,
        ];

        let expected = Random { bytes: data };

        let (_, parsed) = Random::parse(&data).unwrap();
        assert_eq!(parsed, expected);
    }

    #[test]
    fn random_serialize() {
        let random = Random {
            bytes: [
                0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A, 0x0B, 0x0C, 0x0D, 0x0E,
                0x0F, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1A, 0x1B, 0x1C,
                0x1D, 0x1E, 0x1F, 0x20,
            ],
        };

        let mut serialized = Buf::new();
        random.serialize(&mut serialized);

        assert_eq!(&*serialized, &random.bytes);
    }

    #[test]
    fn compression_supported_has_only_null() {
        let supported = CompressionMethod::supported();
        assert_eq!(
            supported,
            &[CompressionMethod::Null],
            "Only Null compression should be supported"
        );
    }

    #[test]
    fn signature_scheme_named_group_ecdsa() {
        assert_eq!(
            SignatureScheme::ECDSA_SECP256R1_SHA256.named_group(),
            Some(NamedGroup::Secp256r1)
        );
        assert_eq!(
            SignatureScheme::ECDSA_SECP384R1_SHA384.named_group(),
            Some(NamedGroup::Secp384r1)
        );
    }

    #[test]
    fn signature_scheme_named_group_non_ecdsa() {
        assert_eq!(SignatureScheme::RSA_PSS_RSAE_SHA256.named_group(), None);
        assert_eq!(SignatureScheme::ED25519.named_group(), None);
        assert_eq!(SignatureScheme::ECDSA_SECP521R1_SHA512.named_group(), None);
        assert_eq!(SignatureScheme::from_u16(0xFFFF).named_group(), None);
    }

    #[test]
    fn random_parse_roundtrip() {
        let data = [
            0x5F, 0x37, 0xA9, 0x4B, // could be gmt_unix_time in 1.2
            0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A, 0x0B, 0x0C, 0x0D, 0x0E,
            0x0F, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1A, 0x1B, 0x1C,
        ];

        let (_, parsed) = Random::parse(&data).unwrap();
        let mut serialized = Buf::new();
        parsed.serialize(&mut serialized);

        assert_eq!(&*serialized, &data[..]);
    }
}
