//! Segment-aware path matching, mirroring `moq_net::Path`.
//!
//! moq-token is deliberately free of moq-* dependencies so a token minting service
//! doesn't pull in the wire stack, so the handful of prefix operations
//! [`Claims::authorize`](crate::Claims::authorize) needs live here instead. The
//! normalization and boundary rules must stay identical to `moq_net::Path`, which
//! the relay applies to the same strings.
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

/// True when `path` starts with `prefix` on a segment boundary, so `foo` does
/// not match `foobar`. The empty prefix matches everything.
pub fn has_prefix(path: &str, prefix: &str) -> bool {
	strip_prefix(path, prefix).is_some()
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

/// Join two relative paths, skipping the delimiter when either side is empty.
pub fn join(base: &str, other: &str) -> String {
	match (base.is_empty(), other.is_empty()) {
		(true, _) => other.to_string(),
		(_, true) => base.to_string(),
		_ => format!("{base}/{other}"),
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
	fn test_has_prefix() {
		assert!(has_prefix("foo/bar", ""));
		assert!(has_prefix("foo/bar", "foo"));
		assert!(has_prefix("foo/bar", "foo/bar"));
		assert!(!has_prefix("foo/bar", "fo"));
		assert!(!has_prefix("foobar", "foo"));
		assert!(!has_prefix("foo", "foo/bar"));
	}

	#[test]
	fn test_strip_prefix() {
		assert_eq!(strip_prefix("foo/bar", ""), Some("foo/bar"));
		assert_eq!(strip_prefix("foo/bar", "foo"), Some("bar"));
		assert_eq!(strip_prefix("foo/bar", "foo/bar"), Some(""));
		assert_eq!(strip_prefix("foobar", "foo"), None);
		assert_eq!(strip_prefix("foo", "bar"), None);
	}

	#[test]
	fn test_join() {
		assert_eq!(join("", ""), "");
		assert_eq!(join("foo", ""), "foo");
		assert_eq!(join("", "bar"), "bar");
		assert_eq!(join("foo", "bar"), "foo/bar");
	}
}
