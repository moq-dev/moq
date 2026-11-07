//! Public error type returned by the high-level DTLS API.

use std::fmt;

use crate::dtls12::message::Dtls12CipherSuite;
use crate::types::CompressionMethod;
use crate::types::Dtls13CipherSuite;
use crate::types::HashAlgorithm;
use crate::types::NamedGroup;
use crate::types::ProtocolVersion;
use crate::types::SignatureAlgorithm;
use crate::types::SignatureScheme;

pub(crate) fn bounded_error_len(len: usize) -> u16 {
    len.min(u16::MAX as usize) as u16
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
/// Errors returned by DTLS processing functions.
pub enum Error {
    /// Unexpected DTLS message.
    UnexpectedMessage(UnexpectedMessageError),
    /// Local state was missing data required for the requested operation.
    InvalidState(InvalidStateError),
    /// Cryptographic operation failed.
    CryptoError(CryptoError),
    /// Certificate validation failed.
    CertificateError(CertificateError),
    /// Security policy violation.
    SecurityError(SecurityError),
    /// PSK (Pre-Shared Key) error.
    PskError(PskError),
    /// Incoming queue exceeded capacity.
    ReceiveQueueFull,
    /// Outgoing queue exceeded capacity.
    TransmitQueueFull,
    /// A saved DTLS 1.3 handshake record exhausted its bounded transmission history.
    TransmitHistoryFull,
    /// Missing fields when parsing ServerHello.
    IncompleteServerHello,
    /// Something timed out.
    Timeout(TimeoutError),
    /// Configuration error (e.g., invalid crypto provider).
    ConfigError(ConfigError),
    /// Peer attempted renegotiation (not supported).
    RenegotiationAttempt,
    /// The connection has been closed (close_notify sent or received).
    ConnectionClosed,
    /// If we are in auto-sense mode for a server and we received too
    /// many client hello fragments that haven't made a packet.
    TooManyClientHelloFragments,
    /// The DTLS 1.3 server received a ClientHello that does not offer
    /// DTLS 1.3 in `supported_versions`. In auto-sense mode the caller
    /// should fall back to a DTLS 1.2 server and replay the buffered
    /// packets.
    ///
    /// This value should never be seen outside dimpl. It's an internal
    /// value to communicate from dtls13/server.rs to lib.rs.
    #[doc(hidden)]
    Dtls12Fallback,
}

/// Fine-grained reason for an [`Error::UnexpectedMessage`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum UnexpectedMessageError {
    /// Auto-detection received a server response that was neither DTLS 1.2 nor DTLS 1.3.
    UnrecognizedAutoServerResponse,
    /// A record from a handshake epoch remained when entering application data.
    UnexpectedEpoch,
    /// A DTLS 1.2 `ServerKeyExchange` omitted its required signature.
    ServerKeyExchangeWithoutSignature,
    /// A PSK `ServerKeyExchange` was received while processing an ECDHE suite.
    PskServerKeyExchangeInEcdhePath,
    /// An ECDHE `ServerKeyExchange` was received while processing a PSK suite.
    EcdheServerKeyExchangeInPskPath,
    /// A PSK `ClientKeyExchange` was received while processing an ECDHE suite.
    PskClientKeyExchangeInEcdhePath,
    /// An ECDHE `ClientKeyExchange` was received while processing a PSK suite.
    EcdheClientKeyExchangeInPskPath,
    /// A DTLS 1.3 `CertificateRequest` context was shorter than declared.
    CertificateRequestContextTruncated,
    /// A DTLS 1.3 `CertificateRequest` extension block was shorter than declared.
    CertificateRequestExtensionsTruncated,
}

/// Fine-grained reason for an [`Error::InvalidState`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum InvalidStateError {
    /// No cipher suite has been selected for the connection.
    NoCipherSuiteSelected,
    /// A cipher suite was required but not available.
    NoCipherSuite,
    /// The client random was required but not available.
    NoClientRandom,
    /// The server random was required but not available.
    NoServerRandom,
    /// The handshake key schedule was used before a shared secret existed.
    NoSharedSecretForHandshakeKeyDerivation,
    /// The server handshake traffic secret was required but not available.
    NoServerHandshakeTrafficSecret,
    /// The server handshake traffic secret was required to verify or create `Finished`.
    NoServerHandshakeTrafficSecretForFinished,
    /// The client handshake traffic secret was required but not available.
    NoClientHandshakeTrafficSecret,
    /// The client handshake traffic secret was required to verify or create `Finished`.
    NoClientHandshakeTrafficSecretForFinished,
    /// Application traffic keys were requested before the handshake secret existed.
    NoHandshakeSecretForApplicationKeyDerivation,
    /// A key exchange was required but no exchange was active.
    NoActiveKeyExchange,
    /// A DTLS 1.3 key update was requested before current send keys existed.
    NoCurrentAppSendKeysForKeyUpdate,
    /// A DTLS 1.3 peer key update was processed before current receive keys existed.
    NoCurrentAppRecvKeysForKeyUpdate,
    /// Exported keying material was requested before the exporter secret was derived.
    ExporterMasterSecretNotDerived,
    /// Extended master secret was negotiated, but the session hash was not captured.
    ExtendedMasterSecretSessionHashMissing,
}

