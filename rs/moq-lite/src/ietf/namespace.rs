use crate::{Path, coding::*};

/// Helper function to encode namespace as tuple of strings
pub fn encode_namespace<W: bytes::BufMut, V: Clone>(w: &mut W, namespace: &Path, version: V) {
	// Split the path by '/' to get individual parts
	let path_str = namespace.as_str();
	if path_str.is_empty() {
		0u64.encode(w, version);
	} else {
		let parts: Vec<&str> = path_str.split('/').collect();
		(parts.len() as u64).encode(w, version.clone());
		for part in parts {
			part.encode(w, version.clone());
		}
	}
}

/// Helper function to decode namespace from tuple of strings
pub fn decode_namespace<R: bytes::Buf, V: Clone>(r: &mut R, version: V) -> Result<Path<'static>, DecodeError> {
	let count = u64::decode(r, version.clone())? as usize;

	if count == 0 {
		return Ok(Path::from(String::new()));
	}

	let mut parts = Vec::with_capacity(count.min(16));
	for _ in 0..count {
		let part = String::decode(r, version.clone())?;
		parts.push(part);
	}

	Ok(Path::from(parts.join("/")))
}
