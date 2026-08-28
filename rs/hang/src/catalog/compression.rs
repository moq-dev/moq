use crate::Error;

use derive_more::Display;
use std::str::FromStr;

/// The compression applied to a metadata track's frames.
///
/// A track's catalog entry declares this explicitly; the conventional `.z` suffix on a track name
/// is a naming convention, not a signal a consumer may rely on. An absent field (`None` on the
/// config) means the frames are uncompressed.
#[derive(Debug, Clone, PartialEq, Eq, Display)]
#[non_exhaustive]
pub enum Compression {
	/// Group-scoped raw DEFLATE ([RFC 1951](https://www.rfc-editor.org/rfc/rfc1951.html)),
	/// sync-flushed at each frame boundary so every frame is self-delimited while later frames
	/// compress against the earlier ones in the same group.
	#[display("deflate")]
	Deflate,

	/// A compression this build does not recognize, preserved verbatim.
	///
	/// A consumer MUST ignore the track, since its frames cannot be decoded.
	#[display("{_0}")]
	Unknown(String),
}

impl FromStr for Compression {
	type Err = Error;

	fn from_str(s: &str) -> Result<Self, Self::Err> {
		Ok(match s {
			"deflate" => Self::Deflate,
			_ => Self::Unknown(s.to_string()),
		})
	}
}

#[cfg(test)]
mod test {
	use super::*;

	#[test]
	fn deflate_roundtrip() {
		let mut config = crate::catalog::JsonConfig::new(crate::catalog::Mode::Stream);
		config.compression = Some(Compression::Deflate);
		let encoded = serde_json::to_value(&config).unwrap();
		assert_eq!(encoded["compression"], serde_json::json!("deflate"));
		assert_eq!(
			serde_json::from_value::<crate::catalog::JsonConfig>(encoded)
				.unwrap()
				.compression,
			Some(Compression::Deflate)
		);
	}

	#[test]
	fn unknown_roundtrip() {
		let wire = serde_json::json!({ "mode": "stream", "compression": "zstd" });
		let config: crate::catalog::JsonConfig = serde_json::from_value(wire.clone()).unwrap();
		assert_eq!(config.compression, Some(Compression::Unknown("zstd".to_string())));
		assert_eq!(serde_json::to_value(&config).unwrap(), wire);
	}
}