/// Fine-grained reason for an [`Error::CryptoError`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum CryptoError {
    /// No supported key exchange group is available.
    NoSupportedKeyExchangeGroups,
    /// DTLS 1.2 needs a key exchange group but none is configured.
    NoDtls12KeyExchangeGroupsConfigured,
    /// The epoch 0 sequence number space has been exhausted.
    Epoch0SequenceNumberExhausted,
    /// The send sequence number space for an epoch has been exhausted.
    SendSequenceNumberExhausted {
        /// The epoch whose send sequence number was exhausted.
        epoch: u16,
    },
    /// Send keys are not available for an epoch.
    SendKeysNotAvailable {
        /// The epoch whose send keys are unavailable.
        epoch: u16,
    },
    /// Receive keys are not available for an epoch.
    RecvKeysNotAvailable {
        /// The epoch whose receive keys are unavailable.
        epoch: u16,
    },
    /// No provider key exchange group matches the negotiated group.
    KeyExchangeGroupNotFound(NamedGroup),
    /// A key exchange operation was requested before initialization.
    KeyExchangeNotInitialized,
    /// The requested key exchange group is unsupported.
    UnsupportedKeyExchangeGroup(NamedGroup),
    /// The requested ECDHE group is unsupported.
    UnsupportedEcdheNamedGroup(NamedGroup),
    /// The requested DTLS 1.2 cipher suite is unsupported.
    UnsupportedCipherSuite(Dtls12CipherSuite),
    /// The requested HMAC hash algorithm is unsupported.
    UnsupportedHmacHash(HashAlgorithm),
    /// The requested signature algorithm is unsupported.
    UnsupportedSignatureAlgorithm(SignatureAlgorithm),
    /// No locally supported signature algorithm was offered by the peer.
    SignatureAlgorithmNotOfferedByClient,
    /// The signature algorithm did not match the expected algorithm.
    SignatureAlgorithmMismatch {
        /// The signature algorithm expected for this operation.
        expected: SignatureAlgorithm,
        /// The signature algorithm actually present.
        actual: SignatureAlgorithm,
    },
    /// The signature and hash algorithm pair is unsupported.
    UnsupportedSignaturePair {
        /// The requested signature algorithm.
        signature: SignatureAlgorithm,
        /// The requested hash algorithm.
        hash: HashAlgorithm,
    },
    /// The signature, hash, and key group combination is unsupported for verification.
    UnsupportedSignatureVerification {
        /// The requested signature algorithm.
        signature: SignatureAlgorithm,
        /// The requested hash algorithm.
        hash: HashAlgorithm,
        /// The public key group used for verification.
        group: NamedGroup,
    },
    /// Signature verification failed for a supported signature/hash/group combination.
    SignatureVerificationFailed {
        /// The requested signature algorithm.
        signature: SignatureAlgorithm,
        /// The requested hash algorithm.
        hash: HashAlgorithm,
        /// The public key group used for verification.
        group: NamedGroup,
    },
    /// The public key algorithm is unsupported.
    UnsupportedPublicKeyAlgorithm,
    /// A certificate or key references an unsupported EC curve.
    UnsupportedEcCurve,
    /// Certificate parsing failed during a crypto operation.
    CertificateParseFailed,
    /// A certificate omitted its required EC curve parameter.
    MissingEcCurveParameter,
    /// A certificate had an invalid EC curve parameter.
    InvalidEcCurveParameter,
    /// A certificate had an invalid subject public key.
    InvalidSubjectPublicKey,
    /// A signature was not encoded in the expected format.
    InvalidSignatureFormat,
    /// A public key could not be parsed for the given group.
    InvalidPublicKey(NamedGroup),
    /// A private key could not be parsed in any supported format.
    InvalidPrivateKey,
    /// A signing key was used with a hash other than the one it is bound to.
    SigningKeyHashMismatch {
        /// The hash algorithm bound to the signing key.
        key_hash: HashAlgorithm,
        /// The hash algorithm requested by the caller.
        requested: HashAlgorithm,
    },
    /// A signing key group does not support the requested hash algorithm.
    SigningKeyUnsupportedHash {
        /// The key group used for signing.
        group: NamedGroup,
        /// The requested hash algorithm.
        hash: HashAlgorithm,
    },
    /// An AES-GCM key had the wrong length.
    InvalidAesGcmKeySize {
        /// The actual key length in bytes.
        ///
        /// Values greater than `u16::MAX` are reported as `u16::MAX`.
        actual: u16,
    },
    /// A ChaCha20-Poly1305 key had the wrong length.
    InvalidChacha20Poly1305KeySize {
        /// The actual key length in bytes.
        ///
        /// Values greater than `u16::MAX` are reported as `u16::MAX`.
        actual: u16,
    },
    /// An AES-128-CCM-8 key had the wrong length.
    InvalidAes128Ccm8KeySize {
        /// The actual key length in bytes.
        ///
        /// Values greater than `u16::MAX` are reported as `u16::MAX`.
        actual: u16,
    },
    /// A nonce was invalid for the selected cipher.
    InvalidNonce,
    /// A ciphertext was shorter than the selected AEAD permits.
    CiphertextTooShort {
        /// The minimum accepted ciphertext length in bytes.
        minimum: u8,
        /// The actual ciphertext length in bytes.
        actual: u8,
    },
    /// The requested HKDF output length is too large.
    HkdfOutputTooLong,
    /// The HKDF label is too long to encode.
    HkdfLabelTooLong,
    /// The HKDF context is too long to encode.
    HkdfContextTooLong,
    /// The HKDF-Expand-Label output length is too large to encode.
    HkdfOutputLengthTooLarge,
    /// The `Finished` verify-data length was invalid.
    InvalidVerifyDataLength,
    /// The `Finished` verify-data is too long to encode.
    VerifyDataTooLong,
    /// The TLS 1.2 master secret is too long to encode.
    MasterSecretTooLong,
    /// The requested exported keying material is too long.
    KeyingMaterialTooLong,
    /// The TLS 1.2 pre-master secret is not available.
    PreMasterSecretNotAvailable,
    /// The TLS 1.2 master secret is not available.
    MasterSecretNotAvailable,
    /// The client random is not available.
    ClientRandomNotAvailable,
    /// The server random is not available.
    ServerRandomNotAvailable,
    /// The client cipher has not been initialized.
    ClientCipherNotInitialized,
    /// The server cipher has not been initialized.
    ServerCipherNotInitialized,
    /// A write IV is not available for the requested side.
    WriteIvNotAvailable {
        /// Whether the missing write IV is for the client side.
        is_client: bool,
    },
    /// The DTLS 1.2 record IV length is unsupported for the selected suite.
    UnsupportedDtls12RecordIvLen {
        /// The unsupported record IV length.
        ///
        /// Values greater than `u16::MAX` are reported as `u16::MAX`.
        len: u16,
        /// The selected DTLS 1.2 cipher suite.
        suite: Dtls12CipherSuite,
    },
    /// No private key is configured.
    NoPrivateKeyConfigured,
    /// A PSK operation was requested before a PSK was set.
    PskNotSet,
    /// The exporter master secret is not available.
    ExporterMasterSecretNotDerived,
    /// A provider operation failed without a more specific reason.
    OperationFailed(CryptoOperation),
}

