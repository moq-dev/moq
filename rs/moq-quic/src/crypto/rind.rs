use rind::{aead, hkdf, hmac};

use crate::crypto::{self, CryptoError};

impl crypto::HmacKey for hmac::Key {
	fn sign(&self, data: &[u8], out: &mut [u8]) {
		out.copy_from_slice(hmac::sign(self, data).as_ref());
	}

	fn signature_len(&self) -> usize {
		32
	}
}

impl crypto::HandshakeTokenKey for hkdf::Prk {
	fn aead_from_hkdf(&self, random_bytes: &[u8]) -> Box<dyn crypto::AeadKey> {
		Box::new(aead::Key::from(self.expand(&[random_bytes], &aead::AES_256_GCM)))
	}
}

impl crypto::AeadKey for aead::Key {
	fn seal(&self, data: &mut Vec<u8>, additional_data: &[u8]) -> Result<(), CryptoError> {
		// Each token derives its own key from a fresh random salt, so its nonce is zero.
		let nonce = aead::Nonce::assume_unique_for_key([0; 12]);
		self.seal_in_place_append_tag(nonce, additional_data, data)
			.map_err(|_| CryptoError)
	}

	fn open<'a>(&self, data: &'a mut [u8], additional_data: &[u8]) -> Result<&'a mut [u8], CryptoError> {
		let nonce = aead::Nonce::assume_unique_for_key([0; 12]);
		self.open_in_place(nonce, additional_data, data)
			.map_err(|_| CryptoError)
	}
}
