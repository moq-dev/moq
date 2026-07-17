use crate::path;
use serde::{Deserialize, Serialize};
use serde_with::{OneOrMany, TimestampSeconds, formats::PreferMany, serde_as};

/// The access a [`Claims`] grants at a specific path, with every prefix rebased so
/// it is relative to that path.
///
/// Produced by [`Claims::authorize`]. An empty string grants the path itself and
/// everything beneath it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Permissions {
	/// Paths the holder may subscribe to, relative to the authorized path.
	pub subscribe: Vec<String>,

	/// Paths the holder may publish to, relative to the authorized path.
	pub publish: Vec<String>,
}

/// The payload of a token: a root, plus the publish/subscribe prefixes granted beneath it.
///
/// Build one from [`Default`] with the `with_*` setters, sign it with
/// [`Key::sign`](crate::Key::sign), and scope it to a connection with
/// [`authorize`](Self::authorize).
///
/// ```no_run
/// let claims = moq_token::Claims::default()
///     .with_root("room/123")
///     .with_publish(["alice"])
///     .with_subscribe([""]);
/// ```
#[serde_with::skip_serializing_none]
#[serde_as]
#[derive(Debug, Serialize, Deserialize, Default, Clone)]
#[serde(default)]
#[non_exhaustive]
pub struct Claims {
	/// The root for the publish/subscribe options below.
	/// It's mostly for compression and is optional, defaulting to the empty string.
	#[serde(default, rename = "root", skip_serializing_if = "String::is_empty")]
	pub root: String,

	/// If specified, the user can publish any matching broadcasts.
	/// If not specified, the user will not publish any broadcasts.
	#[serde(default, rename = "put", skip_serializing_if = "Vec::is_empty")]
	#[serde_as(as = "OneOrMany<_, PreferMany>")]
	pub publish: Vec<String>,

	/// If specified, the user can subscribe to any matching broadcasts.
	/// If not specified, the user will not receive announcements and cannot subscribe to any broadcasts.
	// NOTE: This can't be renamed to "sub" because that's a reserved JWT field.
	#[serde(default, rename = "get", skip_serializing_if = "Vec::is_empty")]
	#[serde_as(as = "OneOrMany<_, PreferMany>")]
	pub subscribe: Vec<String>,

	/// The expiration time of the token as a unix timestamp.
	#[serde(rename = "exp")]
	#[serde_as(as = "Option<TimestampSeconds<i64>>")]
	pub expires: Option<std::time::SystemTime>,

	/// The issued time of the token as a unix timestamp.
	#[serde(rename = "iat")]
	#[serde_as(as = "Option<TimestampSeconds<i64>>")]
	pub issued: Option<std::time::SystemTime>,
}

impl Claims {
	/// Set the root that the publish/subscribe prefixes are relative to.
	pub fn with_root(mut self, root: impl Into<String>) -> Self {
		self.root = root.into();
		self
	}

	/// Grant publish access to these prefixes, relative to the root.
	pub fn with_publish(mut self, paths: impl IntoIterator<Item = impl Into<String>>) -> Self {
		self.publish = paths.into_iter().map(Into::into).collect();
		self
	}

	/// Grant subscribe access to these prefixes, relative to the root.
	pub fn with_subscribe(mut self, paths: impl IntoIterator<Item = impl Into<String>>) -> Self {
		self.subscribe = paths.into_iter().map(Into::into).collect();
		self
	}

	/// Expire the token at this time. Enforced by [`Key::verify`](crate::Key::verify).
	///
	/// Accepts an `Option` so a caller can pass one through without unwrapping it.
	pub fn with_expires(mut self, at: impl Into<Option<std::time::SystemTime>>) -> Self {
		self.expires = at.into();
		self
	}

	/// Record when the token was issued. Purely informational; nothing enforces it.
	///
	/// Accepts an `Option` so a caller can pass one through without unwrapping it.
	pub fn with_issued(mut self, at: impl Into<Option<std::time::SystemTime>>) -> Self {
		self.issued = at.into();
		self
	}