/// A cryptographic operation that can fail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum CryptoOperation {
    /// Create an AEAD cipher instance.
    CreateCipher,
    /// Encrypt plaintext.
    Encrypt,
    /// Decrypt ciphertext.
    Decrypt,
    /// Sign a transcript or payload.
    Sign,
    /// Verify a signature.
    VerifySignature,
    /// Load a private key.
    LoadPrivateKey,
    /// Start a key exchange.
    StartKeyExchange,
    /// Complete a key exchange.
    CompleteKeyExchange,
    /// Generate an ephemeral key.
    GenerateEphemeralKey,
    /// Compute a public key.
    ComputePublicKey,
    /// Fill a buffer with random bytes.
    FillRandom,
    /// Compute an HMAC.
    ComputeHmac,
    /// Run the TLS 1.2 PRF.
    Prf,
    /// Run HKDF-Extract.
    HkdfExtract,
    /// Run HKDF-Expand.
    HkdfExpand,
    /// Run TLS HKDF-Expand-Label.
    HkdfExpandLabel,
    /// Derive a DTLS 1.3 early secret.
    DeriveEarlySecret,
    /// Derive a DTLS 1.3 derived secret.
    DeriveDerivedSecret,
    /// Derive a DTLS 1.3 handshake secret.
    DeriveHandshakeSecret,
    /// Derive a DTLS 1.3 traffic secret.
    DeriveTrafficSecret,
    /// Derive a DTLS 1.3 master secret.
    DeriveMasterSecret,
    /// Derive a DTLS 1.3 exporter master secret.
    DeriveExporterMasterSecret,
    /// Derive a DTLS 1.3 next-generation traffic secret.
    DeriveNextTrafficSecret,
    /// Derive an AEAD key.
    DeriveKey,
    /// Derive an AEAD IV.
    DeriveIv,
    /// Derive a record sequence-number encryption key.
    DeriveSequenceNumberKey,
    /// Derive a Finished-message key.
    DeriveFinishedKey,
    /// Compute Finished verify data.
    ComputeVerifyData,
    /// Verify Finished verify data.
    VerifyData,
    /// Compute a PSK pre-master secret.
    ComputePskPreMasterSecret,
    /// Compute a DTLS cookie.
    ComputeCookie,
    /// Extract SRTP keying material.
    ExtractSrtpKeyingMaterial,
    /// Encode a key.
    EncodeKey,
    /// Decode a key.
    DecodeKey,
}

/// Fine-grained reason for an [`Error::CertificateError`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum CertificateError {
    /// The peer did not send a server certificate.
    NoServerCertificateReceived,
    /// The client sent an empty certificate list when a certificate was required.
    NoClientCertificateReceived,
    /// Client certificate verification was requested with no client certificate.
    NoClientCertificateForVerification,
    /// Server certificate verification was requested with no server certificate.
    NoServerCertificateForVerification,
    /// A DTLS 1.3 server certificate carried a non-empty context.
    ServerCertificateContextMustBeEmpty,
    /// A DTLS 1.3 client certificate carried a non-empty context.
    ClientCertificateContextMustBeEmpty,
    /// The certificate operation needs an unsupported hash algorithm.
    UnsupportedHashAlgorithm(HashAlgorithm),
    /// Certificate parsing failed.
    ParseFailed,
    /// A certificate omitted its EC curve parameter.
    MissingEcCurveParameter,
    /// A certificate had an invalid EC curve parameter.
    InvalidEcCurveParameter,
    /// A certificate references an unsupported EC curve.
    UnsupportedEcCurve,
    /// A certificate had an invalid subject public key.
    InvalidSubjectPublicKey,
}

/// Fine-grained reason for an [`Error::SecurityError`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum SecurityError {
    /// `HelloVerifyRequest` used an unsupported protocol version.
    UnsupportedHelloVerifyRequestVersion(ProtocolVersion),
    /// A server selected an unsupported protocol version.
    UnsupportedServerVersion(ProtocolVersion),
    /// A client offered an unsupported protocol version.
    UnsupportedClientVersion(ProtocolVersion),
    /// A server selected an unsupported DTLS 1.2 compression method.
    UnsupportedServerCompression(CompressionMethod),
    /// A client did not offer null compression.
    UnsupportedClientCompression,
    /// The selected key exchange algorithm is unsupported.
    UnsupportedKeyExchangeAlgorithm,
    /// The server selected a cipher suite that was not offered or recognized.
    ServerSelectedUnknownCipherSuite,
    /// The server selected a DTLS 1.2 cipher suite incompatible with local mode.
    ServerSelectedIncompatibleCipherSuite(Dtls12CipherSuite),
    /// The server selected a DTLS 1.2 cipher suite disallowed by configuration.
    ServerSelectedDisallowedCipherSuite(Dtls12CipherSuite),
    /// The server selected a DTLS 1.3 cipher suite disallowed by configuration.
    ServerSelectedDisallowedDtls13CipherSuite(Dtls13CipherSuite),
    /// DTLS 1.2 extended master secret was required but not negotiated.
    ExtendedMasterSecretNotNegotiated,
    /// No mutually acceptable cipher suite was found.
    NoMutuallyAcceptableCipherSuite,
    /// A DTLS 1.3 ClientHello did not use the required DTLS 1.2 legacy version.
    ClientHelloLegacyVersionNotDtls12,
    /// A DTLS 1.3 ServerHello did not use the required DTLS 1.2 legacy version.
    ServerHelloLegacyVersionNotDtls12,
    /// A DTLS 1.3 ClientHello did not contain a recognized DTLS 1.3 version.
    ClientHelloMissingDtls13SupportedVersions,
    /// A ClientHello did not offer null compression.
    ClientHelloMustOfferNullCompression,
    /// The ClientHello cookie did not match the expected cookie.
    InvalidCookieInClientHello,
    /// A DTLS 1.3 ClientHello carried a non-empty legacy_cookie field.
    InvalidLegacyCookieInClientHello,
    /// The server attempted to send a second HelloRetryRequest.
    CannotSendSecondHelloRetryRequest,
    /// The client received a second HelloRetryRequest.
    UnexpectedSecondHelloRetryRequest,
    /// No common DTLS 1.3 cipher suite was found.
    NoCommonCipherSuite,
    /// No common DTLS 1.3 key exchange group was found.
    NoCommonKeyExchangeGroup,
    /// A HelloRetryRequest selected a disallowed cipher suite.
    HrrSelectedDisallowedCipherSuite,
    /// A HelloRetryRequest did not select DTLS 1.3.
    HrrDidNotSelectDtls13,
    /// A ServerHello selected a non-null compression method.
    ServerHelloCompressionMustBeNull,
    /// A server did not negotiate DTLS 1.3.
    ServerDidNotNegotiateDtls13,
    /// A DTLS 1.3 server did not send a key share.
    ServerMissingKeyShare,
    /// The server key share group did not match the expected group.
    ServerKeyShareGroupMismatch {
        /// The expected key exchange group.
        expected: NamedGroup,
        /// The key exchange group in the server key share.
        actual: NamedGroup,
    },
    /// A signature was too large to encode.
    SignatureTooLarge,
    /// A signature scheme was used even though the peer did not offer it.
    SignatureSchemeNotOffered(SignatureScheme),
    /// A signature algorithm did not match the expected algorithm.
    SignatureAlgorithmMismatch {
        /// The expected signature algorithm.
        expected: SignatureAlgorithm,
        /// The actual signature algorithm.
        actual: SignatureAlgorithm,
    },
    /// The signature scheme is unsupported.
    UnsupportedSignatureScheme(SignatureScheme),
    /// The signature scheme and certificate curve are incompatible.
    SignatureSchemeCertificateCurveMismatch {
        /// The signature scheme used for the operation.
        scheme: SignatureScheme,
        /// The certificate curve required by the signature scheme.
        expected: NamedGroup,
        /// The actual certificate curve.
        actual: NamedGroup,
    },
    /// Server Finished verification failed.
    ServerFinishedVerificationFailed,
    /// Client Finished verification failed.
    ClientFinishedVerificationFailed,
    /// A fatal DTLS alert was received.
    FatalAlert {
        /// The DTLS alert description.
        description: u8,
    },
}

