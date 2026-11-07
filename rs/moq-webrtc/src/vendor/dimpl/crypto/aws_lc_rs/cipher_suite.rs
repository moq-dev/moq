//! Cipher suite implementations using aws-lc-rs.

use aws_lc_rs::aead::quic::{self as aws_quic, HeaderProtectionKey};
use aws_lc_rs::aead::{AES_128_GCM, AES_256_GCM, CHACHA20_POLY1305, UnboundKey};
use aws_lc_rs::aead::{Aad as AwsAad, LessSafeKey, Nonce as AwsNonce};
use aws_lc_rs::cipher::{self as aws_cipher, AES_128, AES_256, EncryptingKey, UnboundCipherKey};

use super::super::{Cipher, SupportedDtls12CipherSuite, SupportedDtls13CipherSuite};
use crate::buffer::{Buf, TmpBuf};
use crate::crypto::{Aad, Nonce};
use crate::dtls12::message::Dtls12CipherSuite;
use crate::error::bounded_error_len;
use crate::types::{Dtls13CipherSuite, HashAlgorithm};
use crate::{CryptoError, CryptoOperation};

/// AES-GCM cipher implementation using aws-lc-rs.
struct AesGcm {
    key: LessSafeKey,
}

impl std::fmt::Debug for AesGcm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AesGcm").finish_non_exhaustive()
    }
}

impl AesGcm {
    fn new(key: &[u8]) -> Result<Self, CryptoError> {
        let algorithm = match key.len() {
            16 => &AES_128_GCM,
            32 => &AES_256_GCM,
            _ => {
                return Err(CryptoError::InvalidAesGcmKeySize {
                    actual: bounded_error_len(key.len()),
                });
            }
        };

        let unbound_key = UnboundKey::new(algorithm, key)
            .map_err(|_| CryptoError::OperationFailed(CryptoOperation::CreateCipher))?;

        Ok(AesGcm {
            key: LessSafeKey::new(unbound_key),
        })
    }
}

impl Cipher for AesGcm {
    fn encrypt(&mut self, plaintext: &mut Buf, aad: Aad, nonce: Nonce) -> Result<(), CryptoError> {
        let aws_nonce =
            AwsNonce::try_assume_unique_for_key(&nonce).map_err(|_| CryptoError::InvalidNonce)?;

        let aws_aad = AwsAad::from(&aad[..]);

        self.key
            .seal_in_place_append_tag(aws_nonce, aws_aad, plaintext)
            .map_err(|_| CryptoError::OperationFailed(CryptoOperation::Encrypt))?;

        Ok(())
    }

    fn decrypt(
        &mut self,
        ciphertext: &mut TmpBuf,
        aad: Aad,
        nonce: Nonce,
    ) -> Result<(), CryptoError> {
        if ciphertext.len() < 16 {
            return Err(CryptoError::CiphertextTooShort {
                minimum: 16,
                actual: ciphertext.len() as u8,
            });
        }

        let aws_nonce =
            AwsNonce::try_assume_unique_for_key(&nonce).map_err(|_| CryptoError::InvalidNonce)?;

        let aws_aad = AwsAad::from(&aad[..]);

        let plaintext = self
            .key
            .open_in_place(aws_nonce, aws_aad, ciphertext.as_mut())
            .map_err(|_| CryptoError::OperationFailed(CryptoOperation::Decrypt))?;

        let plaintext_len = plaintext.len();
        ciphertext.truncate(plaintext_len);

        Ok(())
    }
}

/// ChaCha20-Poly1305 cipher implementation using aws-lc-rs.
struct ChaCha20Poly1305Cipher {
    key: LessSafeKey,
}

impl std::fmt::Debug for ChaCha20Poly1305Cipher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChaCha20Poly1305Cipher")
            .finish_non_exhaustive()
    }
}

impl ChaCha20Poly1305Cipher {
    fn new(key: &[u8]) -> Result<Self, CryptoError> {
        // The UnboundKey::new call also validates length, but doesnt give us
        // a reasonable error message. This makes it equivalent to RustCrypto
        if key.len() != 32 {
            return Err(CryptoError::InvalidChacha20Poly1305KeySize {
                actual: bounded_error_len(key.len()),
            });
        }
        let unbound_key = UnboundKey::new(&CHACHA20_POLY1305, key)
            .map_err(|_| CryptoError::OperationFailed(CryptoOperation::CreateCipher))?;

        Ok(ChaCha20Poly1305Cipher {
            key: LessSafeKey::new(unbound_key),
        })
    }
}

