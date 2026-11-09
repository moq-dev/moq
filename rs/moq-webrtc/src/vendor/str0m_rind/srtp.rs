use std::fmt;

use rind::aead::{self, Nonce, Tag};
use rind::cipher::{self, Aes, AesCtr};

use crate::vendor::str0m_proto::crypto::{
	AeadAes128GcmCipher, AeadAes256GcmCipher, Aes128CmSha1_80Cipher, CryptoError, SrtpProvider, SupportedAeadAes128Gcm,
	SupportedAeadAes256Gcm, SupportedAes128CmSha1_80,
};

const TAG_LEN: usize = 16;

pub(super) static SRTP: RindSrtp = RindSrtp;

#[derive(Debug)]
pub(super) struct RindSrtp;

impl SrtpProvider for RindSrtp {
	fn aes_128_cm_sha1_80(&self) -> &'static dyn SupportedAes128CmSha1_80 {
		&RindSrtp
	}

	fn aead_aes_128_gcm(&self) -> &'static dyn SupportedAeadAes128Gcm {
		&RindSrtp
	}

	fn aead_aes_256_gcm(&self) -> &'static dyn SupportedAeadAes256Gcm {
		&RindSrtp
	}

	/// Encrypts one SRTP key derivation block.
	///
	/// Panics unless `key` is 16 bytes, `input` 16, and `output` at least 16:
	/// the trait returns `()`, so there is no error to report.
	fn srtp_aes_128_ecb_round(&self, key: &[u8], input: &[u8], output: &mut [u8]) {
		ecb_round(&cipher::AES_128, key, input, output);
	}

	/// Encrypts one SRTP key derivation block.
	///
	/// Panics unless `key` is 32 bytes, `input` 16, and `output` at least 16:
	/// the trait returns `()`, so there is no error to report.
	fn srtp_aes_256_ecb_round(&self, key: &[u8], input: &[u8], output: &mut [u8]) {
		ecb_round(&cipher::AES_256, key, input, output);
	}
}

// str0m sizes these from fixed arrays, never from a packet, so a mismatch is a
// str0m bug worth a panic.
fn ecb_round(algorithm: &'static cipher::Algorithm, key: &[u8], input: &[u8], output: &mut [u8]) {
	let aes = Aes::new(algorithm, key).expect("SRTP master key length");
	let mut block: [u8; 16] = input.try_into().expect("SRTP key derivation block");
	aes.encrypt_block(&mut block);
	output[..16].copy_from_slice(&block);
}

impl SupportedAes128CmSha1_80 for RindSrtp {
	fn create_cipher(&self, key: [u8; 16], _encrypt: bool) -> Box<dyn Aes128CmSha1_80Cipher> {
		Box::new(Cm(AesCtr::new(&cipher::AES_128, &key, 16).unwrap()))
	}
}

impl SupportedAeadAes128Gcm for RindSrtp {
	fn create_cipher(&self, key: [u8; 16], _encrypt: bool) -> Box<dyn AeadAes128GcmCipher> {
		Box::new(Gcm::new(&aead::AES_128_GCM, &key))
	}
}

impl SupportedAeadAes256Gcm for RindSrtp {
	fn create_cipher(&self, key: [u8; 32], _encrypt: bool) -> Box<dyn AeadAes256GcmCipher> {
		Box::new(Gcm::new(&aead::AES_256_GCM, &key))
	}
}

/// AES counter mode, keyed for one SRTP or SRTCP direction.
struct Cm(AesCtr);

impl fmt::Debug for Cm {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("Cm").finish_non_exhaustive()
	}
}

impl Cm {
	fn apply(&self, iv: &[u8; 16], input: &[u8], output: &mut [u8]) -> Result<(), CryptoError> {
		let output = output
			.get_mut(..input.len())
			.ok_or_else(|| CryptoError::Other("AES-CM output too short".into()))?;
		output.copy_from_slice(input);

		// SRTP AES-CM counts over the IV's low 16 bits, which str0m leaves
		// zero, so rind refuses a packet past 2^16 blocks rather than wrap.
		self.0
			.apply_keystream(iv, output)
			.map_err(|e| CryptoError::Other(format!("AES-CM failed: {e:?}")))
	}
}