/// Fine-grained reason for an [`Error::PskError`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum PskError {
    /// No PSK resolver is configured.
    NoPskResolverConfigured,
    /// No PSK identity is configured.
    NoPskIdentityConfigured,
    /// The configured PSK resolver did not return a key.
    ResolverReturnedNoKey,
}

/// Fine-grained reason for an [`Error::Timeout`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum TimeoutError {
    /// Timeout while waiting for an auto client hybrid ClientHello to resolve.
    HybridClientHello,
    /// Timeout while connecting.
    Connect,
    /// Timeout while handshaking.
    Handshake,
}

/// Fine-grained reason for an [`Error::ConfigError`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ConfigError {
    /// The configured MTU is smaller than dimpl permits.
    MtuTooSmall {
        /// The configured MTU.
        mtu: u16,
        /// The minimum accepted MTU.
        minimum: u16,
    },
    /// The configured AEAD encryption limit is too small.
    AeadEncryptionLimitTooSmall,
    /// The handshake timeout is zero or greater than 3650 days.
    InvalidHandshakeTimeout,
    /// The initial flight RTO is zero or greater than 3650 days.
    InvalidFlightStartRto,
    /// Cipher-suite filtering removed every available suite.
    NoCipherSuitesAfterFiltering,
    /// A PSK resolver is configured but no PSK cipher suite remains enabled.
    PskConfiguredWithoutPskCipherSuite,
    /// DTLS 1.2 suites are enabled but no compatible key exchange group remains enabled.
    NoDtls12KeyExchangeGroupsAfterFiltering,
    /// DTLS 1.3 suites are enabled but no key exchange group remains enabled.
    NoDtls13KeyExchangeGroupsAfterFiltering,
    /// Crypto provider validation failed.
    CryptoProvider(CryptoProviderValidationError),
}

/// Fine-grained reason for crypto provider validation failure.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum CryptoProviderValidationError {
    /// The provider has no cipher suites supported by dimpl.
    NoCipherSuites,
    /// The provider has ECDH cipher suites but no key exchange groups.
    EcdhCipherSuitesWithoutKeyExchangeGroups,
    /// The provider has no DTLS 1.3 cipher suites.
    NoDtls13CipherSuites,
    /// No hash test vector exists for the hash algorithm.
    MissingHashTestVector(HashAlgorithm),
    /// The provider hash implementation returned an unexpected value.
    HashProviderIncorrect(HashAlgorithm),
    /// The provider PRF operation failed.
    PrfFailed {
        /// The hash algorithm used by the PRF.
        hash: HashAlgorithm,
        /// The underlying crypto failure.
        source: CryptoError,
    },
    /// No PRF test vector exists for the hash algorithm.
    MissingPrfTestVector(HashAlgorithm),
    /// The provider PRF returned incorrect output.
    PrfIncorrect(HashAlgorithm),
    /// No signature-validation vector exists for the algorithm pair.
    NoSignatureValidationVector {
        /// The hash algorithm used by the vector.
        hash: HashAlgorithm,
        /// The signature algorithm used by the vector.
        signature: SignatureAlgorithm,
    },
    /// Provider signature verification failed.
    SignatureVerificationFailed {
        /// The hash algorithm used for verification.
        hash: HashAlgorithm,
        /// The signature algorithm used for verification.
        signature: SignatureAlgorithm,
        /// The underlying crypto failure.
        source: CryptoError,
    },
    /// Provider HKDF validation failed.
    HkdfFailed {
        /// The DTLS 1.3 cipher suite under validation.
        suite: Dtls13CipherSuite,
        /// The underlying crypto failure.
        source: CryptoError,
    },
    /// Provider HKDF returned empty output.
    HkdfEmptyOutput(Dtls13CipherSuite),
    /// No AEAD test vector exists for the suite.
    NoAeadTestVector(Dtls13CipherSuite),
    /// Creating the AEAD cipher failed.
    AeadCreateFailed {
        /// The DTLS 1.3 cipher suite under validation.
        suite: Dtls13CipherSuite,
        /// The underlying crypto failure.
        source: CryptoError,
    },
    /// AEAD encryption failed.
    AeadEncryptFailed {
        /// The DTLS 1.3 cipher suite under validation.
        suite: Dtls13CipherSuite,
        /// The underlying crypto failure.
        source: CryptoError,
    },
    /// AEAD encryption returned the wrong output.
    AeadEncryptWrongOutput(Dtls13CipherSuite),
    /// AEAD decryption failed.
    AeadDecryptFailed {
        /// The DTLS 1.3 cipher suite under validation.
        suite: Dtls13CipherSuite,
        /// The underlying crypto failure.
        source: CryptoError,
    },
    /// AEAD decryption returned the wrong output.
    AeadDecryptWrongOutput(Dtls13CipherSuite),
    /// No record-number-encryption vector exists for the suite.
    NoRecordNumberEncryptionTestVector(Dtls13CipherSuite),
    /// Record-number encryption returned the wrong mask.
    RecordNumberEncryptionWrongMask(Dtls13CipherSuite),
    /// Starting key exchange failed.
    KeyExchangeStartFailed {
        /// The key exchange group under validation.
        group: NamedGroup,
        /// The underlying crypto failure.
        source: CryptoError,
    },
    /// Completing key exchange failed.
    KeyExchangeCompleteFailed {
        /// The key exchange group under validation.
        group: NamedGroup,
        /// The underlying crypto failure.
        source: CryptoError,
    },
    /// Two sides of a validation key exchange produced different shared secrets.
    KeyExchangeMismatchedSharedSecret(NamedGroup),
    /// HMAC validation failed.
    HmacFailed(CryptoError),
    /// HMAC validation returned incorrect output.
    HmacIncorrect,
}

