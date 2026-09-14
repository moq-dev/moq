use crate::path;
use moq_pattern::Patterns;
use serde::{Deserialize, Serialize};
use serde_with::{TimestampSeconds, serde_as};

/// The immutable ceiling on what a key may grant, embedded in its JWK.
///
/// Patterns in `publish` and `subscribe` are relative to `root`, matching token claim
/// semantics. A key signs a token only when every pattern the token grants is
/// contained by one the scope allows, in the same role; see [`allows`](Self::allows).
///
/// The scope is fixed at key generation. Widening it means minting a new key, which
/// is the point: a leaked scoped key can never be talked into signing more than it
/// already could. A key with no scope at all is unrestricted, so keys minted before
/// scopes existed keep working.
#[derive(Debug, Serialize, Deserialize, Default, Clone, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct Scope {
	/// The root for the publish/subscribe patterns below.
	#[serde(skip_serializing_if = "String::is_empty")]
	pub root: String,

	/// Patterns this key may grant to publishers.
	#[serde(skip_serializing_if = "Patterns::is_empty")]
	pub publish: Patterns,

	/// Patterns this key may grant to subscribers.
	#[serde(skip_serializing_if = "Patterns::is_empty")]
	pub subscribe: Patterns,
}

impl Scope {
	/// Returns an error when the scope permits nothing, making the key unusable.
	pub fn validate(&self) -> crate::Result<()> {
		if self.publish.is_empty() && self.subscribe.is_empty() {
			return Err(crate::Error::UselessScope);
		}

		Ok(())
	}

	/// Whether every pattern `claims` grants is covered by this scope, per role.
	///
	/// Both sides are placed beneath their own root before comparing, so the same
	/// grant expressed as `root: "demo"` + `publish: ["room/**"]` or as
	/// `publish: ["demo/room/**"]` is treated identically. Containment is per pattern,
	/// so a scope of `live/**` does not cover `lively/**`, and the roles are checked
	/// independently: a publish-only scope never authorizes a subscribe grant.
	///
	/// `**` covers everything beneath the scope root, so a scope of `root: "demo"` +
	/// `publish: ["**"]` grants publish anywhere under `demo`.
	pub fn allows(&self, claims: &Claims) -> bool {
		let covers = |granted: &Patterns, requested: &Patterns| {
			match (granted.rooted(&self.root), requested.rooted(&claims.root)) {
				(Ok(granted), Ok(requested)) => granted.covers(&requested),
				// A root too deep to place the patterns beneath cannot be granted either way.
				_ => false,
			}
		};

		covers(&self.publish, &claims.publish) && covers(&self.subscribe, &claims.subscribe)
	}
}

/// The access a [`Claims`] grants at a specific path, with every pattern rebased so
/// it is relative to that path.
///
/// Produced by [`Claims::authorize`]. `**` grants the path itself and everything
/// beneath it; the empty pattern grants exactly the path.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Permissions {
	/// Patterns the holder may subscribe to, relative to the authorized path.
	pub subscribe: Patterns,

	/// Patterns the holder may publish to, relative to the authorized path.
	pub publish: Patterns,
}

/// The payload of a token: a root, plus the publish/subscribe patterns granted beneath it.
///
/// Build one from [`Default`] with the `with_*` setters, sign it with
/// [`Key::sign`](crate::Key::sign), and scope it to a connection with
/// [`authorize`](Self::authorize). A pattern names exactly what it says: `alice`
/// is one broadcast, `alice/**` is a subtree, and `**` is everything under the root.
///
/// ```no_run
/// let claims = moq_auth::Claims::default()
///     .with_root("room/123")
///     .with_publish(["alice/**".parse().unwrap()])
///     .with_subscribe(["**".parse().unwrap()]);
/// ```
///
/// Any other field, including the retired `put` and `get` prefix lists, fails
/// verification: a token either speaks patterns or it is not one of ours.
#[serde_with::skip_serializing_none]
#[serde_as]
#[derive(Debug, Serialize, Deserialize, Default, Clone)]
#[serde(default, deny_unknown_fields)]
#[non_exhaustive]
pub struct Claims {
	/// The root for the publish/subscribe patterns below.
	/// It's mostly for compression and is optional, defaulting to the empty string.
	#[serde(skip_serializing_if = "String::is_empty")]
	pub root: String,

