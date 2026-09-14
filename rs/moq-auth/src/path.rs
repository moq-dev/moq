//! Segment-aware root matching, mirroring `moq_net::Path`.
//!
//! A token's root and a connection's path are literal paths, so overlapping them is
//! prefix arithmetic; everything beneath is a pattern from `moq-pattern`, re-exported
//! from this crate so a token minting service does not pull in the wire stack. The
//! normalization and boundary rules must stay identical to `moq_net::Path`, which the
//! relay applies to the same strings.
//!
//! Every function below assumes its arguments are already [`normalize`]d.

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
	fn test_strip_prefix() {
		assert_eq!(strip_prefix("foo/bar", ""), Some("foo/bar"));
		assert_eq!(strip_prefix("foo/bar", "foo"), Some("bar"));
		assert_eq!(strip_prefix("foo/bar", "foo/bar"), Some(""));
		assert_eq!(strip_prefix("foobar", "foo"), None);
		assert_eq!(strip_prefix("foo", "bar"), None);
	}
}
