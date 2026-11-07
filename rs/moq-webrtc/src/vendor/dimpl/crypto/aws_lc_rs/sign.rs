//! Signing and key loading implementations using aws-lc-rs.

use std::str;

use aws_lc_rs::signature::ECDSA_P384_SHA384_ASN1_SIGNING;
use aws_lc_rs::signature::{ECDSA_P256_SHA256_ASN1, ECDSA_P256_SHA384_ASN1};
use aws_lc_rs::signature::{ECDSA_P256_SHA256_ASN1_SIGNING, UnparsedPublicKey};
use aws_lc_rs::signature::{ECDSA_P384_SHA256_ASN1, ECDSA_P384_SHA384_ASN1};
use aws_lc_rs::signature::{EcdsaKeyPair, EcdsaSigningAlgorithm, EcdsaVerificationAlgorithm};
use der::{Decode, Encode};
use spki::ObjectIdentifier;
use x509_cert::Certificate as X509Certificate;

use super::super::{KeyProvider, SignatureVerifier, SigningKey, check_verify_scheme};
use super::super::{OID_P256, OID_P384};
use crate::buffer::Buf;
use crate::types::{HashAlgorithm, NamedGroup, SignatureAlgorithm};
use crate::{CryptoError, CryptoOperation};

/// ECDSA signing key implementation.
struct EcdsaSigningKey {
    key_pair: EcdsaKeyPair,
    signing_algorithm: &'static EcdsaSigningAlgorithm,
}

impl std::fmt::Debug for EcdsaSigningKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EcdsaSigningKey")
            .field("signing_algorithm", &self.signing_algorithm)
            .finish()
    }
}

impl SigningKey for EcdsaSigningKey {
    fn sign(
        &mut self,
        data: &[u8],
        hash_alg: HashAlgorithm,
        buf: &mut Buf,
    ) -> Result<(), CryptoError> {
        let key_hash = self.hash_algorithm();
        if hash_alg != key_hash {
            return Err(CryptoError::SigningKeyHashMismatch {
                key_hash,
                requested: hash_alg,
            });
        }
        let rng = aws_lc_rs::rand::SystemRandom::new();
        let signature = self
            .key_pair
            .sign(&rng, data)
            .map_err(|_| CryptoError::OperationFailed(CryptoOperation::Sign))?;
        buf.clear();
        buf.extend_from_slice(signature.as_ref());
        Ok(())
    }

    fn algorithm(&self) -> SignatureAlgorithm {
        SignatureAlgorithm::ECDSA
    }

    fn hash_algorithm(&self) -> HashAlgorithm {
        if self.signing_algorithm == &ECDSA_P256_SHA256_ASN1_SIGNING {
            HashAlgorithm::SHA256
        } else if self.signing_algorithm == &ECDSA_P384_SHA384_ASN1_SIGNING {
            HashAlgorithm::SHA384
        } else {
            panic!("Unsupported signing algorithm")
        }
    }

    fn supported_hash_algorithms(&self) -> &[HashAlgorithm] {
        // aws-lc-rs locks the hash at key-load time; only one is supported.
        if self.signing_algorithm == &ECDSA_P256_SHA256_ASN1_SIGNING {
            &[HashAlgorithm::SHA256]
        } else if self.signing_algorithm == &ECDSA_P384_SHA384_ASN1_SIGNING {
            &[HashAlgorithm::SHA384]
        } else {
            panic!("Unsupported signing algorithm")
        }
    }
}

/// Key provider implementation.
#[derive(Debug)]
pub(super) struct AwsLcKeyProvider;

