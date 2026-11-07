//! SRTP cipher implementations using AWS-LC-RS.

use aws_lc_rs::aead::{AES_128_GCM, AES_256_GCM, Aad, LessSafeKey, Nonce, UnboundKey};
use aws_lc_rs::cipher::{AES_128, EncryptingKey, EncryptionContext, UnboundCipherKey};

use str0m_proto::crypto::CryptoError;
use str0m_proto::crypto::{AeadAes128Gcm, AeadAes128GcmCipher, AeadAes256Gcm};
use str0m_proto::crypto::{AeadAes256GcmCipher, Aes128CmSha1_80Cipher};
use str0m_proto::crypto::{SrtpProvider, SupportedAeadAes128Gcm};
use str0m_proto::crypto::{SupportedAeadAes256Gcm, SupportedAes128CmSha1_80};

// ============================================================================
// AES-128-CM-SHA1-80 Cipher
// ============================================================================

struct AwsLcRsAes128CmSha1_80Cipher {
    key: [u8; 16],
}

impl std::fmt::Debug for AwsLcRsAes128CmSha1_80Cipher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AwsLcRsAes128CmSha1_80Cipher").finish()
    }
}

impl Aes128CmSha1_80Cipher for AwsLcRsAes128CmSha1_80Cipher {
    fn encrypt(
        &mut self,
        iv: &[u8; 16],
        input: &[u8],
        output: &mut [u8],
    ) -> Result<(), CryptoError> {
        // AES-128 Counter Mode (CTR)
        let unbound_key = UnboundCipherKey::new(&AES_128, &self.key).expect("valid key");
        let encrypting_key = EncryptingKey::ctr(unbound_key).expect("CTR mode");

        // For CTR mode, we need the full 16-byte IV as the counter
        let context = EncryptionContext::Iv128(
            aws_lc_rs::iv::FixedLength::try_from(iv.as_ref()).expect("16-byte IV"),
        );

        // Copy input to output buffer for in-place encryption
        output[..input.len()].copy_from_slice(input);

        encrypting_key
            .less_safe_encrypt(&mut output[..input.len()], context)
            .map_err(|e| CryptoError::Other(format!("AES-CTR encrypt failed: {}", e)))?;

        Ok(())
    }

    fn decrypt(
        &mut self,
        iv: &[u8; 16],
        input: &[u8],
        output: &mut [u8],
    ) -> Result<(), CryptoError> {
        // AES-CTR mode is symmetric, so we can use the same operation
        self.encrypt(iv, input, output)
    }
}

// ============================================================================
// AEAD-AES-128-GCM Cipher
// ============================================================================

struct AwsLcRsAeadAes128GcmCipher {
    key: LessSafeKey,
}

impl std::fmt::Debug for AwsLcRsAeadAes128GcmCipher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AwsLcRsAeadAes128GcmCipher").finish()
    }
}

impl AeadAes128GcmCipher for AwsLcRsAeadAes128GcmCipher {
    fn encrypt(
        &mut self,
        iv: &[u8; AeadAes128Gcm::IV_LEN],
        aad: &[u8],
        input: &[u8],
        output: &mut [u8],
    ) -> Result<(), CryptoError> {
        assert!(
            aad.len() >= 12,
            "Associated data length MUST be at least 12 octets"
        );

        let nonce = Nonce::try_assume_unique_for_key(iv)
            .map_err(|e| CryptoError::Other(format!("Invalid nonce: {}", e)))?;

        let len = input.len();
        output[..len].copy_from_slice(input);
        let tag = self
            .key
            .seal_in_place_separate_tag(nonce, Aad::from(aad), &mut output[..len])
            .map_err(|e| CryptoError::Other(format!("AES-GCM encrypt failed: {}", e)))?;

        // Copy the result tag to the output
        output[len..len + AeadAes128Gcm::TAG_LEN].copy_from_slice(tag.as_ref());
        Ok(())
    }

