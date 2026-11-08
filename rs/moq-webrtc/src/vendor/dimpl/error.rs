//! Public error type returned by the high-level DTLS API.

use std::fmt;
use tracing::debug;

use crate::vendor::dimpl::dtls12::message::Dtls12CipherSuite;
use crate::vendor::dimpl::types::CompressionMethod;
use crate::vendor::dimpl::types::HashAlgorithm;
use crate::vendor::dimpl::types::NamedGroup;
use crate::vendor::dimpl::types::ProtocolVersion;
use crate::vendor::dimpl::types::SignatureAlgorithm;

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
}

/// Fine-grained reason for an [`Error::UnexpectedMessage`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum UnexpectedMessageError {
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
	/// Extended master secret was negotiated, but the session hash was not captured.
	ExtendedMasterSecretSessionHashMissing,
}

/// Fine-grained reason for an [`Error::CryptoError`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum CryptoError {
	/// DTLS 1.2 needs a key exchange group but none is configured.
	NoDtls12KeyExchangeGroupsConfigured,
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
	/// Generate an ephemeral key.
	GenerateEphemeralKey,
	/// Compute a public key.
	ComputePublicKey,
	/// Fill a buffer with random bytes.
	FillRandom,
	/// Compute a DTLS cookie.
	ComputeCookie,
	/// Encode a key.
	EncodeKey,
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
	/// The certificate operation needs an unsupported hash algorithm.
	UnsupportedHashAlgorithm(HashAlgorithm),
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
	/// DTLS 1.2 extended master secret was required but not negotiated.
	ExtendedMasterSecretNotNegotiated,
	/// No mutually acceptable cipher suite was found.
	NoMutuallyAcceptableCipherSuite,
	/// No common key exchange group was found.
	NoCommonKeyExchangeGroup,
	/// A signature algorithm did not match the expected algorithm.
	SignatureAlgorithmMismatch {
		/// The expected signature algorithm.
		expected: SignatureAlgorithm,
		/// The actual signature algorithm.
		actual: SignatureAlgorithm,
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
			Error::IncompleteServerHello => write!(f, "incomplete ServerHello"),
			Error::Timeout(err) => write!(f, "timeout: {err}"),
			Error::ConfigError(err) => write!(f, "config error: {err}"),
			Error::RenegotiationAttempt => write!(f, "peer attempted renegotiation"),
			Error::ConnectionClosed => write!(f, "connection closed"),
		}
	}
}

impl fmt::Display for UnexpectedMessageError {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::UnexpectedEpoch => {
				write!(f, "pending record in unexpected epoch at handshake completion")
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
			Self::ExtendedMasterSecretSessionHashMissing => {
				write!(f, "extended master secret negotiated but session hash not captured")
			}
		}
	}
}

impl fmt::Display for CryptoError {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::NoDtls12KeyExchangeGroupsConfigured => {
				write!(f, "no DTLS 1.2 key exchange groups configured")
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
				write!(f, "signature algorithm mismatch: {actual:?} != {expected:?}")
			}
			Self::UnsupportedSignatureVerification { signature, hash, group } => write!(
				f,
				"unsupported signature verification: {signature:?} + {hash:?} + {group:?}"
			),
			Self::SignatureVerificationFailed { signature, hash, group } => {
				write!(f, "signature verification failed: {signature:?} + {hash:?} + {group:?}")
			}
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
			Self::InvalidPublicKey(group) => write!(f, "invalid {group:?} public key"),
			Self::InvalidPrivateKey => {
				write!(f, "failed to parse private key in any supported format")
			}
			Self::SigningKeyHashMismatch { key_hash, requested } => write!(
				f,
				"signing key is locked to {key_hash:?} but {requested:?} was requested"
			),
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
			Self::GenerateEphemeralKey => "ephemeral key generation",
			Self::ComputePublicKey => "public key computation",
			Self::FillRandom => "random generation",
			Self::ComputeCookie => "cookie computation",
			Self::EncodeKey => "key encoding",
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
			Self::UnsupportedHashAlgorithm(hash) => {
				write!(f, "unsupported hash algorithm: {hash:?}")
			}
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
				write!(f, "unsupported DTLS version in HelloVerifyRequest: {version:?}")
			}
			Self::UnsupportedServerVersion(version) => {
				write!(f, "unsupported DTLS version from server: {version:?}")
			}
			Self::UnsupportedClientVersion(version) => {
				write!(f, "unsupported DTLS version from client: {version:?}")
			}
			Self::UnsupportedServerCompression(compression) => {
				write!(f, "unsupported compression method from server: {compression:?}")
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
			Self::ExtendedMasterSecretNotNegotiated => {
				write!(f, "extended master secret not negotiated")
			}
			Self::NoMutuallyAcceptableCipherSuite => {
				write!(f, "no mutually acceptable cipher suite")
			}
			Self::NoCommonKeyExchangeGroup => write!(f, "no common key exchange group"),
			Self::SignatureAlgorithmMismatch { expected, actual } => {
				write!(f, "signature algorithm mismatch: expected {expected:?}, got {actual:?}")
			}
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
			Self::InvalidHandshakeTimeout => {
				write!(f, "handshake_timeout must be nonzero and at most 3650 days")
			}
			Self::InvalidFlightStartRto => {
				write!(f, "flight_start_rto must be nonzero and at most 3650 days")
			}
			Self::NoCipherSuitesAfterFiltering => write!(
				f,
				"no cipher suites remain after filtering; at least one DTLS 1.2 cipher suite must be available"
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
