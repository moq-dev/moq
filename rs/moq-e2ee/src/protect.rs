use bytes::Bytes;
use rind::aead;

use crate::error::{Error, Result};
use crate::limits::{TAG_LEN, check_u53};

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
	key: &aead::Key,
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
	key.seal_in_place_append_tag(aead::Nonce::assume_unique_for_key(nonce), &[], &mut in_out)
		.expect("AES-128-GCM seals a payload within the limit");
	Ok(Bytes::from(in_out))
}

/// Decrypt `payload` (ciphertext concatenated with the 16-byte tag).
///
/// Stateless: the caller accounts for exhaustion and duplicates.
pub(crate) fn open(key: &aead::Key, group: u64, frame: u64, payload: &[u8], payload_limit: usize) -> Result<Bytes> {
	let nonce = nonce(group, frame)?;
	if payload.len() < TAG_LEN || payload.len() > payload_limit {
		return Err(Error::Oversize);
	}
	let mut in_out = payload.to_vec();
	let len = key
		.open_in_place(aead::Nonce::assume_unique_for_key(nonce), &[], &mut in_out)
		.map_err(|_| Error::Authentication)?
		.len();
	in_out.truncate(len);
	Ok(Bytes::from(in_out))
}