    fn decrypt(
        &mut self,
        iv: &[u8; AeadAes128Gcm::IV_LEN],
        aads: &[&[u8]],
        input: &[u8],
        output: &mut [u8],
    ) -> Result<usize, CryptoError> {
        // Caller contract: `SrtpContext::unprotect_rtp` and `unprotect_rtcp` guarantee
        // the input holds at least the auth tag. A shorter buffer is a bug in str0m,
        // not bad input, so fail fast rather than hiding it in an error.
        assert!(input.len() >= AeadAes128Gcm::TAG_LEN);

        let nonce = Nonce::try_assume_unique_for_key(iv)
            .map_err(|e| CryptoError::Other(format!("Invalid nonce: {}", e)))?;

        // Concatenate AAD slices if needed
        let aad_data: Vec<u8>;
        let aad = if aads.len() == 1 {
            Aad::from(aads[0])
        } else {
            aad_data = aads.concat();
            Aad::from(aad_data.as_slice())
        };

        let ct_len = input.len() - AeadAes128Gcm::TAG_LEN;
        let (ciphertext, tag) = input.split_at(ct_len);
        self.key
            .open_separate_gather(nonce, aad, ciphertext, tag, &mut output[..ct_len])
            .map_err(|e| CryptoError::Other(format!("AES-GCM decrypt failed: {}", e)))?;

        Ok(ct_len)
    }
}

// ============================================================================
// AEAD-AES-256-GCM Cipher
// ============================================================================

struct AwsLcRsAeadAes256GcmCipher {
    key: LessSafeKey,
}

impl std::fmt::Debug for AwsLcRsAeadAes256GcmCipher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AwsLcRsAeadAes256GcmCipher").finish()
    }
}

impl AeadAes256GcmCipher for AwsLcRsAeadAes256GcmCipher {
    fn encrypt(
        &mut self,
        iv: &[u8; AeadAes256Gcm::IV_LEN],
        aad: &[u8],
        input: &[u8],
        output: &mut [u8],
    ) -> Result<(), CryptoError> {
        assert!(
            aad.len() >= 12,
            "Associated data length MUST be at least 12 octets"
        );

        let nonce = Nonce::try_assume_unique_for_key(iv)
            .map_err(|e| CryptoError::Other(format!("Invalid nonce: {}", e)))?;

        let len = input.len();
        output[..len].copy_from_slice(input);
        let tag = self
            .key
            .seal_in_place_separate_tag(nonce, Aad::from(aad), &mut output[..len])
            .map_err(|e| CryptoError::Other(format!("AES-GCM encrypt failed: {}", e)))?;

        // Copy the result tag to the output
        output[len..len + AeadAes256Gcm::TAG_LEN].copy_from_slice(tag.as_ref());
        Ok(())
    }

    fn decrypt(
        &mut self,
        iv: &[u8; AeadAes256Gcm::IV_LEN],
        aads: &[&[u8]],
        input: &[u8],
        output: &mut [u8],
    ) -> Result<usize, CryptoError> {
        // Caller contract: `SrtpContext::unprotect_rtp` and `unprotect_rtcp` guarantee
        // the input holds at least the auth tag. A shorter buffer is a bug in str0m,
        // not bad input, so fail fast rather than hiding it in an error.
        assert!(input.len() >= AeadAes256Gcm::TAG_LEN);

        let nonce = Nonce::try_assume_unique_for_key(iv)
            .map_err(|e| CryptoError::Other(format!("Invalid nonce: {}", e)))?;

        // Concatenate AAD slices if needed
        let aad_data: Vec<u8>;
        let aad = if aads.len() == 1 {
            Aad::from(aads[0])
        } else {
            aad_data = aads.concat();
            Aad::from(aad_data.as_slice())
        };

        let ct_len = input.len() - AeadAes256Gcm::TAG_LEN;
        let (ciphertext, tag) = input.split_at(ct_len);
        self.key
            .open_separate_gather(nonce, aad, ciphertext, tag, &mut output[..ct_len])
            .map_err(|e| CryptoError::Other(format!("AES-GCM decrypt failed: {}", e)))?;

        Ok(ct_len)
    }
}

// ============================================================================
// SRTP Profile Support Implementations
// ============================================================================

struct AwsLcRsSupportedAes128CmSha1_80;

impl std::fmt::Debug for AwsLcRsSupportedAes128CmSha1_80 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AwsLcRsSupportedAes128CmSha1_80").finish()
    }
}

impl SupportedAes128CmSha1_80 for AwsLcRsSupportedAes128CmSha1_80 {
    fn create_cipher(&self, key: [u8; 16], _encrypt: bool) -> Box<dyn Aes128CmSha1_80Cipher> {
        Box::new(AwsLcRsAes128CmSha1_80Cipher { key })
    }
}

