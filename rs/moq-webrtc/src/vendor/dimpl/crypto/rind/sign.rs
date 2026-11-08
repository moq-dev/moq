use std::fmt;

use crate::vendor::dimpl::crypto::{
	Buf, HashAlgorithm, KeyProvider, NamedGroup, OID_P256, OID_P384, SignatureAlgorithm, SignatureVerifier, SigningKey,
	check_verify_scheme,
};
use crate::vendor::dimpl::{CryptoError, CryptoOperation};
use rind::signature::{EcdsaKeyPair, EcdsaSigningAlgorithm, PublicKey, VerificationAlgorithm, sign, verify};
use x509_cert::Certificate;
use x509_cert::der::Decode;
use x509_cert::spki::ObjectIdentifier;

const ID_EC_PUBLIC_KEY: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.10045.2.1");

/// Each curve, and the one hash its key signs with.
///
/// A key is bound to its hash when loaded, so each curve signs with its own
/// hash only.
const CURVES: [(NamedGroup, HashAlgorithm, &EcdsaSigningAlgorithm); 2] = [
	(
		NamedGroup::Secp256r1,
		HashAlgorithm::SHA256,
		&sign::ECDSA_P256_SHA256_ASN1,
	),
	(
		NamedGroup::Secp384r1,
		HashAlgorithm::SHA384,
		&sign::ECDSA_P384_SHA384_ASN1,
	),
];

#[derive(Debug)]
pub(crate) struct Keys;

pub(crate) static KEYS: Keys = Keys;

impl KeyProvider for Keys {
	fn load_private_key(&self, der: &[u8]) -> Result<Box<dyn SigningKey>, CryptoError> {
		type Load = fn(&'static EcdsaSigningAlgorithm, &[u8]) -> Result<EcdsaKeyPair, rind::Error>;
		let load = |load: Load| {
			CURVES.into_iter().find_map(|(group, hash, algorithm)| {
				let key = load(algorithm, der).ok()?;
				Some(Box::new(Key { key, group, hash }) as Box<dyn SigningKey>)
			})
		};
		load(EcdsaKeyPair::from_pkcs8)
			.or_else(|| load(EcdsaKeyPair::from_sec1))
			.ok_or(CryptoError::InvalidPrivateKey)
	}
}

struct Key {
	key: EcdsaKeyPair,
	group: NamedGroup,
	hash: HashAlgorithm,
}

impl fmt::Debug for Key {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("Key")
			.field("group", &self.group)
			.field("hash", &self.hash)
			.finish_non_exhaustive()
	}
}

impl SigningKey for Key {
	fn sign(&mut self, data: &[u8], hash: HashAlgorithm, out: &mut Buf) -> Result<(), CryptoError> {
		if hash != self.hash {
			return Err(CryptoError::SigningKeyHashMismatch {
				key_hash: self.hash,
				requested: hash,
			});
		}
		let signature = self
			.key
			.sign(data)
			.map_err(|_| CryptoError::OperationFailed(CryptoOperation::Sign))?;
		out.clear();
		out.extend_from_slice(signature.as_ref());
		Ok(())
	}

	fn algorithm(&self) -> SignatureAlgorithm {
		SignatureAlgorithm::ECDSA
	}

	fn hash_algorithm(&self) -> HashAlgorithm {
		self.hash
	}

	fn supported_hash_algorithms(&self) -> &[HashAlgorithm] {
		core::slice::from_ref(&self.hash)
	}
}

#[derive(Debug)]
pub(crate) struct Verifier;

pub(crate) static VERIFIER: Verifier = Verifier;

impl SignatureVerifier for Verifier {
	fn verify_signature(
		&self,
		cert_der: &[u8],
		data: &[u8],
		signature: &[u8],
		hash: HashAlgorithm,
		algorithm: SignatureAlgorithm,
	) -> Result<(), CryptoError> {
		if algorithm != SignatureAlgorithm::ECDSA {
			return Err(CryptoError::UnsupportedSignatureAlgorithm(algorithm));
		}

		let cert = Certificate::from_der(cert_der).map_err(|_| CryptoError::CertificateParseFailed)?;
		let spki = cert.tbs_certificate().subject_public_key_info();
		if spki.algorithm.oid != ID_EC_PUBLIC_KEY {
			return Err(CryptoError::UnsupportedPublicKeyAlgorithm);
		}
		let curve: ObjectIdentifier = spki
			.algorithm
			.parameters
			.as_ref()
			.ok_or(CryptoError::MissingEcCurveParameter)?
			.decode_as()
			.map_err(|_| CryptoError::InvalidEcCurveParameter)?;
		let group = match curve {
			OID_P256 => NamedGroup::Secp256r1,
			OID_P384 => NamedGroup::Secp384r1,
			_ => return Err(CryptoError::UnsupportedEcCurve),
		};
		check_verify_scheme(algorithm, hash, group)?;

		// DTLS 1.2 may pair either curve with either hash.
		let verification: &'static VerificationAlgorithm = match (group, hash) {
			(NamedGroup::Secp256r1, HashAlgorithm::SHA256) => &verify::ECDSA_P256_SHA256_ASN1,
			(NamedGroup::Secp256r1, HashAlgorithm::SHA384) => &verify::ECDSA_P256_SHA384_ASN1,
			(NamedGroup::Secp384r1, HashAlgorithm::SHA256) => &verify::ECDSA_P384_SHA256_ASN1,
			(NamedGroup::Secp384r1, HashAlgorithm::SHA384) => &verify::ECDSA_P384_SHA384_ASN1,
			_ => {
				return Err(CryptoError::UnsupportedSignatureVerification {
					signature: algorithm,
					hash,
					group,
				});
			}
		};

		let point = spki
			.subject_public_key
			.as_bytes()
			.ok_or(CryptoError::InvalidSubjectPublicKey)?;
		PublicKey::new(verification, point)
			.map_err(|_| CryptoError::InvalidPublicKey(group))?
			.verify(data, signature)
			.map_err(|_| CryptoError::SignatureVerificationFailed {
				signature: algorithm,
				hash,
				group,
			})
	}
}
