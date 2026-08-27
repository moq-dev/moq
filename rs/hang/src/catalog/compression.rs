use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// The compression applied to a metadata track's frames.
///
/// A track's catalog entry declares this explicitly; the conventional `.z` suffix on a track name
/// is a naming convention, not a signal a consumer may rely on. An absent field (`None` on the
/// config) means the frames are uncompressed.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Compression {
	/// Group-scoped raw DEFLATE ([RFC 1951](https://www.rfc-editor.org/rfc/rfc1951.html)),
	/// sync-flushed at each frame boundary so every frame is self-delimited while later frames
	/// compress against the earlier ones in the same group.
	Deflate,

	/// A compression this build does not recognize, preserved verbatim.
	///
	/// A consumer MUST ignore the track, since its frames cannot be decoded.
	Unknown(String),
}

impl Compression {
	/// The compression as it appears on the wire.
	pub fn as_str(&self) -> &str {
		match self {
			Self::Deflate => "deflate",
			Self::Unknown(other) => other,
		}
	}
}

impl Serialize for Compression {
	fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
		serializer.serialize_str(self.as_str())
	}
}

impl<'de> Deserialize<'de> for Compression {
	fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
		let raw = String::deserialize(deserializer)?;
		Ok(match raw.as_str() {
			"deflate" => Self::Deflate,
			_ => Self::Unknown(raw),
		})
	}
}

#[cfg(test)]
mod test {
	use super::*;

	#[test]
	fn deflate_roundtrip() {
		assert_eq!(serde_json::to_string(&Compression::Deflate).unwrap(), r#""deflate""#);
		assert_eq!(
			serde_json::from_str::<Compression>(r#""deflate""#).unwrap(),
			Compression::Deflate
		);
	}

	#[test]
	fn unknown_roundtrip() {
		let parsed: Compression = serde_json::from_str(r#""zstd""#).unwrap();
		assert_eq!(parsed, Compression::Unknown("zstd".to_string()));
		assert_eq!(serde_json::to_string(&parsed).unwrap(), r#""zstd""#);
	}
}
