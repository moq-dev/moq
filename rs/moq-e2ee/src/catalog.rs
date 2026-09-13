//! Encrypted catalog names and payloads.
//!
//! Semantic names stay off the wire. Compress before AEAD; never encrypt then compress.

use bytes::Bytes;

use crate::credential::Credential;
use crate::error::Result;
use crate::key::TrackKey;
use crate::limits::MAX_GROUPED_PAYLOAD;

/// Hang catalog track semantic name.
pub const JSON: &str = "catalog.json";

/// Compressed Hang catalog track semantic name.
pub const JSON_Z: &str = "catalog.json.z";

/// MSF catalog track semantic name.
pub const MSF: &str = "catalog";

/// Encrypt catalog bytes as grouped frame 0 of group 0.
///
/// Compression, if any, must already have been applied.
///
/// Single-shot per `(credential, semantic)`: protecting the same semantic name
/// twice reuses the `(group 0, frame 0)` nonce with the same key. Publish live
/// catalog updates through [`crate::track::Producer`] instead.
///
/// # Errors
///
/// Profile protect errors.
pub fn protect(credential: &Credential, semantic: &str, plaintext: &[u8]) -> Result<Bytes> {
	let physical = credential.physical_name(semantic)?;
	let mut key = TrackKey::derive(credential, &physical, crate::Domain::Group)?;
	key.protect(0, 0, plaintext, MAX_GROUPED_PAYLOAD)
}

/// Decrypt a catalog payload as grouped frame 0 of group 0.
///
/// Only opens the single-shot snapshot from [`protect`]; later catalog groups
/// on a live track must be opened through [`crate::track::Consumer`].
///
/// # Errors
///
/// Profile open errors.
pub fn open(credential: &Credential, semantic: &str, payload: &[u8]) -> Result<Bytes> {
	let physical = credential.physical_name(semantic)?;
	let mut key = TrackKey::derive(credential, &physical, crate::Domain::Group)?;
	key.open(0, 0, payload, MAX_GROUPED_PAYLOAD)
}

/// Compress with group-scoped DEFLATE, then encrypt under [`JSON_Z`].
///
/// # Errors
///
/// Profile protect errors.
pub fn protect_deflate(credential: &Credential, plaintext: &[u8]) -> Result<Bytes> {
	let compressed = moq_flate::Encoder::new().frame(plaintext);
	protect(credential, JSON_Z, &compressed)
}

/// Decrypt a [`JSON_Z`] payload, then decompress.
///
/// # Errors
///
/// Profile open errors, or a deflate error mapped to [`Error::Authentication`](crate::Error::Authentication)
/// so a malformed compressed catalog cannot be distinguished from a bad tag by a relay.
pub fn open_deflate(credential: &Credential, payload: &[u8]) -> Result<Bytes> {
	let compressed = open(credential, JSON_Z, payload)?;
	moq_flate::Decoder::new()
		.frame(&compressed)
		.map_err(|_| crate::Error::Authentication)
}
