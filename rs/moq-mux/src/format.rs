//! Filename-style format extensions for broadcast names.
//!
//! Broadcast names use a filename-style suffix to advertise their catalog format,
//! e.g. `demo/bbb.hang` or `demo/bbb.msf`. Producers append the suffix when missing,
//! consumers parse it to pick a catalog track without explicit configuration.

/// The catalog format advertised by a broadcast name suffix.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum CatalogFormat {
	/// `hang` JSON catalog (track `catalog.json`).
	Hang,
	/// MSF catalog (track `catalog`).
	Msf,
}

impl CatalogFormat {
	/// The fallback used when a broadcast name has no recognized extension.
	pub const DEFAULT: Self = Self::Hang;

	/// The filename-style suffix (including leading dot) for this format.
	pub fn extension(self) -> &'static str {
		match self {
			Self::Hang => ".hang",
			Self::Msf => ".msf",
		}
	}
}

/// Detect the catalog format from a broadcast name suffix.
///
/// Returns `None` if the name has no recognized extension.
pub fn detect(name: &str) -> Option<CatalogFormat> {
	if name.ends_with(CatalogFormat::Hang.extension()) {
		Some(CatalogFormat::Hang)
	} else if name.ends_with(CatalogFormat::Msf.extension()) {
		Some(CatalogFormat::Msf)
	} else {
		None
	}
}

/// Return `name` unchanged if it already has a recognized extension,
/// otherwise append `default.extension()`.
pub fn ensure(name: &str, default: CatalogFormat) -> String {
	if detect(name).is_some() {
		name.to_string()
	} else {
		format!("{}{}", name, default.extension())
	}
}

#[cfg(test)]
mod test {
	use super::*;

	#[test]
	fn detect_hang() {
		assert_eq!(detect("demo/bbb.hang"), Some(CatalogFormat::Hang));
		assert_eq!(detect("bbb.hang"), Some(CatalogFormat::Hang));
	}

	#[test]
	fn detect_msf() {
		assert_eq!(detect("demo/bbb.msf"), Some(CatalogFormat::Msf));
	}

	#[test]
	fn detect_none() {
		assert_eq!(detect("demo/bbb"), None);
		assert_eq!(detect(""), None);
		assert_eq!(detect("demo/foo.v2"), None);
	}

	#[test]
	fn ensure_appends_default() {
		assert_eq!(ensure("demo/bbb", CatalogFormat::Hang), "demo/bbb.hang");
		assert_eq!(ensure("", CatalogFormat::Hang), ".hang");
		assert_eq!(ensure("demo/foo.v2", CatalogFormat::Hang), "demo/foo.v2.hang");
	}

	#[test]
	fn ensure_appends_msf_default() {
		assert_eq!(ensure("demo/bbb", CatalogFormat::Msf), "demo/bbb.msf");
	}

	#[test]
	fn ensure_keeps_existing_extension() {
		assert_eq!(ensure("demo/bbb.hang", CatalogFormat::Hang), "demo/bbb.hang");
		assert_eq!(ensure("demo/bbb.msf", CatalogFormat::Hang), "demo/bbb.msf");
		assert_eq!(ensure("demo/bbb.hang", CatalogFormat::Msf), "demo/bbb.hang");
	}
}