	/// If specified, the user can publish any matching broadcasts.
	/// If not specified, the user will not publish any broadcasts.
	#[serde(skip_serializing_if = "Patterns::is_empty")]
	pub publish: Patterns,

	/// If specified, the user can subscribe to any matching broadcasts.
	/// If not specified, the user will not receive announcements and cannot subscribe to any broadcasts.
	#[serde(skip_serializing_if = "Patterns::is_empty")]
	pub subscribe: Patterns,

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
	/// Set the root that the publish/subscribe patterns are relative to.
	pub fn with_root(mut self, root: impl Into<String>) -> Self {
		self.root = root.into();
		self
	}

	/// Grant publish access to these patterns, relative to the root.
	pub fn with_publish(mut self, patterns: impl IntoIterator<Item = moq_pattern::Pattern>) -> Self {
		self.publish = patterns.into_iter().collect();
		self
	}

	/// Grant subscribe access to these patterns, relative to the root.
	pub fn with_subscribe(mut self, patterns: impl IntoIterator<Item = moq_pattern::Pattern>) -> Self {
		self.subscribe = patterns.into_iter().collect();
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

	/// The access these claims grant at `path`, rebased so each returned pattern is
	/// relative to `path`.
	///
	/// `path` and [`root`](Self::root) must overlap, in either direction:
	///
	/// - `path` extends the root (root `demo`, path `demo/room`), so the extra
	///   `room` narrows each pattern and drops the ones outside it.
	/// - `path` is a parent of the root (root `demo`, path ``), so `demo` is
	///   prepended to each pattern to keep it anchored where the token points.
	///
	/// Matching is segment-aware, so a root of `foo` does not cover `foobar`.
	/// Slashes at the boundaries are implicit: `/demo/` and `demo` are the same path.
	///
	/// Returns [`Error::RootMismatch`](crate::Error::RootMismatch) when the two don't
	/// overlap, and [`Error::NoAccess`](crate::Error::NoAccess) when they do but every
	/// pattern falls outside `path`.
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

		let scope = |patterns: &Patterns| -> crate::Result<Patterns> {
			if prefix.is_empty() {
				// The path reaches into the grant; keep what each pattern says below it.
				Ok(patterns.rebase(suffix))
			} else {
				// The grant sits below the path; name it from there.
				Ok(patterns.rooted(prefix)?)
			}
		};

		let permissions = Permissions {
			subscribe: scope(&self.subscribe)?,
			publish: scope(&self.publish)?,
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

	fn patterns(texts: &[&str]) -> Patterns {
		texts.iter().map(|text| text.parse().unwrap()).collect()
	}

	fn create_test_claims() -> Claims {
		Claims {
			root: "test-path".to_string(),
			publish: patterns(&["test-pub/**"]),
			subscribe: patterns(&["test-sub/**"]),
			expires: Some(SystemTime::now() + Duration::from_secs(3600)),
			issued: Some(SystemTime::now()),
		}
	}

	#[test]
	fn scope_allows_contained_claims() {
		let scope = Scope {
			root: "project".into(),
			publish: patterns(&["live/**"]),
			subscribe: patterns(&["watch/**"]),
		};
		let claims = Claims {
			root: "project/live/room".into(),
			publish: patterns(&["**"]),
			..Default::default()
		};
		assert!(scope.allows(&claims));
	}

	#[test]
	fn scope_rejects_sibling_and_role_escalation() {
		let scope = Scope {
			root: "project".into(),
			publish: patterns(&["live/**"]),
			subscribe: Patterns::new(),
		};
		let sibling = Claims {
			root: "project/lively".into(),
			publish: patterns(&["**"]),
			..Default::default()
		};
		let role = Claims {
			root: "project/live".into(),
			subscribe: patterns(&["**"]),
			..Default::default()
		};
		assert!(!scope.allows(&sibling));
		assert!(!scope.allows(&role));
	}

	#[test]
	fn scope_ignores_how_the_root_is_split() {
		// The same grant, expressed three ways, must compare identically.
		let scope = Scope {
			root: "project".into(),
			publish: patterns(&["live/**"]),
			subscribe: Patterns::new(),
		};

		for claims in [
			Claims {
				root: "project".into(),
				publish: patterns(&["live/room/**"]),
				..Default::default()
			},
			Claims {
				root: String::new(),
				publish: patterns(&["project/live/room/**"]),
				..Default::default()
			},
			Claims {
				root: "/project/live/".into(),
				publish: patterns(&["room/**"]),
				..Default::default()
			},
		] {
			assert!(scope.allows(&claims), "{claims:?}");
		}
	}

	#[test]
	fn scope_rejects_escaping_above_its_root() {
		let scope = Scope {
			root: "project".into(),
			publish: patterns(&["live/**"]),
			subscribe: Patterns::new(),
		};

		// A root above the scope's does not widen it, even though `**` would grant
		// everything within the scope.
		let claims = Claims {
			root: String::new(),
			publish: patterns(&["**"]),
			..Default::default()
		};
		assert!(!scope.allows(&claims));
	}

	#[test]
	fn scope_globstar_grants_everything_beneath_it() {
		let scope = Scope {
			root: "project".into(),
			publish: patterns(&["**"]),
			subscribe: Patterns::new(),
		};
		let claims = Claims {
			root: "project/anything/deep".into(),
			publish: patterns(&["**"]),
			..Default::default()
		};
		assert!(scope.allows(&claims));
	}

	#[test]
	fn scope_requires_every_requested_pattern() {
		// One allowed pattern does not carry an unallowed sibling along with it.
		let scope = Scope {
			root: "project".into(),
			publish: patterns(&["live/**"]),
			subscribe: Patterns::new(),
		};
		let claims = Claims {
			root: "project".into(),
			publish: patterns(&["live/room/**", "other/**"]),
			..Default::default()
		};
		assert!(!scope.allows(&claims));
	}

	#[test]
	fn scope_is_exact_about_a_literal() {
		// `live` is one broadcast; a subtree beneath it is more than the scope grants.
		let scope = Scope {
			root: "project".into(),
			publish: patterns(&["live"]),
			subscribe: Patterns::new(),
		};
		let exact = Claims {
			root: "project".into(),
			publish: patterns(&["live"]),
			..Default::default()
		};
		let subtree = Claims {
			root: "project".into(),
			publish: patterns(&["live/**"]),
			..Default::default()
		};
		assert!(scope.allows(&exact));
		assert!(!scope.allows(&subtree));
	}

	#[test]
	fn scope_without_grants_is_useless() {
		assert!(matches!(Scope::default().validate(), Err(crate::Error::UselessScope)));
	}

	#[test]
	fn scope_refuses_the_old_prefix_fields() {
		let err = serde_json::from_str::<Scope>(r#"{"root":"demo","put":["room"]}"#).unwrap_err();
		assert!(err.to_string().contains("unknown field `put`"), "{err}");
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
			..Default::default()
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
			publish: patterns(&["test-pub"]),
			..Default::default()
		};

		assert!(claims.validate().is_ok());
	}

	#[test]
	fn test_claims_validation_only_subscribe() {
		let claims = Claims {
			root: "test-path".to_string(),
			subscribe: patterns(&["test-sub"]),
			..Default::default()
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
	fn test_claims_serde_names() {
		let claims = Claims {
			root: "live".into(),
			publish: patterns(&["camera1"]),
			subscribe: patterns(&["camera1", "camera2"]),
			..Default::default()
		};
		assert_eq!(
			serde_json::to_string(&claims).unwrap(),
			r#"{"root":"live","publish":["camera1"],"subscribe":["camera1","camera2"]}"#
		);
	}

	#[test]
	fn test_claims_refuse_the_old_prefix_fields() {
		for json in [
			r#"{"root":"test","put":["pub1"]}"#,
			r#"{"root":"test","get":"sub1"}"#,
			r#"{"root":"test","publish":["pub1"],"get":["sub1"]}"#,
		] {
			let err = serde_json::from_str::<Claims>(json).unwrap_err();
			assert!(err.to_string().contains("unknown field"), "{json}: {err}");
		}
	}

	#[test]
	fn test_claims_refuse_a_bad_pattern() {
		let err = serde_json::from_str::<Claims>(r#"{"publish":["a/**/b/**"]}"#).unwrap_err();
		assert!(err.to_string().contains("**"), "{err}");
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
			subscribe: patterns(subscribe),
			publish: patterns(publish),
			..Default::default()
		}
	}

	#[test]
	fn test_authorize_path_equals_root() {
		let claims = authorize_claims("room/123", &["**"], &["alice/**"]);
		let permissions = claims.authorize("room/123").unwrap();

		assert_eq!(permissions.subscribe, patterns(&["**"]));
		assert_eq!(permissions.publish, patterns(&["alice/**"]));
	}

	#[test]
	fn test_authorize_path_extends_root() {
		// Connecting below the root consumes the matching part of each grant.
		let claims = authorize_claims("room/123", &["bob/**"], &["alice/**"]);
		let permissions = claims.authorize("room/123/alice").unwrap();

		assert_eq!(permissions.subscribe, Patterns::new());
		assert_eq!(permissions.publish, patterns(&["**"]));
	}

	#[test]
	fn test_authorize_literal_becomes_the_path_itself() {
		// A literal grant reached exactly is the empty pattern: this path, nothing below.
		let claims = authorize_claims("room", &[], &["alice"]);
		let permissions = claims.authorize("room/alice").unwrap();

		assert_eq!(permissions.publish, patterns(&[""]));
	}

	#[test]
	fn test_authorize_path_is_parent_of_root() {
		// Connecting above the root prepends it, keeping the grants anchored.
		let claims = authorize_claims("demo", &["**"], &["alice/**"]);
		let permissions = claims.authorize("/").unwrap();

		assert_eq!(permissions.subscribe, patterns(&["demo/**"]));
		assert_eq!(permissions.publish, patterns(&["demo/alice/**"]));
	}

	#[test]
	fn test_authorize_empty_root() {
		// A root-scoped token grants everything it lists, wherever it connects.
		let claims = authorize_claims("", &["demo/**"], &[]);
		let permissions = claims.authorize("demo/room").unwrap();

		assert_eq!(permissions.subscribe, patterns(&["**"]));
		assert_eq!(permissions.publish, Patterns::new());
	}

	#[test]
	fn test_authorize_slashes_are_implicit() {
		let claims = authorize_claims("/room/123/", &["bob/**"], &[]);
		let permissions = claims.authorize("//room/123//").unwrap();

		assert_eq!(permissions.subscribe, patterns(&["bob/**"]));
	}

	#[test]
	fn test_authorize_respects_segment_boundaries() {
		// "foo" must not cover "foobar".
		let claims = authorize_claims("foo", &["**"], &["**"]);
		assert!(matches!(claims.authorize("foobar"), Err(crate::Error::RootMismatch(_))));
	}

	#[test]
	fn test_authorize_unrelated_path() {
		let claims = authorize_claims("demo", &["**"], &["**"]);
		assert!(matches!(claims.authorize("other"), Err(crate::Error::RootMismatch(_))));
	}

	#[test]
	fn test_authorize_no_access_at_path() {
		// The path overlaps the root, but every grant sits outside it.
		let claims = authorize_claims("", &["demo/**"], &[]);
		assert!(matches!(claims.authorize("other"), Err(crate::Error::NoAccess(_))));
	}

	#[test]
	fn test_authorize_wildcards_rebase_as_a_set() {
		// `**/chat` reached at `chat` is both the path itself and deeper `**/chat`.
		let claims = authorize_claims("", &["**/chat"], &[]);
		let permissions = claims.authorize("chat").unwrap();
		assert_eq!(permissions.subscribe.len(), 2);
		assert_eq!(permissions.subscribe, patterns(&["", "**/chat"]));
	}
}
