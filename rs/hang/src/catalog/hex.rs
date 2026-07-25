//! Hex encoding for the catalog's binary fields.

use bytes::Bytes;
use serde::de::{Deserializer, Error as _};
use serde::{Deserialize, Serialize, Serializer};
use serde_with::{DeserializeAs, SerializeAs};

/// A [`serde_with`] adapter for the catalog's hex-encoded binary fields (`description`).
///
/// [`serde_with::hex::Hex`] would do the job, but its failure is a bare
/// `Invalid character 'U' at position 1` from the `hex` crate, which never says what it was
/// trying to decode. Naming the expected encoding is the whole point here: the CMAF container's
/// `init` field sitting next to `description` is base64, so which one applies is a real question.
pub(crate) struct Hex;

impl SerializeAs<Bytes> for Hex {
	fn serialize_as<S: Serializer>(source: &Bytes, serializer: S) -> Result<S::Ok, S::Error> {
		hex::encode(source).serialize(serializer)
	}
}

impl<'de> DeserializeAs<'de, Bytes> for Hex {
	fn deserialize_as<D: Deserializer<'de>>(deserializer: D) -> Result<Bytes, D::Error> {
		let encoded = String::deserialize(deserializer)?;
		hex::decode(&encoded)
			.map(Bytes::from)
			.map_err(|err| D::Error::custom(format_args!("expected hex: {err}")))
	}
}

#[cfg(test)]
mod test {
	use crate::catalog::Catalog;

	// An avcC always starts with 0x01, so base64 encoding one starts with "AU": 'A' is a valid hex
	// digit and 'U' is not. See https://github.com/moq-dev/moq/issues/2509.
	#[test]
	fn base64_description() {
		let json = r#"{"video":{"renditions":{"video/0":{"codec":"avc1.42e02a","container":{"kind":"legacy"},"description":"AUIAKv/hABtnQgAq"}}}}"#;

		let err = Catalog::from_str(json).unwrap_err().to_string();
		assert!(err.contains("expected hex"), "{err}");

		// Also via `from_value`, which is how a consumer reaches this: moq-json reconstructs a
		// Value from the frames and deserializes that. It carries no line or column, so the
		// message is all a consumer gets (moq-json prefixes the field path on its own side).
		let value: serde_json::Value = serde_json::from_str(json).unwrap();
		let err = serde_json::from_value::<Catalog>(value).unwrap_err().to_string();
		assert!(err.contains("expected hex"), "{err}");
	}

	#[test]
	fn hex_description() {
		let json = r#"{"video":{"renditions":{"video/0":{"codec":"avc1.42e02a","container":{"kind":"legacy"},"description":"0142002a"}}}}"#;
		let catalog = Catalog::from_str(json).unwrap();
		let rendition = catalog.video.renditions.get("video/0").unwrap();
		assert_eq!(rendition.description.as_deref(), Some(&[0x01, 0x42, 0x00, 0x2a][..]));

		// Round-trips as lowercase hex.
		assert!(catalog.to_json().unwrap().contains(r#""description":"0142002a""#));
	}
}
