use crate::{Path, Version, coding::*};

/// Helper function to encode namespace as tuple of strings
pub fn encode_namespace<W: bytes::BufMut>(w: &mut W, namespace: &Path, version: Version) -> Result<(), EncodeError> {
	// Split the path by '/' to get individual parts
	let path_str = namespace.as_str();
	if path_str.is_empty() {
		0u64.encode(w, version)?;
	} else {
		let parts: Vec<&str> = path_str.split('/').collect();
		(parts.len() as u64).encode(w, version)?;
		for part in parts {
			part.encode(w, version)?;
		}
	}
	Ok(())
}

/// Helper function to decode namespace from tuple of strings
pub fn decode_namespace<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Path<'static>, DecodeError> {
	let count = u64::decode(r, version)? as usize;

	if count == 0 {
		return Ok(Path::from(String::new()));
	}

	let mut parts = Vec::with_capacity(count.min(16));
	for _ in 0..count {
		let part = String::decode(r, version)?;
		parts.push(part);
	}

	Ok(Path::from(parts.join("/")))
}