impl KeyProvider for AwsLcKeyProvider {
    fn load_private_key(&self, key_der: &[u8]) -> Result<Box<dyn SigningKey>, CryptoError> {
        // Try PKCS#8 DER format first (most common)
        if let Ok(key_pair) = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, key_der) {
            return Ok(Box::new(EcdsaSigningKey {
                key_pair,
                signing_algorithm: &ECDSA_P256_SHA256_ASN1_SIGNING,
            }));
        }
        if let Ok(key_pair) = EcdsaKeyPair::from_pkcs8(&ECDSA_P384_SHA384_ASN1_SIGNING, key_der) {
            return Ok(Box::new(EcdsaSigningKey {
                key_pair,
                signing_algorithm: &ECDSA_P384_SHA384_ASN1_SIGNING,
            }));
        }

        // Try parsing as SEC1 DER format (OpenSSL EC private key format)
        if let Ok(ec_key) = sec1::EcPrivateKey::try_from(key_der) {
            let private_key_len = ec_key.private_key.len();

            let curve_oid = if let Some(params) = &ec_key.parameters {
                match params {
                    sec1::EcParameters::NamedCurve(oid) => Some(*oid),
                }
            } else if private_key_len == 32 {
                Some(ObjectIdentifier::new_unwrap("1.2.840.10045.3.1.7")) // P-256
            } else if private_key_len == 48 {
                Some(ObjectIdentifier::new_unwrap("1.3.132.0.34")) // P-384
            } else {
                None
            };

            if let Some(curve_oid) = curve_oid {
                let ec_alg_oid = ObjectIdentifier::new_unwrap("1.2.840.10045.2.1");
                let curve_params_der = curve_oid
                    .to_der()
                    .map_err(|_| CryptoError::OperationFailed(CryptoOperation::EncodeKey))?;
                let curve_params_any = der::asn1::AnyRef::try_from(curve_params_der.as_slice())
                    .map_err(|_| CryptoError::OperationFailed(CryptoOperation::EncodeKey))?;

                let algorithm = spki::AlgorithmIdentifierRef {
                    oid: ec_alg_oid,
                    parameters: Some(curve_params_any),
                };

                let private_key = der::asn1::OctetStringRef::new(key_der)
                    .map_err(|_| CryptoError::OperationFailed(CryptoOperation::EncodeKey))?;
                let pkcs8 = pkcs8::PrivateKeyInfoRef {
                    algorithm,
                    private_key,
                    public_key: None,
                };

                let pkcs8_der = pkcs8
                    .to_der()
                    .map_err(|_| CryptoError::OperationFailed(CryptoOperation::EncodeKey))?;

                let p256_curve = ObjectIdentifier::new_unwrap("1.2.840.10045.3.1.7");
                if curve_oid == p256_curve
                    && let Ok(key_pair) =
                        EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, &pkcs8_der)
                {
                    return Ok(Box::new(EcdsaSigningKey {
                        key_pair,
                        signing_algorithm: &ECDSA_P256_SHA256_ASN1_SIGNING,
                    }));
                }

                let p384_curve = ObjectIdentifier::new_unwrap("1.3.132.0.34");
                if curve_oid == p384_curve
                    && let Ok(key_pair) =
                        EcdsaKeyPair::from_pkcs8(&ECDSA_P384_SHA384_ASN1_SIGNING, &pkcs8_der)
                {
                    return Ok(Box::new(EcdsaSigningKey {
                        key_pair,
                        signing_algorithm: &ECDSA_P384_SHA384_ASN1_SIGNING,
                    }));
                }
            }
        }

        // Check if it's a PEM encoded key
        if let Ok(pem_str) = str::from_utf8(key_der)
            && pem_str.contains("-----BEGIN")
            && let Ok((_label, doc)) = pkcs8::Document::from_pem(pem_str)
        {
            return self.load_private_key(doc.as_bytes());
        }

        Err(CryptoError::InvalidPrivateKey)
    }
}

/// Signature verifier implementation.
#[derive(Debug)]
pub(super) struct AwsLcSignatureVerifier;

