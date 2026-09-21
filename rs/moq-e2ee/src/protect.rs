use aws_lc_rs::aead::{AES_128_GCM, Aad, LessSafeKey, Nonce, UnboundKey};
use bytes::Bytes;

use crate::error::{Error, Result};
use crate::limits::{KEY_LEN, TAG_LEN, check_u53};

/// 96-bit AES-GCM nonce: `u64(group) || u32(frame)`.
pub(crate) fn nonce(group: u64, frame: u64) -> Result<[u8; 12]> {
	check_u53(group)?;
	let frame = u32::try_from(frame).map_err(|_| Error::Identity)?;
	let mut out = [0u8; 12];
	out[..8].copy_from_slice(&group.to_be_bytes());
	out[8..].copy_from_slice(&frame.to_be_bytes());
	Ok(out)
}

/// Encrypt `plaintext` under AES-128-GCM with empty AAD, appending the tag.
///
/// Stateless: the caller accounts for reuse and exhaustion.
pub(crate) fn protect(
	key: &[u8; KEY_LEN],
	group: u64,
	frame: u64,
	plaintext: &[u8],
	payload_limit: usize,
) -> Result<Bytes> {
	let nonce = nonce(group, frame)?;
	if plaintext.len().saturating_add(TAG_LEN) > payload_limit {
		return Err(Error::Oversize);
	}
	let mut in_out = Vec::with_capacity(plaintext.len() + TAG_LEN);
	in_out.extend_from_slice(plaintext);
	aead(key)
		.seal_in_place_append_tag(Nonce::assume_unique_for_key(nonce), Aad::empty(), &mut in_out)
		.expect("AES-128-GCM seal with a valid key and nonce");
	Ok(Bytes::from(in_out))
}

/// Decrypt `payload` (ciphertext concatenated with the 16-byte tag).
///
/// Stateless: the caller accounts for exhaustion and duplicates.
pub(crate) fn open(key: &[u8; KEY_LEN], group: u64, frame: u64, payload: &[u8], payload_limit: usize) -> Result<Bytes> {
	let nonce = nonce(group, frame)?;
	if payload.len() < TAG_LEN || payload.len() > payload_limit {
		return Err(Error::Oversize);
	}
	let mut in_out = payload.to_vec();
	let len = aead(key)
		.open_in_place(Nonce::assume_unique_for_key(nonce), Aad::empty(), &mut in_out)
		.map_err(|_| Error::Authentication)?
		.len();
	in_out.truncate(len);
	Ok(Bytes::from(in_out))
}

fn aead(key: &[u8; KEY_LEN]) -> LessSafeKey {
	let unbound = UnboundKey::new(&AES_128_GCM, key).expect("16-byte AES-128-GCM key");
	LessSafeKey::new(unbound)
}