#[derive(Debug)]
pub(crate) enum InternalError {
    Transient(TransientError),
    Fatal(Error),
}

#[derive(Debug)]
pub(crate) enum TransientError {
    ParseIncomplete,
    Parse(nom::error::ErrorKind),
    TooManyRecords,
}

impl InternalError {
    pub(crate) fn parse_incomplete() -> Self {
        Self::Transient(TransientError::ParseIncomplete)
    }

    pub(crate) fn parse(kind: nom::error::ErrorKind) -> Self {
        Self::Transient(TransientError::Parse(kind))
    }

    pub(crate) fn too_many_records() -> Self {
        Self::Transient(TransientError::TooManyRecords)
    }

    pub(crate) fn into_public_error(self) -> Option<Error> {
        match self {
            Self::Transient(err) => {
                debug!("Discarding packet: {err}");
                None
            }
            Self::Fatal(err) => Some(err),
        }
    }
}

impl From<Error> for InternalError {
    fn from(value: Error) -> Self {
        Self::Fatal(value)
    }
}

impl<'a> From<nom::Err<nom::error::Error<&'a [u8]>>> for InternalError {
    fn from(value: nom::Err<nom::error::Error<&'a [u8]>>) -> Self {
        match value {
            nom::Err::Incomplete(_) => InternalError::parse_incomplete(),
            nom::Err::Error(x) => InternalError::parse(x.code),
            nom::Err::Failure(x) => InternalError::parse(x.code),
        }
    }
}

impl From<CryptoError> for Error {
    fn from(value: CryptoError) -> Self {
        Self::CryptoError(value)
    }
}

impl From<CertificateError> for Error {
    fn from(value: CertificateError) -> Self {
        Self::CertificateError(value)
    }
}

impl From<SecurityError> for Error {
    fn from(value: SecurityError) -> Self {
        Self::SecurityError(value)
    }
}

impl From<PskError> for Error {
    fn from(value: PskError) -> Self {
        Self::PskError(value)
    }
}

impl From<ConfigError> for Error {
    fn from(value: ConfigError) -> Self {
        Self::ConfigError(value)
    }
}

impl std::error::Error for Error {}

impl fmt::Display for InternalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            InternalError::Transient(err) => err.fmt(f),
            InternalError::Fatal(err) => err.fmt(f),
        }
    }
}

impl fmt::Display for TransientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TransientError::ParseIncomplete => write!(f, "parse incomplete"),
            TransientError::Parse(kind) => write!(f, "parse error: {:?}", kind),
            TransientError::TooManyRecords => write!(f, "too many records in packet"),
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::UnexpectedMessage(err) => write!(f, "unexpected message: {err}"),
            Error::InvalidState(err) => write!(f, "invalid state: {err}"),
            Error::CryptoError(err) => write!(f, "crypto error: {err}"),
            Error::CertificateError(err) => write!(f, "certificate error: {err}"),
            Error::SecurityError(err) => write!(f, "security error: {err}"),
            Error::PskError(err) => write!(f, "psk error: {err}"),
            Error::ReceiveQueueFull => write!(f, "receive queue full"),
            Error::TransmitQueueFull => write!(f, "transmit queue full"),
            Error::TransmitHistoryFull => write!(f, "transmit record-number history full"),
            Error::IncompleteServerHello => write!(f, "incomplete ServerHello"),
            Error::Timeout(err) => write!(f, "timeout: {err}"),
            Error::ConfigError(err) => write!(f, "config error: {err}"),
            Error::RenegotiationAttempt => write!(f, "peer attempted renegotiation"),
            Error::TooManyClientHelloFragments => write!(f, "too many client hello fragments"),
            Error::ConnectionClosed => write!(f, "connection closed"),
            Error::Dtls12Fallback => write!(f, "dtls 1.2 fallback (internal)"),
        }
    }
}

impl fmt::Display for UnexpectedMessageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnrecognizedAutoServerResponse => write!(f, "unrecognized response from server"),
            Self::UnexpectedEpoch => {
                write!(
                    f,
                    "pending record in unexpected epoch at handshake completion"
                )
            }
            Self::ServerKeyExchangeWithoutSignature => {
                write!(f, "ServerKeyExchange without signature")
            }
            Self::PskServerKeyExchangeInEcdhePath => {
                write!(f, "PSK ServerKeyExchange in ECDHE path")
            }
            Self::EcdheServerKeyExchangeInPskPath => {
                write!(f, "ECDHE ServerKeyExchange in PSK path")
            }
            Self::PskClientKeyExchangeInEcdhePath => {
                write!(f, "PSK ClientKeyExchange in ECDHE path")
            }
            Self::EcdheClientKeyExchangeInPskPath => {
                write!(f, "ECDHE ClientKeyExchange in PSK path")
            }
            Self::CertificateRequestContextTruncated => {
                write!(f, "CertificateRequest context truncated")
            }
            Self::CertificateRequestExtensionsTruncated => {
                write!(f, "CertificateRequest extensions truncated")
            }
        }
    }
}

impl fmt::Display for InvalidStateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoCipherSuiteSelected => write!(f, "no cipher suite selected"),
            Self::NoCipherSuite => write!(f, "no cipher suite"),
            Self::NoClientRandom => write!(f, "no client random"),
            Self::NoServerRandom => write!(f, "no server random"),
            Self::NoSharedSecretForHandshakeKeyDerivation => {
                write!(f, "no shared secret for handshake key derivation")
            }
            Self::NoServerHandshakeTrafficSecret => {
                write!(f, "no server handshake traffic secret")
            }
            Self::NoServerHandshakeTrafficSecretForFinished => {
                write!(f, "no server handshake traffic secret for Finished")
            }
            Self::NoClientHandshakeTrafficSecret => {
                write!(f, "no client handshake traffic secret")
            }
            Self::NoClientHandshakeTrafficSecretForFinished => {
                write!(f, "no client handshake traffic secret for Finished")
            }
            Self::NoHandshakeSecretForApplicationKeyDerivation => {
                write!(f, "no handshake secret for application key derivation")
            }
            Self::NoActiveKeyExchange => write!(f, "no active key exchange"),
            Self::NoCurrentAppSendKeysForKeyUpdate => {
                write!(f, "no current app send keys for KeyUpdate")
            }
            Self::NoCurrentAppRecvKeysForKeyUpdate => {
                write!(f, "no current app recv keys for KeyUpdate")
            }
            Self::ExporterMasterSecretNotDerived => {
                write!(f, "exporter master secret not yet derived")
            }
            Self::ExtendedMasterSecretSessionHashMissing => {
                write!(
                    f,
                    "extended master secret negotiated but session hash not captured"
                )
            }
        }
    }
}

