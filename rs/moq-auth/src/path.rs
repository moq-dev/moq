//! Segment-aware root matching, mirroring `moq_net::Path`.
//!
//! A token's root and a connection's path are literal paths, so overlapping them is
//! prefix arithmetic; everything beneath is a pattern from `moq-pattern`, re-exported
//! from this crate so a token minting service does not pull in the wire stack. The
//! boundary rules must stay identical to `moq_net::Path`, which the relay applies to
//! the same strings. `moq_net::Path` refuses the empty segments [`normalize`] drops, so
//! a string spelled with one is refused there rather than read as another path. Minting
//! checks a root with [`validate`] instead, so a token never names a path no peer can spell.
//!
//! Every function below but [`validate`] assumes its arguments are already [`normalize`]d.

/// The most bytes a path may hold, matching `moq_net::Path::MAX_BYTES`.
const MAX_BYTES: usize = 4096;

/// Refuse a root `moq_net::Path::new` would: one longer than 4096 bytes, with a leading,
/// trailing, or doubled `/`, or with a `\` before anything but `\` or `/`.
pub fn validate(root: &str) -> crate::Result<()> {
	let invalid = |reason: &str| Err(crate::Error::InvalidRoot(format!("{root:?} {reason}")));
	if root.len() > MAX_BYTES {
		return invalid("is longer than 4096 bytes");
	}
	if !root.is_empty() && (root.starts_with('/') || root.ends_with('/') || root.contains("//")) {
		return invalid("contains an empty segment");
	}
	let mut rest = root.as_bytes();
	while let Some(i) = rest.iter().position(|&b| b == b'\\') {
		match rest.get(i + 1) {
			Some(b'\\' | b'/') => rest = &rest[i + 2..],
			_ => return invalid(r"contains a `\` that escapes neither `\` nor `/`"),
		}
	}
	Ok(())
}

/// Trim leading and trailing slashes and collapse consecutive ones, so all
/// slashes are implicit at boundaries and `/foo//bar/` == `foo/bar`.
pub fn normalize(path: &str) -> String {
	path.split('/')
		.filter(|part| !part.is_empty())
		.collect::<Vec<_>>()
		.join("/")
}

/// `path` with `prefix` and its trailing delimiter removed, or `None` when
/// `prefix` isn't a segment-aligned prefix of `path`.
pub fn strip_prefix<'a>(path: &'a str, prefix: &str) -> Option<&'a str> {
	if prefix.is_empty() {
		return Some(path);
	}

	let rest = path.strip_prefix(prefix)?;
	match rest.as_bytes().first() {
		None => Some(""),
		Some(b'/') => Some(&rest[1..]),
		Some(_) => None,
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn test_normalize() {
		assert_eq!(normalize(""), "");
		assert_eq!(normalize("/"), "");
		assert_eq!(normalize("foo"), "foo");
		assert_eq!(normalize("/foo/bar/"), "foo/bar");
		assert_eq!(normalize("//foo///bar//"), "foo/bar");
	}

	#[test]
	fn test_validate() {
		for root in ["", "foo", "foo/bar", r"a\\b", r"a\/b", ".hidden/x"] {
			assert!(validate(root).is_ok(), "{root:?}");
		}
		let long = "a".repeat(MAX_BYTES + 1);
		for root in ["/", "/foo", "foo/", "foo//bar", r"a\b", r"a\", long.as_str()] {
			assert!(matches!(validate(root), Err(crate::Error::InvalidRoot(_))), "{root:?}");
		}
		assert!(validate(&long[1..]).is_ok());
	}

	#[test]
	fn test_strip_prefix() {
		assert_eq!(strip_prefix("foo/bar", ""), Some("foo/bar"));
		assert_eq!(strip_prefix("foo/bar", "foo"), Some("bar"));
		assert_eq!(strip_prefix("foo/bar", "foo/bar"), Some(""));
		assert_eq!(strip_prefix("foobar", "foo"), None);
		assert_eq!(strip_prefix("foo", "bar"), None);
	}
}
