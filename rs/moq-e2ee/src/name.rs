use std::fmt;
use std::str::FromStr;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rind::{aead, hmac};

use crate::error::{Error, Result};
use crate::limits::{NAME_BLOCK, NAME_NONCE_LEN, TAG_LEN, check_bytes};

/// An opaque physical track name: unpadded base64url of a semantic name sealed under its generation.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct Name(String);

impl Name {
	/// The base64url text, a valid moq-lite track name.
	pub fn as_str(&self) -> &str {
		&self.0
	}

	/// The nonce, sealed padded name, and tag; parsing already checked it decodes.
	fn material(&self) -> Vec<u8> {
		URL_SAFE_NO_PAD.decode(&self.0).expect("parsed as base64url")
	}
}

impl FromStr for Name {
	type Err = Error;

	/// Accept a name learned from a track request, checking syntax only: only its generation can open it.
	///
	/// # Errors
	///
	/// [`Error::Identity`] unless `name` is canonical unpadded base64url decoding to a 12-byte
	/// nonce, a whole number of 16-byte blocks, and a 16-byte tag.
	fn from_str(name: &str) -> Result<Self> {
		// Refuses padding and nonzero trailing bits, so each name has one spelling.
		let material = URL_SAFE_NO_PAD.decode(name).map_err(|_| Error::Identity)?;
		let padded = material
			.len()
			.checked_sub(NAME_NONCE_LEN + TAG_LEN)
			.ok_or(Error::Identity)?;
		if padded == 0 || padded % NAME_BLOCK != 0 {
			return Err(Error::Identity);
		}
		Ok(Self(name.to_string()))
	}
}

impl fmt::Debug for Name {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_tuple("Name").field(&self.as_str()).finish()
	}
}

impl fmt::Display for Name {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.write_str(self.as_str())
	}
}

impl AsRef<str> for Name {
	fn as_ref(&self) -> &str {
		self.as_str()
	}
}

/// A generation's name keys: an HMAC-SHA-256 key for nonces and an AES-128-GCM key for sealing.
pub(crate) struct Keys {
	mac: hmac::Key,
	seal: aead::Key,
}

impl Keys {
	pub(crate) fn new(mac: hmac::Key, seal: aead::Key) -> Self {
		Self { mac, seal }
	}

	/// The physical name for a semantic name: its HMAC nonce, then the padded name sealed under it.
	///
	/// # Errors
	///
	/// [`Error::Identity`] if `semantic` exceeds 65535 bytes.
	pub(crate) fn seal(&self, semantic: &str) -> Result<Name> {
		let mut padded = pad(semantic.as_bytes())?;
		let nonce = self.nonce(&padded);
		self.seal
			.seal_in_place_append_tag(aead::Nonce::assume_unique_for_key(nonce), &[], &mut padded)
			.expect("AES-128-GCM seals a name within the limit");
		let mut material = Vec::with_capacity(NAME_NONCE_LEN + padded.len());
		material.extend_from_slice(&nonce);
		material.extend_from_slice(&padded);
		Ok(Name(URL_SAFE_NO_PAD.encode(material)))
	}

	/// Recover the semantic name of a physical name this generation issued.
	///
	/// # Errors
	///
	/// [`Error::Authentication`] if `name` does not open, its nonce is not the HMAC of its
	/// plaintext, or its padding or UTF-8 is not canonical: no generation issued it.
	#[cfg_attr(
		not(test),
		allow(dead_code, reason = "undeclared names will serve requests through it")
	)]
	pub(crate) fn open(&self, name: &Name) -> Result<String> {
		let mut material = name.material();
		let (nonce, sealed) = material.split_at_mut(NAME_NONCE_LEN);
		let nonce: [u8; NAME_NONCE_LEN] = (&*nonce).try_into().expect("parsed with a nonce");
		let padded = self
			.seal
			.open_in_place(aead::Nonce::assume_unique_for_key(nonce), &[], sealed)
			.map_err(|_| Error::Authentication)?;
		// One physical name per semantic name, so two tracks never share its payload keys.
		if self.nonce(padded) != nonce {
			return Err(Error::Authentication);
		}
		let semantic = unpad(padded)?;
		String::from_utf8(semantic.to_vec()).map_err(|_| Error::Authentication)
	}

	fn nonce(&self, padded: &[u8]) -> [u8; NAME_NONCE_LEN] {
		let tag = hmac::sign(&self.mac, padded);
		tag.as_ref()[..NAME_NONCE_LEN]
			.try_into()
			.expect("HMAC-SHA-256 is 32 bytes")
	}
}

impl fmt::Debug for Keys {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.write_str("name::Keys(<redacted>)")
	}
}

/// `semantic || 0x80 || 0x00...`, with the fewest zeros that reach a multiple of 16 bytes.
pub(crate) fn pad(semantic: &[u8]) -> Result<Vec<u8>> {
	check_bytes(semantic.len())?;
	let len = (semantic.len() / NAME_BLOCK + 1) * NAME_BLOCK;
	// Room for the tag the seal appends in place.
	let mut padded = Vec::with_capacity(len + TAG_LEN);
	padded.extend_from_slice(semantic);
	padded.push(0x80);
	padded.resize(len, 0);
	Ok(padded)
}

/// Strip canonical padding, refusing any other.
fn unpad(padded: &[u8]) -> Result<&[u8]> {
	let end = padded.iter().rposition(|&b| b != 0).ok_or(Error::Authentication)?;
	if padded[end] != 0x80 || padded.len() - end > NAME_BLOCK {
		return Err(Error::Authentication);
	}
	Ok(&padded[..end])
}