impl Cipher for ChaCha20Poly1305Cipher {
    fn encrypt(&mut self, plaintext: &mut Buf, aad: Aad, nonce: Nonce) -> Result<(), CryptoError> {
        let aws_nonce =
            AwsNonce::try_assume_unique_for_key(&nonce).map_err(|_| CryptoError::InvalidNonce)?;

        let aws_aad = AwsAad::from(&aad[..]);

        self.key
            .seal_in_place_append_tag(aws_nonce, aws_aad, plaintext)
            .map_err(|_| CryptoError::OperationFailed(CryptoOperation::Encrypt))?;

        Ok(())
    }

    fn decrypt(
        &mut self,
        ciphertext: &mut TmpBuf,
        aad: Aad,
        nonce: Nonce,
    ) -> Result<(), CryptoError> {
        if ciphertext.len() < 16 {
            return Err(CryptoError::CiphertextTooShort {
                minimum: 16,
                actual: ciphertext.len() as u8,
            });
        }

        let aws_nonce =
            AwsNonce::try_assume_unique_for_key(&nonce).map_err(|_| CryptoError::InvalidNonce)?;

        let aws_aad = AwsAad::from(&aad[..]);

        let plaintext = self
            .key
            .open_in_place(aws_nonce, aws_aad, ciphertext.as_mut())
            .map_err(|_| CryptoError::OperationFailed(CryptoOperation::Decrypt))?;

        let plaintext_len = plaintext.len();
        ciphertext.truncate(plaintext_len);

        Ok(())
    }
}

/// TLS_ECDHE_ECDSA_WITH_AES_128_GCM_SHA256 cipher suite.
#[derive(Debug)]
struct Aes128GcmSha256;

impl SupportedDtls12CipherSuite for Aes128GcmSha256 {
    fn suite(&self) -> Dtls12CipherSuite {
        Dtls12CipherSuite::ECDHE_ECDSA_AES128_GCM_SHA256
    }

    fn hash_algorithm(&self) -> HashAlgorithm {
        HashAlgorithm::SHA256
    }

    fn key_lengths(&self) -> (usize, usize, usize) {
        (0, 16, 4) // (mac_key_len, enc_key_len, fixed_iv_len)
    }

    fn explicit_nonce_len(&self) -> usize {
        8
    }

    fn tag_len(&self) -> usize {
        16
    }

    fn create_cipher(&self, key: &[u8]) -> Result<Box<dyn Cipher>, CryptoError> {
        Ok(Box::new(AesGcm::new(key)?))
    }
}

/// TLS_ECDHE_ECDSA_WITH_AES_256_GCM_SHA384 cipher suite.
#[derive(Debug)]
struct Aes256GcmSha384;

impl SupportedDtls12CipherSuite for Aes256GcmSha384 {
    fn suite(&self) -> Dtls12CipherSuite {
        Dtls12CipherSuite::ECDHE_ECDSA_AES256_GCM_SHA384
    }

    fn hash_algorithm(&self) -> HashAlgorithm {
        HashAlgorithm::SHA384
    }

    fn key_lengths(&self) -> (usize, usize, usize) {
        (0, 32, 4) // (mac_key_len, enc_key_len, fixed_iv_len)
    }

    fn explicit_nonce_len(&self) -> usize {
        8
    }

    fn tag_len(&self) -> usize {
        16
    }

    fn create_cipher(&self, key: &[u8]) -> Result<Box<dyn Cipher>, CryptoError> {
        Ok(Box::new(AesGcm::new(key)?))
    }
}

/// TLS_ECDHE_ECDSA_WITH_CHACHA20_POLY1305_SHA256 cipher suite.
#[derive(Debug)]
struct ChaCha20Poly1305Sha256;

impl SupportedDtls12CipherSuite for ChaCha20Poly1305Sha256 {
    fn suite(&self) -> Dtls12CipherSuite {
        Dtls12CipherSuite::ECDHE_ECDSA_CHACHA20_POLY1305_SHA256
    }