impl SignatureVerifier for AwsLcSignatureVerifier {
    fn verify_signature(
        &self,
        cert_der: &[u8],
        data: &[u8],
        signature: &[u8],
        hash_alg: HashAlgorithm,
        sig_alg: SignatureAlgorithm,
    ) -> Result<(), CryptoError> {
        if sig_alg != SignatureAlgorithm::ECDSA {
            return Err(CryptoError::UnsupportedSignatureAlgorithm(sig_alg));
        }

        let cert =
            X509Certificate::from_der(cert_der).map_err(|_| CryptoError::CertificateParseFailed)?;
        let spki = cert.tbs_certificate().subject_public_key_info();

        const OID_EC_PUBLIC_KEY: ObjectIdentifier =
            ObjectIdentifier::new_unwrap("1.2.840.10045.2.1");

        if spki.algorithm.oid != OID_EC_PUBLIC_KEY {
            return Err(CryptoError::UnsupportedPublicKeyAlgorithm);
        }

        let pubkey_bytes = spki
            .subject_public_key
            .as_bytes()
            .ok_or(CryptoError::InvalidSubjectPublicKey)?;

        let curve_oid: ObjectIdentifier = spki
            .algorithm
            .parameters
            .as_ref()
            .ok_or(CryptoError::MissingEcCurveParameter)?
            .decode_as()
            .map_err(|_| CryptoError::InvalidEcCurveParameter)?;

        let group = match curve_oid {
            OID_P256 => NamedGroup::Secp256r1,
            OID_P384 => NamedGroup::Secp384r1,
            _ => return Err(CryptoError::UnsupportedEcCurve),
        };

        check_verify_scheme(sig_alg, hash_alg, group)?;

        let algorithm: &EcdsaVerificationAlgorithm = match (group, hash_alg) {
            (NamedGroup::Secp256r1, HashAlgorithm::SHA256) => &ECDSA_P256_SHA256_ASN1,
            (NamedGroup::Secp256r1, HashAlgorithm::SHA384) => &ECDSA_P256_SHA384_ASN1,
            (NamedGroup::Secp384r1, HashAlgorithm::SHA256) => &ECDSA_P384_SHA256_ASN1,
            (NamedGroup::Secp384r1, HashAlgorithm::SHA384) => &ECDSA_P384_SHA384_ASN1,
            // unreachable: check_verify_scheme already validated
            _ => unreachable!(),
        };

        let public_key = UnparsedPublicKey::new(algorithm, pubkey_bytes);
        public_key
            .verify(data, signature)
            .map_err(|_| CryptoError::SignatureVerificationFailed {
                signature: sig_alg,
                hash: hash_alg,
                group,
            })
    }
}

/// Static instance of the key provider.
pub(super) static KEY_PROVIDER: AwsLcKeyProvider = AwsLcKeyProvider;

/// Static instance of the signature verifier.
pub(super) static SIGNATURE_VERIFIER: AwsLcSignatureVerifier = AwsLcSignatureVerifier;

#[cfg(all(test, feature = "rcgen"))]
mod tests {
    use super::*;
    use crate::certificate::generate_self_signed_certificate;

    #[test]
    fn invalid_signature_returns_structured_verification_error() {
        let cert = generate_self_signed_certificate().expect("generate cert");
        let mut key = KEY_PROVIDER
            .load_private_key(&cert.private_key)
            .expect("load private key");
        let data = b"signed data";
        let mut signature = Buf::new();
        key.sign(data, HashAlgorithm::SHA256, &mut signature)
            .expect("sign data");

        let last = signature.len() - 1;
        signature[last] ^= 0x01;

        let err = SIGNATURE_VERIFIER
            .verify_signature(
                &cert.certificate,
                data,
                &signature,
                HashAlgorithm::SHA256,
                SignatureAlgorithm::ECDSA,
            )
            .expect_err("corrupt signature should fail");

        assert_eq!(
            err,
            CryptoError::SignatureVerificationFailed {
                signature: SignatureAlgorithm::ECDSA,
                hash: HashAlgorithm::SHA256,
                group: NamedGroup::Secp256r1,
            }
        );
    }
}