struct AwsLcRsSupportedAeadAes128Gcm;

impl std::fmt::Debug for AwsLcRsSupportedAeadAes128Gcm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AwsLcRsSupportedAeadAes128Gcm").finish()
    }
}

impl SupportedAeadAes128Gcm for AwsLcRsSupportedAeadAes128Gcm {
    fn create_cipher(&self, key: [u8; 16], _encrypt: bool) -> Box<dyn AeadAes128GcmCipher> {
        let unbound_key = UnboundKey::new(&AES_128_GCM, &key).expect("valid AES-128-GCM key");
        let less_safe_key = LessSafeKey::new(unbound_key);

        Box::new(AwsLcRsAeadAes128GcmCipher { key: less_safe_key })
    }
}

struct AwsLcRsSupportedAeadAes256Gcm;

impl std::fmt::Debug for AwsLcRsSupportedAeadAes256Gcm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AwsLcRsSupportedAeadAes256Gcm").finish()
    }
}

impl SupportedAeadAes256Gcm for AwsLcRsSupportedAeadAes256Gcm {
    fn create_cipher(&self, key: [u8; 32], _encrypt: bool) -> Box<dyn AeadAes256GcmCipher> {
        let unbound_key = UnboundKey::new(&AES_256_GCM, &key).expect("valid AES-256-GCM key");
        let less_safe_key = LessSafeKey::new(unbound_key);

        Box::new(AwsLcRsAeadAes256GcmCipher { key: less_safe_key })
    }
}

// ============================================================================
// SRTP Provider Implementation
// ============================================================================

pub(super) struct AwsLcRsSrtpProvider;

impl std::fmt::Debug for AwsLcRsSrtpProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AwsLcRsSrtpProvider").finish()
    }
}

impl SrtpProvider for AwsLcRsSrtpProvider {
    fn aes_128_cm_sha1_80(&self) -> &'static dyn SupportedAes128CmSha1_80 {
        &AwsLcRsSupportedAes128CmSha1_80
    }

    fn aead_aes_128_gcm(&self) -> &'static dyn SupportedAeadAes128Gcm {
        &AwsLcRsSupportedAeadAes128Gcm
    }

    fn aead_aes_256_gcm(&self) -> &'static dyn SupportedAeadAes256Gcm {
        &AwsLcRsSupportedAeadAes256Gcm
    }

    fn srtp_aes_128_ecb_round(&self, key: &[u8], input: &[u8], output: &mut [u8]) {
        use aws_lc_rs::cipher::{AES_128, EncryptingKey, EncryptionContext, UnboundCipherKey};

        let unbound_key = UnboundCipherKey::new(&AES_128, key).expect("valid key");
        let encrypting_key = EncryptingKey::ecb(unbound_key).expect("ECB mode");

        // Copy input to output buffer for in-place encryption
        output[..input.len()].copy_from_slice(input);

        // ECB mode uses EncryptionContext::None
        let _decryption_context = encrypting_key
            .less_safe_encrypt(&mut output[..input.len()], EncryptionContext::None)
            .expect("AES-128-ECB encryption");

        // The less_safe_encrypt returns a DecryptionContext which includes the padding info
        // For ECB with PKCS7 padding, 16 bytes input becomes 32 bytes output
    }

    fn srtp_aes_256_ecb_round(&self, key: &[u8], input: &[u8], output: &mut [u8]) {
        use aws_lc_rs::cipher::{AES_256, EncryptingKey, EncryptionContext, UnboundCipherKey};

        let unbound_key = UnboundCipherKey::new(&AES_256, key).expect("valid key");
        let encrypting_key = EncryptingKey::ecb(unbound_key).expect("ECB mode");

        // Copy input to output buffer for in-place encryption
        output[..input.len()].copy_from_slice(input);

        // ECB mode uses EncryptionContext::None
        let _decryption_context = encrypting_key
            .less_safe_encrypt(&mut output[..input.len()], EncryptionContext::None)
            .expect("AES-256-ECB encryption");

        // The less_safe_encrypt returns a DecryptionContext which includes the padding info
        // For ECB with PKCS7 padding, 16 bytes input becomes 32 bytes output
    }
}