	/// Returns an error when the token grants nothing at all, making it useless.
	pub fn validate(&self) -> crate::Result<()> {
		if self.publish.is_empty() && self.subscribe.is_empty() {
			return Err(crate::Error::UselessToken);
		}

		Ok(())
	}

	/// The access these claims grant at `path`, rebased so each returned prefix is
	/// relative to `path`.
	///
	/// `path` and [`root`](Self::root) must overlap, in either direction:
	///
	/// - `path` extends the root (root `demo`, path `demo/room`), so the extra
	///   `room` narrows each prefix and drops the ones outside it.
	/// - `path` is a parent of the root (root `demo`, path ``), so `demo` is
	///   prepended to each prefix to keep it anchored where the token points.
	///
	/// Matching is segment-aware, so a root of `foo` does not cover `foobar`.
	/// Slashes at the boundaries are implicit: `/demo/` and `demo` are the same path.
	///
	/// Returns [`Error::RootMismatch`](crate::Error::RootMismatch) when the two don't
	/// overlap, and [`Error::NoAccess`](crate::Error::NoAccess) when they do but every
	/// prefix falls outside `path`.
	///
	/// This is authorization only. Verify the signature first with
	/// [`Key::verify`](crate::Key::verify), which is where expiry is enforced.
	pub fn authorize(&self, path: &str) -> crate::Result<Permissions> {
		let path = path::normalize(path);
		let root = path::normalize(&self.root);

		// Exactly one of these is non-empty: `suffix` is how far the path reaches
		// past the root, `prefix` is how far the root reaches past the path.
		let (suffix, prefix) = if let Some(suffix) = path::strip_prefix(&path, &root) {
			(suffix, "")
		} else if let Some(prefix) = path::strip_prefix(&root, &path) {
			("", prefix)
		} else {
			return Err(crate::Error::RootMismatch(path));
		};

		let scope = |paths: &[String]| -> Vec<String> {
			paths
				.iter()
				.filter_map(|granted| {
					let granted = path::join(prefix, &path::normalize(granted));

					if let Some(remaining) = path::strip_prefix(&granted, suffix) {
						// The grant covers the path; keep what's left below it.
						Some(remaining.to_string())
					} else if path::has_prefix(suffix, &granted) {
						// The grant stops short of the path but still contains it,
						// so everything below the path is granted.
						Some(String::new())
					} else {
						None
					}
				})
				.collect()
		};

		let permissions = Permissions {
			subscribe: scope(&self.subscribe),
			publish: scope(&self.publish),
		};

		if permissions.subscribe.is_empty() && permissions.publish.is_empty() {
			return Err(crate::Error::NoAccess(path));
		}

		Ok(permissions)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	use std::time::{Duration, SystemTime};

	fn create_test_claims() -> Claims {
		Claims {
			root: "test-path".to_string(),
			publish: vec!["test-pub".into()],
			subscribe: vec!["test-sub".into()],
			expires: Some(SystemTime::now() + Duration::from_secs(3600)),
			issued: Some(SystemTime::now()),
		}
	}

	#[test]
	fn test_claims_validation_success() {
		let claims = create_test_claims();
		assert!(claims.validate().is_ok());
	}

	#[test]
	fn test_claims_validation_no_publish_or_subscribe() {
		let claims = Claims {
			root: "test-path".to_string(),
			publish: vec![],
			subscribe: vec![],
			expires: None,
			issued: None,
		};

		let result = claims.validate();
		assert!(result.is_err());
		assert!(
			result
				.unwrap_err()
				.to_string()
				.contains("no publish or subscribe allowed; token is useless")
		);
	}

	#[test]
	fn test_claims_validation_only_publish() {
		let claims = Claims {
			root: "test-path".to_string(),
			publish: vec!["test-pub".into()],
			subscribe: vec![],
			expires: None,
			issued: None,
		};

		assert!(claims.validate().is_ok());
	}

	#[test]
	fn test_claims_validation_only_subscribe() {
		let claims = Claims {
			root: "test-path".to_string(),
			publish: vec![],
			subscribe: vec!["test-sub".into()],
			expires: None,
			issued: None,
		};

		assert!(claims.validate().is_ok());
	}

	#[test]
	fn test_claims_validation_path_not_prefix_relative_publish() {
		let claims = Claims {
			root: "test-path".to_string(),        // no trailing slash
			publish: vec!["relative-pub".into()], // relative path without leading slash
			subscribe: vec![],
			expires: None,
			issued: None,
		};

		let result = claims.validate();
		assert!(result.is_ok()); // Now passes because slashes are implicitly added
	}

	#[test]
	fn test_claims_validation_path_not_prefix_relative_subscribe() {
		let claims = Claims {
			root: "test-path".to_string(), // no trailing slash
			publish: vec![],
			subscribe: vec!["relative-sub".into()], // relative path without leading slash
			expires: None,
			issued: None,
		};

		let result = claims.validate();
		assert!(result.is_ok()); // Now passes because slashes are implicitly added
	}

	#[test]
	fn test_claims_validation_path_not_prefix_absolute_publish() {
		let claims = Claims {
			root: "test-path".to_string(),         // no trailing slash
			publish: vec!["/absolute-pub".into()], // absolute path with leading slash
			subscribe: vec![],
			expires: None,
			issued: None,
		};

		assert!(claims.validate().is_ok());
	}

	#[test]
	fn test_claims_validation_path_not_prefix_absolute_subscribe() {
		let claims = Claims {
			root: "test-path".to_string(), // no trailing slash
			publish: vec![],
			subscribe: vec!["/absolute-sub".into()], // absolute path with leading slash
			expires: None,
			issued: None,
		};

		assert!(claims.validate().is_ok());
	}

	#[test]
	fn test_claims_validation_path_not_prefix_empty_publish() {
		let claims = Claims {
			root: "test-path".to_string(), // no trailing slash
			publish: vec!["".into()],      // empty string
			subscribe: vec![],
			expires: None,
			issued: None,
		};

		assert!(claims.validate().is_ok());
	}

	#[test]
	fn test_claims_validation_path_not_prefix_empty_subscribe() {
		let claims = Claims {
			root: "test-path".to_string(), // no trailing slash
			publish: vec![],
			subscribe: vec!["".into()], // empty string
			expires: None,
			issued: None,
		};

		assert!(claims.validate().is_ok());
	}

	#[test]
	fn test_claims_validation_path_is_prefix() {
		let claims = Claims {
			root: "test-path".to_string(),          // with trailing slash
			publish: vec!["relative-pub".into()],   // relative path is ok when path is prefix
			subscribe: vec!["relative-sub".into()], // relative path is ok when path is prefix
			expires: None,
			issued: None,
		};

		assert!(claims.validate().is_ok());
	}

	#[test]
	fn test_claims_validation_empty_path() {
		let claims = Claims {
			root: "".to_string(), // empty path
			publish: vec!["test-pub".into()],
			subscribe: vec![],
			expires: None,
			issued: None,
		};

		assert!(claims.validate().is_ok());
	}

	#[test]
	fn test_claims_serde() {
		let claims = create_test_claims();
		let json = serde_json::to_string(&claims).unwrap();
		let deserialized: Claims = serde_json::from_str(&json).unwrap();

		assert_eq!(deserialized.root, claims.root);
		assert_eq!(deserialized.publish, claims.publish);
		assert_eq!(deserialized.subscribe, claims.subscribe);
	}

	#[test]
	fn test_claims_default() {
		let claims = Claims::default();
		assert_eq!(claims.root, "");
		assert!(claims.publish.is_empty());
		assert!(claims.subscribe.is_empty());
		assert_eq!(claims.expires, None);
		assert_eq!(claims.issued, None);
	}

	fn authorize_claims(root: &str, subscribe: &[&str], publish: &[&str]) -> Claims {
		Claims {
			root: root.to_string(),
			subscribe: subscribe.iter().map(|s| s.to_string()).collect(),
			publish: publish.iter().map(|s| s.to_string()).collect(),
			..Default::default()
		}
	}

	#[test]
	fn test_authorize_path_equals_root() {
		let claims = authorize_claims("room/123", &[""], &["alice"]);
		let permissions = claims.authorize("room/123").unwrap();

		assert_eq!(permissions.subscribe, [""]);
		assert_eq!(permissions.publish, ["alice"]);
	}

	#[test]
	fn test_authorize_path_extends_root() {
		// Connecting below the root consumes the matching part of each grant.
		let claims = authorize_claims("room/123", &["bob"], &["alice"]);
		let permissions = claims.authorize("room/123/alice").unwrap();

		assert_eq!(permissions.subscribe, Vec::<String>::new());
		assert_eq!(permissions.publish, [""]);
	}

	#[test]
	fn test_authorize_path_is_parent_of_root() {
		// Connecting above the root prepends it, keeping the grants anchored.
		let claims = authorize_claims("demo", &[""], &["alice"]);
		let permissions = claims.authorize("/").unwrap();

		assert_eq!(permissions.subscribe, ["demo"]);
		assert_eq!(permissions.publish, ["demo/alice"]);
	}

	#[test]
	fn test_authorize_empty_root() {
		// A root-scoped token grants everything it lists, wherever it connects.
		let claims = authorize_claims("", &["demo"], &[]);
		let permissions = claims.authorize("demo/room").unwrap();

		assert_eq!(permissions.subscribe, [""]);
		assert_eq!(permissions.publish, Vec::<String>::new());
	}

	#[test]
	fn test_authorize_slashes_are_implicit() {
		let claims = authorize_claims("/room/123/", &["/bob/"], &[]);
		let permissions = claims.authorize("//room/123//").unwrap();

		assert_eq!(permissions.subscribe, ["bob"]);
	}

	#[test]
	fn test_authorize_respects_segment_boundaries() {
		// "foo" must not cover "foobar".
		let claims = authorize_claims("foo", &[""], &[""]);
		assert!(matches!(claims.authorize("foobar"), Err(crate::Error::RootMismatch(_))));
	}

	#[test]
	fn test_authorize_unrelated_path() {
		let claims = authorize_claims("demo", &[""], &[""]);
		assert!(matches!(claims.authorize("other"), Err(crate::Error::RootMismatch(_))));
	}

	#[test]
	fn test_authorize_no_access_at_path() {
		// The path overlaps the root, but every grant sits outside it.
		let claims = authorize_claims("", &["demo"], &[]);
		assert!(matches!(claims.authorize("other"), Err(crate::Error::NoAccess(_))));
	}

	#[test]
	fn test_deserialize_string_as_vec() {
		let json = r#"{
			"root": "test",
			"put": "single-publish",
			"get": "single-subscribe"
		}"#;

		let claims: Claims = serde_json::from_str(json).unwrap();
		assert_eq!(claims.publish, vec!["single-publish"]);
		assert_eq!(claims.subscribe, vec!["single-subscribe"]);
	}

	#[test]
	fn test_deserialize_vec_as_vec() {
		let json = r#"{
			"root": "test",
			"put": ["pub1", "pub2"],
			"get": ["sub1", "sub2"]
		}"#;

		let claims: Claims = serde_json::from_str(json).unwrap();
		assert_eq!(claims.publish, vec!["pub1", "pub2"]);
		assert_eq!(claims.subscribe, vec!["sub1", "sub2"]);
	}

	#[test]
	fn test_deserialize_mixed() {
		let json = r#"{
			"root": "test",
			"put": "single",
			"get": ["multi1", "multi2"]
		}"#;

		let claims: Claims = serde_json::from_str(json).unwrap();
		assert_eq!(claims.publish, vec!["single"]);
		assert_eq!(claims.subscribe, vec!["multi1", "multi2"]);
	}
}
