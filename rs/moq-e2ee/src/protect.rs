use aws_lc_rs::aead::{AES_128_GCM, Aad, LessSafeKey, Nonce, UnboundKey};
use bytes::Bytes;

use crate::error::{Error, Result};
use crate::limits::{KEY_LEN, TAG_LEN, check_frame, check_u53};

/// 96-bit AES-GCM nonce: `u64(group) || u32(frame)`.
///
/// # Errors
///
/// [`Error::Identity`] if `group` exceeds `2^53-1` or `frame` exceeds `2^32-1`.
pub fn nonce(group: u64, frame: u64) -> Result<[u8; 12]> {
	check_u53(group)?;
	let frame = check_frame(frame)?;
	let mut out = [0u8; 12];
	out[..8].copy_from_slice(&group.to_be_bytes());
	out[8..].copy_from_slice(&frame.to_be_bytes());
	Ok(out)
}

/// Encrypt `plaintext` under AES-128-GCM with empty AAD.
///
/// Stateless: does not track reuse, exhaustion, or duplicates.
///
/// # Errors
///
/// [`Error::Identity`] if `group`/`frame` is out of range, [`Error::Oversize`] if
/// plaintext plus tag exceeds `payload_limit`, [`Error::InvalidSecret`] if `key` is
/// not 16 bytes.
pub fn protect(key: &[u8], group: u64, frame: u64, plaintext: &[u8], payload_limit: usize) -> Result<Bytes> {
	let nonce = nonce(group, frame)?;
	if plaintext.len().saturating_add(TAG_LEN) > payload_limit {
		return Err(Error::Oversize);
	}
	let aead = less_safe_key(key)?;
	let mut in_out = Vec::with_capacity(plaintext.len() + TAG_LEN);
	in_out.extend_from_slice(plaintext);
	aead.seal_in_place_append_tag(Nonce::assume_unique_for_key(nonce), Aad::empty(), &mut in_out)
		.expect("AES-128-GCM seal with a valid key and nonce");
	Ok(Bytes::from(in_out))
}

/// Decrypt `payload` (ciphertext concatenated with the 16-byte tag).
///
/// Stateless: does not track reuse, exhaustion, or duplicates.
///
/// # Errors
///
/// [`Error::Identity`] if `group`/`frame` is out of range, [`Error::Oversize`] if
/// the payload is shorter than the tag or larger than `payload_limit`,
/// [`Error::Authentication`] if the tag fails, [`Error::InvalidSecret`] if `key`
/// is not 16 bytes.
pub fn open(key: &[u8], group: u64, frame: u64, payload: &[u8], payload_limit: usize) -> Result<Bytes> {
	let nonce = nonce(group, frame)?;
	if payload.len() < TAG_LEN || payload.len() > payload_limit {
		return Err(Error::Oversize);
	}
	let aead = less_safe_key(key)?;
	let mut in_out = payload.to_vec();
	let plaintext = aead
		.open_in_place(Nonce::assume_unique_for_key(nonce), Aad::empty(), &mut in_out)
		.map_err(|_| Error::Authentication)?;
	let len = plaintext.len();
	in_out.truncate(len);
	Ok(Bytes::from(in_out))
}

pub(crate) fn less_safe_key(key: &[u8]) -> Result<LessSafeKey> {
	if key.len() != KEY_LEN {
		return Err(Error::InvalidSecret);
	}
	let unbound = UnboundKey::new(&AES_128_GCM, key).map_err(|_| Error::InvalidSecret)?;
	Ok(LessSafeKey::new(unbound))
}