    fn hash_algorithm(&self) -> HashAlgorithm {
        HashAlgorithm::SHA256
    }

    fn key_lengths(&self) -> (usize, usize, usize) {
        (0, 32, 12) // (mac_key_len, enc_key_len, fixed_iv_len)
    }

    fn explicit_nonce_len(&self) -> usize {
        0
    }

    fn tag_len(&self) -> usize {
        16
    }

    fn create_cipher(&self, key: &[u8]) -> Result<Box<dyn Cipher>, CryptoError> {
        Ok(Box::new(ChaCha20Poly1305Cipher::new(key)?))
    }
}

/// TLS_PSK_WITH_AES_128_CCM_8 cipher suite.
#[derive(Debug)]
struct PskAes128Ccm8;

impl SupportedDtls12CipherSuite for PskAes128Ccm8 {
    fn suite(&self) -> Dtls12CipherSuite {
        Dtls12CipherSuite::PSK_AES128_CCM_8
    }

    fn hash_algorithm(&self) -> HashAlgorithm {
        HashAlgorithm::SHA256
    }

    fn key_lengths(&self) -> (usize, usize, usize) {
        (0, 16, 4) // (mac_key_len, enc_key_len, fixed_iv_len)
    }

    fn explicit_nonce_len(&self) -> usize {
        8
    }

    fn tag_len(&self) -> usize {
        8
    }

    fn create_cipher(&self, key: &[u8]) -> Result<Box<dyn Cipher>, CryptoError> {
        Ok(Box::new(crate::crypto::ccm_cipher::AesCcm8Cipher::new(
            key,
        )?))
    }
}

/// Static instances of supported DTLS 1.2 cipher suites.
static AES_128_GCM_SHA256: Aes128GcmSha256 = Aes128GcmSha256;
static AES_256_GCM_SHA384: Aes256GcmSha384 = Aes256GcmSha384;
static CHACHA20_POLY1305_SHA256: ChaCha20Poly1305Sha256 = ChaCha20Poly1305Sha256;
static PSK_AES_128_CCM_8: PskAes128Ccm8 = PskAes128Ccm8;

/// All supported DTLS 1.2 cipher suites.
pub(super) static ALL_CIPHER_SUITES: &[&dyn SupportedDtls12CipherSuite] = &[
    &AES_128_GCM_SHA256,
    &AES_256_GCM_SHA384,
    &CHACHA20_POLY1305_SHA256,
    &PSK_AES_128_CCM_8,
];

// ============================================================================
// DTLS 1.3 Cipher Suites
// ============================================================================

/// TLS_AES_128_GCM_SHA256 cipher suite (TLS 1.3 / DTLS 1.3).
#[derive(Debug)]
struct Tls13Aes128GcmSha256;

impl SupportedDtls13CipherSuite for Tls13Aes128GcmSha256 {
    fn suite(&self) -> Dtls13CipherSuite {
        Dtls13CipherSuite::AES_128_GCM_SHA256
    }

    fn hash_algorithm(&self) -> HashAlgorithm {
        HashAlgorithm::SHA256
    }

    fn key_len(&self) -> usize {
        16 // AES-128
    }

    fn iv_len(&self) -> usize {
        12 // GCM IV
    }

    fn tag_len(&self) -> usize {
        16 // GCM tag
    }

    fn create_cipher(&self, key: &[u8]) -> Result<Box<dyn Cipher>, CryptoError> {
        Ok(Box::new(AesGcm::new(key)?))
    }

    fn encrypt_sn(&self, sn_key: &[u8], sample: &[u8; 16]) -> [u8; 16] {
        aes_ecb_encrypt(&AES_128, sn_key, sample)
    }
}

/// TLS_AES_256_GCM_SHA384 cipher suite (TLS 1.3 / DTLS 1.3).
#[derive(Debug)]
struct Tls13Aes256GcmSha384;

impl SupportedDtls13CipherSuite for Tls13Aes256GcmSha384 {
    fn suite(&self) -> Dtls13CipherSuite {
        Dtls13CipherSuite::AES_256_GCM_SHA384
    }

    fn hash_algorithm(&self) -> HashAlgorithm {
        HashAlgorithm::SHA384
    }

