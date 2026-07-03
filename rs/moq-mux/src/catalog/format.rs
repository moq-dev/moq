//! Filename-style format extensions for broadcast names.
//!
//! Broadcast names use a filename-style suffix to advertise their catalog format,
//! e.g. `demo/bbb.hang` or `demo/bbb.msf`. Consumers parse the suffix to pick a
//! catalog track without explicit configuration; publishers should include the
//! suffix in the name they publish so consumers can detect it.

/// The catalog format advertised by a broadcast name suffix.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum CatalogFormat {
	/// `hang` JSON catalog (track `catalog.json`).
	#[default]
	Hang,
	/// DEFLATE-compressed `hang` JSON catalog (track `catalog.json.z`).
	///
	/// Same broadcast-name suffix as [`Hang`](Self::Hang) (`.hang`): the compression is a track-level
	/// choice, not a different broadcast. Opt in explicitly. [`detect`](Self::detect) never returns
	/// this, so name-based auto-detection stays on the uncompressed track until consumers are moved over.
	HangZ,
	/// MSF catalog (track `catalog`).
	Msf,
}

impl CatalogFormat {
	/// The fallback used when a broadcast name has no recognized extension.
	///
	/// Matches `<Self as Default>::default()`.
	pub const DEFAULT: Self = Self::Hang;

	/// The filename-style suffix (including leading dot) for this format.
	pub fn extension(self) -> &'static str {
		match self {
			Self::Hang | Self::HangZ => ".hang",
			Self::Msf => ".msf",
		}
	}

	/// Detect the catalog format from a broadcast name suffix.
	///
	/// Returns `None` if the name has no recognized extension.
	pub fn detect(name: &str) -> Option<Self> {
		if name.ends_with(Self::Hang.extension()) {
			Some(Self::Hang)
		} else if name.ends_with(Self::Msf.extension()) {
			Some(Self::Msf)
		} else {
			None
		}
	}
}

#[cfg(test)]
mod test {
	use super::*;

	#[test]
	fn detect_hang() {
		assert_eq!(CatalogFormat::detect("demo/bbb.hang"), Some(CatalogFormat::Hang));
		assert_eq!(CatalogFormat::detect("bbb.hang"), Some(CatalogFormat::Hang));
	}

	#[test]
	fn detect_msf() {
		assert_eq!(CatalogFormat::detect("demo/bbb.msf"), Some(CatalogFormat::Msf));
	}

	#[test]
	fn detect_none() {
		assert_eq!(CatalogFormat::detect("demo/bbb"), None);
		assert_eq!(CatalogFormat::detect(""), None);
		assert_eq!(CatalogFormat::detect("demo/foo.v2"), None);
	}
}