impl fmt::Display for CryptoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoSupportedKeyExchangeGroups => write!(f, "no supported key exchange groups"),
            Self::NoDtls12KeyExchangeGroupsConfigured => {
                write!(f, "no DTLS 1.2 key exchange groups configured")
            }
            Self::Epoch0SequenceNumberExhausted => {
                write!(f, "epoch 0 sequence number exhausted")
            }
            Self::SendSequenceNumberExhausted { epoch } => {
                write!(f, "send sequence number exhausted for epoch {epoch}")
            }
            Self::SendKeysNotAvailable { epoch } => {
                write!(f, "send keys not available for epoch {epoch}")
            }
            Self::RecvKeysNotAvailable { epoch } => {
                write!(f, "recv keys not available for epoch {epoch}")
            }
            Self::KeyExchangeGroupNotFound(group) => {
                write!(f, "key exchange group not found: {group:?}")
            }
            Self::KeyExchangeNotInitialized => write!(f, "key exchange not initialized"),
            Self::UnsupportedKeyExchangeGroup(group) => {
                write!(f, "unsupported key exchange group: {group:?}")
            }
            Self::UnsupportedEcdheNamedGroup(group) => {
                write!(f, "unsupported ECDHE named group: {group:?}")
            }
            Self::UnsupportedCipherSuite(suite) => {
                write!(f, "unsupported cipher suite: {suite:?}")
            }
            Self::UnsupportedHmacHash(hash) => {
                write!(f, "unsupported HMAC hash algorithm: {hash:?}")
            }
            Self::UnsupportedSignatureAlgorithm(sig) => {
                write!(f, "unsupported signature algorithm: {sig:?}")
            }
            Self::SignatureAlgorithmNotOfferedByClient => {
                write!(f, "signature algorithm not offered by client")
            }
            Self::SignatureAlgorithmMismatch { expected, actual } => {
                write!(
                    f,
                    "signature algorithm mismatch: {actual:?} != {expected:?}"
                )
            }
            Self::UnsupportedSignaturePair { signature, hash } => {
                write!(f, "unsupported signature algorithm: {signature:?}/{hash:?}")
            }
            Self::UnsupportedSignatureVerification {
                signature,
                hash,
                group,
            } => write!(
                f,
                "unsupported signature verification: {signature:?} + {hash:?} + {group:?}"
            ),
            Self::SignatureVerificationFailed {
                signature,
                hash,
                group,
            } => write!(
                f,
                "signature verification failed: {signature:?} + {hash:?} + {group:?}"
            ),
            Self::UnsupportedPublicKeyAlgorithm => write!(f, "unsupported public key algorithm"),
            Self::UnsupportedEcCurve => write!(f, "unsupported EC curve"),
            Self::CertificateParseFailed => write!(f, "failed to parse certificate"),
            Self::MissingEcCurveParameter => {
                write!(f, "missing EC curve parameter in certificate")
            }
            Self::InvalidEcCurveParameter => {
                write!(f, "invalid EC curve parameter in certificate")
            }
            Self::InvalidSubjectPublicKey => {
                write!(f, "invalid EC subject_public_key bitstring")
            }
            Self::InvalidSignatureFormat => write!(f, "invalid signature format"),
            Self::InvalidPublicKey(group) => write!(f, "invalid {group:?} public key"),
            Self::InvalidPrivateKey => {
                write!(f, "failed to parse private key in any supported format")
            }
            Self::SigningKeyHashMismatch {
                key_hash,
                requested,
            } => write!(
                f,
                "signing key is locked to {key_hash:?} but {requested:?} was requested"
            ),
            Self::SigningKeyUnsupportedHash { group, hash } => {
                write!(f, "{group:?} key does not support hash algorithm {hash:?}")
            }
            Self::InvalidAesGcmKeySize { actual } => {
                write!(f, "invalid key size for AES-GCM: {actual}")
            }
            Self::InvalidChacha20Poly1305KeySize { actual } => {
                write!(f, "invalid key size for CHACHA20-POLY1305: {actual}")
            }
            Self::InvalidAes128Ccm8KeySize { actual } => {
                write!(f, "invalid key size for AES-128-CCM-8: {actual}")
            }
            Self::InvalidNonce => write!(f, "invalid nonce"),
            Self::CiphertextTooShort { minimum, actual } => {
                write!(f, "ciphertext too short: got {actual}, minimum {minimum}")
            }
            Self::HkdfOutputTooLong => write!(f, "HKDF output too long"),
            Self::HkdfLabelTooLong => write!(f, "label too long for HKDF-Expand-Label"),
            Self::HkdfContextTooLong => write!(f, "context too long for HKDF-Expand-Label"),
            Self::HkdfOutputLengthTooLarge => {
                write!(f, "output length too large for HKDF-Expand-Label")
            }
            Self::InvalidVerifyDataLength => write!(f, "invalid verify data length"),
            Self::VerifyDataTooLong => write!(f, "verify data too long"),
            Self::MasterSecretTooLong => write!(f, "master secret too long"),
            Self::KeyingMaterialTooLong => write!(f, "keying material too long"),
            Self::PreMasterSecretNotAvailable => write!(f, "pre-master secret not available"),
            Self::MasterSecretNotAvailable => write!(f, "master secret not available"),
            Self::ClientRandomNotAvailable => write!(f, "client random not available"),
            Self::ServerRandomNotAvailable => write!(f, "server random not available"),
            Self::ClientCipherNotInitialized => write!(f, "client cipher not initialized"),
            Self::ServerCipherNotInitialized => write!(f, "server cipher not initialized"),
            Self::WriteIvNotAvailable { is_client } => {
                let side = if *is_client { "client" } else { "server" };
                write!(f, "{side} write IV not available")
            }
            Self::UnsupportedDtls12RecordIvLen { len, suite } => {
                write!(f, "unsupported DTLS 1.2 record_iv_len={len} for {suite:?}")
            }
            Self::NoPrivateKeyConfigured => write!(f, "no private key configured"),
            Self::PskNotSet => write!(f, "PSK not set"),
            Self::ExporterMasterSecretNotDerived => {
                write!(f, "exporter master secret not yet derived")
            }
            Self::OperationFailed(op) => write!(f, "{op} failed"),
        }
    }
}