impl Aes128CmSha1_80Cipher for Cm {
	fn encrypt(&mut self, iv: &[u8; 16], input: &[u8], output: &mut [u8]) -> Result<(), CryptoError> {
		self.apply(iv, input, output)
	}

	fn decrypt(&mut self, iv: &[u8; 16], input: &[u8], output: &mut [u8]) -> Result<(), CryptoError> {
		self.apply(iv, input, output)
	}
}

/// AES-GCM, keyed for one SRTP or SRTCP direction.
struct Gcm(aead::Key);

impl fmt::Debug for Gcm {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("Gcm").finish_non_exhaustive()
	}
}

impl Gcm {
	/// str0m passes the profile's key length.
	fn new(algorithm: &'static aead::Algorithm, key: &[u8]) -> Self {
		Self(aead::Key::new(algorithm, key).unwrap())
	}

	/// Writes the ciphertext then the tag into `output`.
	fn seal(&self, iv: &[u8; 12], aad: &[u8], input: &[u8], output: &mut [u8]) -> Result<(), CryptoError> {
		let output = output
			.get_mut(..input.len() + TAG_LEN)
			.ok_or_else(|| CryptoError::Other("AES-GCM output too short".into()))?;
		let (ciphertext, tag_out) = output.split_at_mut(input.len());
		ciphertext.copy_from_slice(input);
		let tag = self
			.0
			.seal_in_place_separate_tag(Nonce::assume_unique_for_key(*iv), aad, ciphertext)
			.map_err(|e| CryptoError::Other(format!("AES-GCM failed: {e:?}")))?;
		tag_out.copy_from_slice(tag.as_ref());
		Ok(())
	}

	/// Opens `input`, the ciphertext then the tag, and returns the plaintext length.
	fn open(&self, iv: &[u8; 12], aads: &[&[u8]], input: &[u8], output: &mut [u8]) -> Result<usize, CryptoError> {
		let ciphertext_len = input
			.len()
			.checked_sub(TAG_LEN)
			.ok_or_else(|| CryptoError::Other("AES-GCM input shorter than its tag".into()))?;
		let (ciphertext, tag) = input.split_at(ciphertext_len);
		let tag = Tag::try_from(tag).expect("split at the tag length");
		let plaintext = output
			.get_mut(..ciphertext.len())
			.ok_or_else(|| CryptoError::Other("AES-GCM output too short".into()))?;
		plaintext.copy_from_slice(ciphertext);

		// str0m splits the AAD only for unencrypted SRTCP.
		let joined;
		let aad = match aads {
			[aad] => aad,
			_ => {
				joined = aads.concat();
				joined.as_slice()
			}
		};

		self.0
			.open_in_place_separate_tag(Nonce::assume_unique_for_key(*iv), aad, tag, plaintext)
			.map_err(|e| CryptoError::Other(format!("AES-GCM failed: {e:?}")))?;
		Ok(ciphertext.len())
	}
}

impl AeadAes128GcmCipher for Gcm {
	fn encrypt(&mut self, iv: &[u8; 12], aad: &[u8], input: &[u8], output: &mut [u8]) -> Result<(), CryptoError> {
		self.seal(iv, aad, input, output)
	}

	fn decrypt(
		&mut self,
		iv: &[u8; 12],
		aads: &[&[u8]],
		input: &[u8],
		output: &mut [u8],
	) -> Result<usize, CryptoError> {
		self.open(iv, aads, input, output)
	}
}

impl AeadAes256GcmCipher for Gcm {
	fn encrypt(&mut self, iv: &[u8; 12], aad: &[u8], input: &[u8], output: &mut [u8]) -> Result<(), CryptoError> {
		self.seal(iv, aad, input, output)
	}

	fn decrypt(
		&mut self,
		iv: &[u8; 12],
		aads: &[&[u8]],
		input: &[u8],
		output: &mut [u8],
	) -> Result<usize, CryptoError> {
		self.open(iv, aads, input, output)
	}
}
