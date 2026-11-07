//! Cipher suite implementations using RustCrypto.
use aes_gcm::aead::AeadInOut;
use aes_gcm::aes::cipher::{BlockCipherEncrypt, KeyInit as BlockKeyInit};
use aes_gcm::aes::{Aes128, Aes256};
use aes_gcm::{Aes128Gcm, Aes256Gcm};
use chacha20::cipher::{KeyIvInit, StreamCipher, StreamCipherSeek};
use chacha20poly1305::ChaCha20Poly1305 as ChaCha20Poly1305Aead;

use super::super::{Cipher, SupportedDtls12CipherSuite, SupportedDtls13CipherSuite};
use crate::buffer::{Buf, TmpBuf};
use crate::crypto::{Aad, Nonce};
use crate::dtls12::message::Dtls12CipherSuite;
use crate::error::bounded_error_len;
use crate::types::{Dtls13CipherSuite, HashAlgorithm};
use crate::{CryptoError, CryptoOperation};

/// AES-GCM cipher implementation using RustCrypto.
enum AesGcm {
    Aes128(Box<Aes128Gcm>),
    Aes256(Box<Aes256Gcm>),
}

impl std::fmt::Debug for AesGcm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AesGcm::Aes128(_) => f.debug_tuple("AesGcm::Aes128").finish(),
            AesGcm::Aes256(_) => f.debug_tuple("AesGcm::Aes256").finish(),
        }
    }
}

impl AesGcm {
    fn new(key: &[u8]) -> Result<Self, CryptoError> {
        match key.len() {
            16 => {
                let cipher = Aes128Gcm::new_from_slice(key)
                    .map_err(|_| CryptoError::OperationFailed(CryptoOperation::CreateCipher))?;
                Ok(AesGcm::Aes128(Box::new(cipher)))
            }
            32 => {
                let cipher = Aes256Gcm::new_from_slice(key)
                    .map_err(|_| CryptoError::OperationFailed(CryptoOperation::CreateCipher))?;
                Ok(AesGcm::Aes256(Box::new(cipher)))
            }
            _ => Err(CryptoError::InvalidAesGcmKeySize {
                actual: bounded_error_len(key.len()),
            }),
        }
    }
}

impl Cipher for AesGcm {
    fn encrypt(&mut self, data: &mut Buf, aad: Aad, nonce: Nonce) -> Result<(), CryptoError> {
        // Create nonce from the provided nonce bytes
        let nonce_array: [u8; 12] = nonce[..12]
            .try_into()
            .map_err(|_| CryptoError::InvalidNonce)?;

        match self {
            AesGcm::Aes128(cipher) => {
                let aes_nonce = aes_gcm::Nonce::from(nonce_array);
                cipher
                    .encrypt_in_place(&aes_nonce, &aad, data)
                    .map_err(|_| CryptoError::OperationFailed(CryptoOperation::Encrypt))?;
            }
            AesGcm::Aes256(cipher) => {
                let aes_nonce = aes_gcm::Nonce::from(nonce_array);
                cipher
                    .encrypt_in_place(&aes_nonce, &aad, data)
                    .map_err(|_| CryptoError::OperationFailed(CryptoOperation::Encrypt))?;
            }
        }

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

        // Create nonce from the provided nonce bytes
        let nonce_array: [u8; 12] = nonce[..12]
            .try_into()
            .map_err(|_| CryptoError::InvalidNonce)?;

        match self {
            AesGcm::Aes128(cipher) => {
                let aes_nonce = aes_gcm::Nonce::from(nonce_array);
                cipher
                    .decrypt_in_place(&aes_nonce, &aad, ciphertext)
                    .map_err(|_| CryptoError::OperationFailed(CryptoOperation::Decrypt))?;
            }
            AesGcm::Aes256(cipher) => {
                let aes_nonce = aes_gcm::Nonce::from(nonce_array);
                cipher
                    .decrypt_in_place(&aes_nonce, &aad, ciphertext)
                    .map_err(|_| CryptoError::OperationFailed(CryptoOperation::Decrypt))?;
            }
        }

        // decrypt_in_place already removes the tag and shortens the buffer
        // No need to truncate further

        Ok(())
    }
}

/// ChaCha20-Poly1305 cipher implementation using RustCrypto.
struct ChaCha20Poly1305Cipher {
    cipher: Box<ChaCha20Poly1305Aead>,
}

impl std::fmt::Debug for ChaCha20Poly1305Cipher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChaCha20Poly1305Cipher")
            .finish_non_exhaustive()
    }
}

impl ChaCha20Poly1305Cipher {
    fn new(key: &[u8]) -> Result<Self, CryptoError> {
        use chacha20poly1305::KeyInit;
        if key.len() != 32 {
            return Err(CryptoError::InvalidChacha20Poly1305KeySize {
                actual: bounded_error_len(key.len()),
            });
        }
        let key = chacha20poly1305::Key::try_from(key)
            .map_err(|_| CryptoError::OperationFailed(CryptoOperation::CreateCipher))?;
        Ok(ChaCha20Poly1305Cipher {
            cipher: Box::new(ChaCha20Poly1305Aead::new(&key)),
        })
    }
}

impl Cipher for ChaCha20Poly1305Cipher {
    fn encrypt(&mut self, data: &mut Buf, aad: Aad, nonce: Nonce) -> Result<(), CryptoError> {
        let nonce_array: [u8; 12] = nonce[..12]
            .try_into()
            .map_err(|_| CryptoError::InvalidNonce)?;

        let chacha_nonce = chacha20poly1305::Nonce::from(nonce_array);
        self.cipher
            .encrypt_in_place(&chacha_nonce, &aad, data)
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

        let nonce_array: [u8; 12] = nonce[..12]
            .try_into()
            .map_err(|_| CryptoError::InvalidNonce)?;

        let chacha_nonce = chacha20poly1305::Nonce::from(nonce_array);
        self.cipher
            .decrypt_in_place(&chacha_nonce, &aad, ciphertext)
            .map_err(|_| CryptoError::OperationFailed(CryptoOperation::Decrypt))?;

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
        // unwrap: sn_key length matches AES-128 key size
        let cipher = Aes128::new_from_slice(sn_key).unwrap();
        let mut block = aes_gcm::aes::Block::from(*sample);
        cipher.encrypt_block(&mut block);
        block.into()
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
        // unwrap: sn_key length matches AES-256 key size
        let cipher = Aes256::new_from_slice(sn_key).unwrap();
        let mut block = aes_gcm::aes::Block::from(*sample);
        cipher.encrypt_block(&mut block);
        block.into()
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
        // counter = sample[0..4] (LE u32), nonce = sample[4..16]
        // mask = first 5 bytes of ChaCha20 keystream at given counter
        let counter = u32::from_le_bytes(sample[0..4].try_into().unwrap());
        let nonce: [u8; 12] = sample[4..16].try_into().unwrap();

        // unwrap: cipher suite keys are validated to be 32 bytes before use.
        let key = chacha20::Key::try_from(sn_key).unwrap();
        let nonce = chacha20::Nonce::from(nonce);

        let mut cipher = chacha20::ChaCha20::new(&key, &nonce);
        // Seek to the correct block counter position (each block = 64 bytes)
        cipher.seek(counter as u64 * 64);

        let mut out = [0u8; 16];
        cipher.apply_keystream(&mut out);
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