impl fmt::Display for CryptoOperation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            Self::CreateCipher => "cipher creation",
            Self::Encrypt => "encryption",
            Self::Decrypt => "decryption",
            Self::Sign => "signing",
            Self::VerifySignature => "signature verification",
            Self::LoadPrivateKey => "private key loading",
            Self::StartKeyExchange => "key exchange start",
            Self::CompleteKeyExchange => "key exchange completion",
            Self::GenerateEphemeralKey => "ephemeral key generation",
            Self::ComputePublicKey => "public key computation",
            Self::FillRandom => "random generation",
            Self::ComputeHmac => "HMAC computation",
            Self::Prf => "PRF",
            Self::HkdfExtract => "HKDF extract",
            Self::HkdfExpand => "HKDF expand",
            Self::HkdfExpandLabel => "HKDF expand label",
            Self::DeriveEarlySecret => "early secret derivation",
            Self::DeriveDerivedSecret => "derived secret derivation",
            Self::DeriveHandshakeSecret => "handshake secret derivation",
            Self::DeriveTrafficSecret => "traffic secret derivation",
            Self::DeriveMasterSecret => "master secret derivation",
            Self::DeriveExporterMasterSecret => "exporter master secret derivation",
            Self::DeriveNextTrafficSecret => "next traffic secret derivation",
            Self::DeriveKey => "key derivation",
            Self::DeriveIv => "IV derivation",
            Self::DeriveSequenceNumberKey => "sequence number key derivation",
            Self::DeriveFinishedKey => "Finished key derivation",
            Self::ComputeVerifyData => "verify data computation",
            Self::VerifyData => "verify data verification",
            Self::ComputePskPreMasterSecret => "PSK pre-master secret computation",
            Self::ComputeCookie => "cookie computation",
            Self::ExtractSrtpKeyingMaterial => "SRTP keying material extraction",
            Self::EncodeKey => "key encoding",
            Self::DecodeKey => "key decoding",
        };
        write!(f, "{text}")
    }
}

impl fmt::Display for CertificateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoServerCertificateReceived => write!(f, "no server certificate received"),
            Self::NoClientCertificateReceived => write!(f, "no client certificate received"),
            Self::NoClientCertificateForVerification => {
                write!(f, "no client certificate for verification")
            }
            Self::NoServerCertificateForVerification => {
                write!(f, "no server certificate for verification")
            }
            Self::ServerCertificateContextMustBeEmpty => {
                write!(f, "server certificate context must be empty")
            }
            Self::ClientCertificateContextMustBeEmpty => {
                write!(f, "client certificate context must be empty")
            }
            Self::UnsupportedHashAlgorithm(hash) => {
                write!(f, "unsupported hash algorithm: {hash:?}")
            }
            Self::ParseFailed => write!(f, "failed to parse certificate"),
            Self::MissingEcCurveParameter => {
                write!(f, "missing EC curve parameter in certificate")
            }
            Self::InvalidEcCurveParameter => {
                write!(f, "invalid EC curve parameter in certificate")
            }
            Self::UnsupportedEcCurve => write!(f, "unsupported EC curve"),
            Self::InvalidSubjectPublicKey => {
                write!(f, "invalid EC subject_public_key bitstring")
            }
        }
    }
}

impl fmt::Display for SecurityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedHelloVerifyRequestVersion(version) => {
                write!(
                    f,
                    "unsupported DTLS version in HelloVerifyRequest: {version:?}"
                )
            }
            Self::UnsupportedServerVersion(version) => {
                write!(f, "unsupported DTLS version from server: {version:?}")
            }
            Self::UnsupportedClientVersion(version) => {
                write!(f, "unsupported DTLS version from client: {version:?}")
            }
            Self::UnsupportedServerCompression(compression) => {
                write!(
                    f,
                    "unsupported compression method from server: {compression:?}"
                )
            }
            Self::UnsupportedClientCompression => {
                write!(f, "client did not offer null compression")
            }
            Self::UnsupportedKeyExchangeAlgorithm => {
                write!(f, "unsupported key exchange algorithm")
            }
            Self::ServerSelectedUnknownCipherSuite => {
                write!(f, "server selected unknown cipher suite")
            }
            Self::ServerSelectedIncompatibleCipherSuite(suite) => {
                write!(f, "server selected incompatible cipher suite: {suite:?}")
            }
            Self::ServerSelectedDisallowedCipherSuite(suite) => {
                write!(f, "server selected disallowed cipher suite: {suite:?}")
            }
            Self::ServerSelectedDisallowedDtls13CipherSuite(suite) => {
                write!(f, "server selected disallowed cipher suite: {suite:?}")
            }
            Self::ExtendedMasterSecretNotNegotiated => {
                write!(f, "extended master secret not negotiated")
            }
            Self::NoMutuallyAcceptableCipherSuite => {
                write!(f, "no mutually acceptable cipher suite")
            }
            Self::ClientHelloLegacyVersionNotDtls12 => {
                write!(f, "ClientHello legacy_version must be DTLS 1.2")
            }
            Self::ServerHelloLegacyVersionNotDtls12 => {
                write!(f, "ServerHello legacy_version must be DTLS 1.2")
            }
            Self::ClientHelloMissingDtls13SupportedVersions => {
                write!(f, "ClientHello missing DTLS 1.3 supported_versions")
            }
            Self::ClientHelloMustOfferNullCompression => {
                write!(f, "ClientHello must offer null compression")
            }
            Self::InvalidCookieInClientHello => write!(f, "invalid cookie in ClientHello"),
            Self::InvalidLegacyCookieInClientHello => {
                write!(f, "ClientHello legacy_cookie must be empty")
            }
            Self::CannotSendSecondHelloRetryRequest => {
                write!(f, "cannot send second HelloRetryRequest")
            }
            Self::UnexpectedSecondHelloRetryRequest => {
                write!(f, "received second HelloRetryRequest")
            }
            Self::NoCommonCipherSuite => write!(f, "no common cipher suite found"),
            Self::NoCommonKeyExchangeGroup => write!(f, "no common key exchange group"),
            Self::HrrSelectedDisallowedCipherSuite => {
                write!(f, "HRR selected disallowed cipher suite")
            }
            Self::HrrDidNotSelectDtls13 => write!(f, "HRR did not select DTLS 1.3"),
            Self::ServerHelloCompressionMustBeNull => {
                write!(f, "ServerHello compression must be null")
            }
            Self::ServerDidNotNegotiateDtls13 => {
                write!(f, "server did not negotiate DTLS 1.3")
            }
            Self::ServerMissingKeyShare => write!(f, "server missing key_share"),
            Self::ServerKeyShareGroupMismatch { expected, actual } => {
                write!(
                    f,
                    "server key_share group mismatch: expected {expected:?}, actual {actual:?}"
                )
            }
            Self::SignatureTooLarge => write!(f, "signature too large"),
            Self::SignatureSchemeNotOffered(scheme) => {
                write!(f, "signature scheme {scheme:?} was not offered")
            }
            Self::SignatureAlgorithmMismatch { expected, actual } => {
                write!(
                    f,
                    "signature algorithm mismatch: expected {expected:?}, got {actual:?}"
                )
            }
            Self::UnsupportedSignatureScheme(scheme) => {
                write!(f, "unsupported signature scheme: {scheme:?}")
            }
            Self::SignatureSchemeCertificateCurveMismatch {
                scheme,
                expected,
                actual,
            } => write!(
                f,
                "signature scheme {scheme:?} requires {expected:?} but certificate uses {actual:?}"
            ),
            Self::ServerFinishedVerificationFailed => {
                write!(f, "server Finished verification failed")
            }
            Self::ClientFinishedVerificationFailed => {
                write!(f, "client Finished verification failed")
            }
            Self::FatalAlert { description } => {
                write!(f, "received fatal alert: description={description}")
            }
        }
    }
}

