//! AES-128-CCM-8 cipher implementation using the RustCrypto `ccm` crate.
//!
//! Shared by both aws-lc-rs and rust-crypto backends since aws-lc-rs
//! does not expose CCM in its high-level API.

use ccm::aead::AeadInOut;
use ccm::aead::KeyInit;
use ccm::consts::{U8, U12};

use super::{Aad, Cipher, Nonce};
use crate::buffer::{Buf, TmpBuf};
use crate::error::bounded_error_len;
use crate::{CryptoError, CryptoOperation};

/// AES-128-CCM with 8-byte tag, 12-byte nonce.
type Aes128Ccm8 = ccm::Ccm<aes::Aes128, U8, U12>;

/// AES-128-CCM-8 cipher for TLS_PSK_WITH_AES_128_CCM_8.
pub struct AesCcm8Cipher {
    cipher: Box<Aes128Ccm8>,
}

impl std::fmt::Debug for AesCcm8Cipher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AesCcm8Cipher").finish_non_exhaustive()
    }
}

impl AesCcm8Cipher {
    pub fn new(key: &[u8]) -> Result<Self, CryptoError> {
        if key.len() != 16 {
            return Err(CryptoError::InvalidAes128Ccm8KeySize {
                actual: bounded_error_len(key.len()),
            });
        }
        let cipher = Aes128Ccm8::new_from_slice(key)
            .map_err(|_| CryptoError::OperationFailed(CryptoOperation::CreateCipher))?;
        Ok(AesCcm8Cipher {
            cipher: Box::new(cipher),
        })
    }
}

impl Cipher for AesCcm8Cipher {
    fn encrypt(&mut self, plaintext: &mut Buf, aad: Aad, nonce: Nonce) -> Result<(), CryptoError> {
        let ccm_nonce = ccm::Nonce::<U12>::from(nonce.0);
        let tag = self
            .cipher
            .encrypt_inout_detached(&ccm_nonce, &aad[..], plaintext.as_mut().into())
            .map_err(|_| CryptoError::OperationFailed(CryptoOperation::Encrypt))?;

        // Append the 8-byte tag
        plaintext.extend_from_slice(&tag);

        Ok(())
    }

    fn decrypt(
        &mut self,
        ciphertext: &mut TmpBuf,
        aad: Aad,
        nonce: Nonce,
    ) -> Result<(), CryptoError> {
        if ciphertext.len() < 8 {
            return Err(CryptoError::CiphertextTooShort {
                minimum: 8,
                actual: ciphertext.len() as u8,
            });
        }

        let ccm_nonce = ccm::Nonce::<U12>::from(nonce.0);

        // Split off the 8-byte tag from the end
        let data_len = ciphertext.len() - 8;
        let mut tag_bytes = [0u8; 8];
        tag_bytes.copy_from_slice(&ciphertext.as_ref()[data_len..]);
        let tag = ccm::Tag::<U8>::from(tag_bytes);

        // Truncate to just the ciphertext (without tag)
        ciphertext.truncate(data_len);

        self.cipher
            .decrypt_inout_detached(&ccm_nonce, &aad[..], ciphertext.as_mut().into(), &tag)
            .map_err(|_| CryptoError::OperationFailed(CryptoOperation::Decrypt))?;

        Ok(())
    }
}
