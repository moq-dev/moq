use std::fmt;

use crate::vendor::dimpl::crypto::{
	Aad, Buf, Cipher, Dtls12CipherSuite, HashAlgorithm, Nonce, SupportedDtls12CipherSuite, TmpBuf,
};
use crate::vendor::dimpl::{CryptoError, CryptoOperation};
use rind::aead;

const TAG_LEN: usize = 16;

/// The AEAD behind a suite.
#[derive(Debug, Clone, Copy)]
enum Algorithm {
	Aes128Gcm,
	Aes256Gcm,
	ChaCha20Poly1305,
}

impl Algorithm {
	fn aead(self) -> &'static aead::Algorithm {
		match self {
			Self::Aes128Gcm => &aead::AES_128_GCM,
			Self::Aes256Gcm => &aead::AES_256_GCM,
			Self::ChaCha20Poly1305 => &aead::CHACHA20_POLY1305,
		}
	}

	fn cipher(self, key: &[u8]) -> Result<Box<dyn Cipher>, CryptoError> {
		let actual = key.len().min(u16::MAX as usize) as u16;
		let key = aead::Key::new(self.aead(), key).map_err(|_| match self {
			Self::Aes128Gcm | Self::Aes256Gcm => CryptoError::InvalidAesGcmKeySize { actual },
			Self::ChaCha20Poly1305 => CryptoError::InvalidChacha20Poly1305KeySize { actual },
		})?;
		Ok(Box::new(Aead(key)))
	}
}

struct Aead(aead::Key);

impl fmt::Debug for Aead {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.write_str("Aead")
	}
}

impl Cipher for Aead {
	fn encrypt(&mut self, plaintext: &mut Buf, aad: Aad, nonce: Nonce) -> Result<(), CryptoError> {
		self.0
			.seal_in_place_append_tag(aead::Nonce::assume_unique_for_key(nonce.0), &aad, plaintext)
			.map_err(|_| CryptoError::OperationFailed(CryptoOperation::Encrypt))
	}

	fn decrypt(&mut self, ciphertext: &mut TmpBuf, aad: Aad, nonce: Nonce) -> Result<(), CryptoError> {
		let len = ciphertext.len();
		let Some(plaintext_len) = len.checked_sub(TAG_LEN) else {
			return Err(CryptoError::CiphertextTooShort {
				minimum: TAG_LEN as u8,
				actual: len as u8,
			});
		};

		self.0
			.open_in_place(aead::Nonce::assume_unique_for_key(nonce.0), &aad, ciphertext.as_mut())
			.map_err(|_| CryptoError::OperationFailed(CryptoOperation::Decrypt))?;

		ciphertext.truncate(plaintext_len);
		Ok(())
	}
}

#[derive(Debug)]
struct Dtls12 {
	suite: Dtls12CipherSuite,
	hash: HashAlgorithm,
	algorithm: Algorithm,
}

impl SupportedDtls12CipherSuite for Dtls12 {
	fn suite(&self) -> Dtls12CipherSuite {
		self.suite
	}

	fn hash_algorithm(&self) -> HashAlgorithm {
		self.hash
	}

	fn key_lengths(&self) -> (usize, usize, usize) {
		// (MAC key, encryption key, fixed IV): RFC 5288 and RFC 7905.
		let fixed_iv_len = match self.algorithm {
			Algorithm::Aes128Gcm | Algorithm::Aes256Gcm => 4,
			Algorithm::ChaCha20Poly1305 => 12,
		};
		(0, self.algorithm.aead().key_len(), fixed_iv_len)
	}

	fn explicit_nonce_len(&self) -> usize {
		match self.algorithm {
			Algorithm::Aes128Gcm | Algorithm::Aes256Gcm => 8,
			Algorithm::ChaCha20Poly1305 => 0,
		}
	}

	fn tag_len(&self) -> usize {
		TAG_LEN
	}

	fn create_cipher(&self, key: &[u8]) -> Result<Box<dyn Cipher>, CryptoError> {
		self.algorithm.cipher(key)
	}
}

/// TLS_PSK_WITH_AES_128_CCM_8, on dimpl's own AES-CCM.
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
		Ok(Box::new(crate::vendor::dimpl::crypto::ccm_cipher::AesCcm8Cipher::new(
			key,
		)?))
	}
}

const AES_128_GCM: &dyn SupportedDtls12CipherSuite = &Dtls12 {
	suite: Dtls12CipherSuite::ECDHE_ECDSA_AES128_GCM_SHA256,
	hash: HashAlgorithm::SHA256,
	algorithm: Algorithm::Aes128Gcm,
};
const AES_256_GCM: &dyn SupportedDtls12CipherSuite = &Dtls12 {
	suite: Dtls12CipherSuite::ECDHE_ECDSA_AES256_GCM_SHA384,
	hash: HashAlgorithm::SHA384,
	algorithm: Algorithm::Aes256Gcm,
};
const CHACHA20_POLY1305: &dyn SupportedDtls12CipherSuite = &Dtls12 {
	suite: Dtls12CipherSuite::ECDHE_ECDSA_CHACHA20_POLY1305_SHA256,
	hash: HashAlgorithm::SHA256,
	algorithm: Algorithm::ChaCha20Poly1305,
};
const PSK_AES_128_CCM_8: &dyn SupportedDtls12CipherSuite = &PskAes128Ccm8;

/// DTLS 1.2 suites, in order of preference where rind runs AES on CPU instructions.
pub(super) static DTLS12: &[&dyn SupportedDtls12CipherSuite] =
	&[AES_128_GCM, AES_256_GCM, CHACHA20_POLY1305, PSK_AES_128_CCM_8];

/// [`DTLS12`] with ChaCha20-Poly1305 first, which outruns AES without AES
/// instructions.
pub(super) static DTLS12_CHACHA20_FIRST: &[&dyn SupportedDtls12CipherSuite] =
	&[CHACHA20_POLY1305, AES_128_GCM, AES_256_GCM, PSK_AES_128_CCM_8];

#[cfg(test)]
mod test {
	use super::*;

	#[test]
	fn chacha20_poly1305_key_len_validation() {
		// incorrect length (should be 32)
		let result = Algorithm::ChaCha20Poly1305.cipher(&[0, 1, 2, 3, 4, 5]);
		assert_eq!(
			CryptoError::InvalidChacha20Poly1305KeySize { actual: 6 },
			result.unwrap_err()
		);
	}
}