impl fmt::Display for PskError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoPskResolverConfigured => write!(f, "no PSK resolver configured"),
            Self::NoPskIdentityConfigured => write!(f, "no PSK identity configured"),
            Self::ResolverReturnedNoKey => write!(f, "PSK resolver returned no key"),
        }
    }
}

impl fmt::Display for TimeoutError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::HybridClientHello => write!(f, "hybrid ClientHello"),
            Self::Connect => write!(f, "connect"),
            Self::Handshake => write!(f, "handshake"),
        }
    }
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MtuTooSmall { mtu, minimum } => {
                write!(f, "MTU {mtu} is too small (minimum {minimum})")
            }
            Self::AeadEncryptionLimitTooSmall => {
                write!(f, "aead_encryption_limit must be at least 1")
            }
            Self::InvalidHandshakeTimeout => {
                write!(f, "handshake_timeout must be nonzero and at most 3650 days")
            }
            Self::InvalidFlightStartRto => {
                write!(f, "flight_start_rto must be nonzero and at most 3650 days")
            }
            Self::NoCipherSuitesAfterFiltering => write!(
                f,
                concat!(
                    "no cipher suites remain after filtering; at least one DTLS 1.2 or ",
                    "DTLS 1.3 cipher suite must be available"
                )
            ),
            Self::PskConfiguredWithoutPskCipherSuite => write!(
                f,
                "PSK is configured but no PSK cipher suite remains after filtering DTLS 1.2 suites"
            ),
            Self::NoDtls12KeyExchangeGroupsAfterFiltering => write!(
                f,
                concat!(
                    "DTLS 1.2 cipher suites are enabled but no compatible key exchange ",
                    "groups remain after filtering"
                )
            ),
            Self::NoDtls13KeyExchangeGroupsAfterFiltering => write!(
                f,
                "DTLS 1.3 cipher suites are enabled but no key exchange groups remain after filtering"
            ),
            Self::CryptoProvider(err) => write!(f, "crypto provider validation failed: {err}"),
        }
    }
}

impl fmt::Display for CryptoProviderValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoCipherSuites => {
                write!(f, "CryptoProvider has no cipher suites supported by dimpl")
            }
            Self::EcdhCipherSuitesWithoutKeyExchangeGroups => write!(
                f,
                "CryptoProvider has ECDH cipher suites but no supported key exchange groups"
            ),
            Self::NoDtls13CipherSuites => {
                write!(f, "CryptoProvider has no DTLS 1.3 cipher suites")
            }
            Self::MissingHashTestVector(hash) => {
                write!(f, "no expected hash data for hash algorithm: {hash:?}")
            }
            Self::HashProviderIncorrect(hash) => {
                write!(f, "hash provider {hash:?} produced incorrect result")
            }
            Self::PrfFailed { hash, source } => write!(f, "PRF failed for {hash:?}: {source}"),
            Self::MissingPrfTestVector(hash) => {
                write!(f, "no expected PRF data for hash algorithm: {hash:?}")
            }
            Self::PrfIncorrect(hash) => write!(f, "PRF {hash:?} produced incorrect result"),
            Self::NoSignatureValidationVector { hash, signature } => {
                write!(f, "no validation test vectors for {hash:?} + {signature:?}")
            }
            Self::SignatureVerificationFailed {
                hash,
                signature,
                source,
            } => write!(
                f,
                "signature verification failed for {hash:?} + {signature:?}: {source}"
            ),
            Self::HkdfFailed { suite, source } => {
                write!(f, "HKDF failed for DTLS 1.3 suite {suite:?}: {source}")
            }
            Self::HkdfEmptyOutput(suite) => {
                write!(f, "HKDF returned empty output for {suite:?}")
            }
            Self::NoAeadTestVector(suite) => {
                write!(f, "no AEAD test vector for DTLS 1.3 suite {suite:?}")
            }
            Self::AeadCreateFailed { suite, source } => {
                write!(f, "failed to create cipher for {suite:?}: {source}")
            }
            Self::AeadEncryptFailed { suite, source } => {
                write!(f, "AEAD encrypt failed for {suite:?}: {source}")
            }
            Self::AeadEncryptWrongOutput(suite) => {
                write!(f, "AEAD encrypt produced wrong output for {suite:?}")
            }
            Self::AeadDecryptFailed { suite, source } => {
                write!(f, "AEAD decrypt failed for {suite:?}: {source}")
            }
            Self::AeadDecryptWrongOutput(suite) => {
                write!(f, "AEAD decrypt produced wrong output for {suite:?}")
            }
            Self::NoRecordNumberEncryptionTestVector(suite) => {
                write!(f, "no encrypt_sn test vector for DTLS 1.3 suite {suite:?}")
            }
            Self::RecordNumberEncryptionWrongMask(suite) => {
                write!(f, "encrypt_sn produced wrong mask for {suite:?}")
            }
            Self::KeyExchangeStartFailed { group, source } => {
                write!(f, "key exchange start failed for {group:?}: {source}")
            }
            Self::KeyExchangeCompleteFailed { group, source } => {
                write!(f, "key exchange complete failed for {group:?}: {source}")
            }
            Self::KeyExchangeMismatchedSharedSecret(group) => {
                write!(f, "key exchange produced different secrets for {group:?}")
            }
            Self::HmacFailed(source) => write!(f, "HMAC provider failed: {source}"),
            Self::HmacIncorrect => {
                write!(f, "HMAC provider produced incorrect result for HMAC-SHA256")
            }
        }
    }
}