    fn key_len(&self) -> usize {
        32 // AES-256
    }

    fn iv_len(&self) -> usize {
        12 // GCM IV
    }

    fn tag_len(&self) -> usize {
        16 // GCM tag
    }

    fn create_cipher(&self, key: &[u8]) -> Result<Box<dyn Cipher>, CryptoError> {
        Ok(Box::new(AesGcm::new(key)?))
    }

    fn encrypt_sn(&self, sn_key: &[u8], sample: &[u8; 16]) -> [u8; 16] {
        aes_ecb_encrypt(&AES_256, sn_key, sample)
    }
}

/// TLS_CHACHA20_POLY1305_SHA256 cipher suite (TLS 1.3 / DTLS 1.3).
#[derive(Debug)]
struct Tls13ChaCha20Poly1305Sha256;

impl SupportedDtls13CipherSuite for Tls13ChaCha20Poly1305Sha256 {
    fn suite(&self) -> Dtls13CipherSuite {
        Dtls13CipherSuite::CHACHA20_POLY1305_SHA256
    }

    fn hash_algorithm(&self) -> HashAlgorithm {
        HashAlgorithm::SHA256
    }

    fn key_len(&self) -> usize {
        32 // ChaCha20 key
    }

    fn iv_len(&self) -> usize {
        12 // Poly1305 nonce
    }

    fn tag_len(&self) -> usize {
        16 // Poly1305 tag
    }

    fn create_cipher(&self, key: &[u8]) -> Result<Box<dyn Cipher>, CryptoError> {
        Ok(Box::new(ChaCha20Poly1305Cipher::new(key)?))
    }

    fn encrypt_sn(&self, sn_key: &[u8], sample: &[u8; 16]) -> [u8; 16] {
        // RFC 9001 Section 5.4.4: ChaCha20 header protection
        // counter = sample[0..4] (LE), nonce = sample[4..16]
        // mask = ChaCha20(sn_key, counter, nonce, <5 zero bytes>)
        // unwrap: sn_key length is validated by caller (32 bytes for ChaCha20)
        let hp_key = HeaderProtectionKey::new(&aws_quic::CHACHA20, sn_key).unwrap();
        // unwrap: sample is exactly 16 bytes as required
        let mask = hp_key.new_mask(sample).unwrap();
        let mut out = [0u8; 16];
        out[..5].copy_from_slice(&mask);
        out
    }
}

/// Static instances of supported DTLS 1.3 cipher suites.
static TLS13_AES_128_GCM_SHA256: Tls13Aes128GcmSha256 = Tls13Aes128GcmSha256;
static TLS13_AES_256_GCM_SHA384: Tls13Aes256GcmSha384 = Tls13Aes256GcmSha384;
static TLS13_CHACHA20_POLY1305_SHA256: Tls13ChaCha20Poly1305Sha256 = Tls13ChaCha20Poly1305Sha256;

/// All supported DTLS 1.3 cipher suites.
pub(super) static ALL_DTLS13_CIPHER_SUITES: &[&dyn SupportedDtls13CipherSuite] = &[
    &TLS13_AES_128_GCM_SHA256,
    &TLS13_AES_256_GCM_SHA384,
    &TLS13_CHACHA20_POLY1305_SHA256,
];

/// AES-ECB single block encryption for record number protection.
fn aes_ecb_encrypt(
    algorithm: &'static aws_cipher::Algorithm,
    key: &[u8],
    input: &[u8; 16],
) -> [u8; 16] {
    // unwrap: key length is validated by caller (matches algorithm)
    let unbound = UnboundCipherKey::new(algorithm, key).unwrap();
    // unwrap: ECB key construction cannot fail for valid AES keys
    let ecb_key = EncryptingKey::ecb(unbound).unwrap();
    let mut block = *input;
    // unwrap: 16-byte input is exactly one AES block
    ecb_key.encrypt(&mut block).unwrap();
    block
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn chacha20_poly1305_key_len_validation() {
        // incorrect length (should be 32)
        let result = ChaCha20Poly1305Cipher::new(&[0, 1, 2, 3, 4, 5]);
        assert_eq!(
            CryptoError::InvalidChacha20Poly1305KeySize { actual: 6 },
            result.unwrap_err()
        );
    }
}
