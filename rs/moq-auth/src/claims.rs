use crate::{Access, path};
use moq_pattern::{Prefix, Prefixes};
use serde::{Deserialize, Serialize};

/// The immutable ceiling on what a key may grant, embedded in its JWK.
///
/// Prefixes in `publish` and `subscribe` are relative to `root`, matching token claim
/// semantics. A key signs a token only when every prefix the token grants lies at or
/// beneath one the scope allows, in the same role; see [`allows`](Self::allows).
///
/// The scope is fixed at key generation. Widening it means minting a new key, which
/// is the point: a leaked scoped key can never be talked into signing more than it
/// already could. A key with no scope at all is unrestricted, so keys minted before
/// scopes existed keep working.
///
/// Legacy `put`/`get` scopes load as the same prefixes; a scope is always written as
/// `publish` and `subscribe`.
#[derive(Debug, Serialize, Deserialize, Default, Clone, PartialEq, Eq)]
#[serde(try_from = "crate::wire::Object<crate::wire::Scope>", into = "crate::wire::Scope")]
pub struct Scope {
	/// The root for the publish/subscribe prefixes below.
	pub root: String,

	/// Prefixes this key may grant to publishers.
	pub publish: Prefixes,

	/// Prefixes this key may grant to subscribers.
	pub subscribe: Prefixes,
}

impl Scope {
	/// Returns an error when the scope permits nothing, making the key unusable, or names
	/// a malformed root. [`Key::with_scope`](crate::Key::with_scope) also refuses a root
	/// [`Key::sign`](crate::Key::sign) would.
	pub fn validate(&self) -> crate::Result<()> {
		path::validate(&self.root)?;
		if self.publish.is_empty() && self.subscribe.is_empty() {
			return Err(crate::Error::UselessScope);
		}

		Ok(())
	}

	/// Whether every prefix `claims` grants is covered by this scope, per role.
	///
	/// Both sides are placed beneath their own root before comparing, so the same
	/// grant expressed as `root: "demo"` + `publish: ["room"]` or as
	/// `publish: ["demo/room"]` is treated identically. Coverage is by segment, so a
	/// scope of `live` does not cover `lively`, and the roles are checked
	/// independently: a publish-only scope never authorizes a subscribe grant.
	///
	/// The empty prefix covers everything beneath the scope root, so a scope of
	/// `root: "demo"` + `publish: [""]` grants publish anywhere under `demo`.
	pub fn allows(&self, claims: &Claims) -> bool {
		let covers = |granted: &Prefixes, requested: &Prefixes| {
			match (granted.rooted(&self.root), requested.rooted(&claims.root)) {
				(Ok(granted), Ok(requested)) => granted.covers(&requested),
				// A root too deep to place the prefixes beneath cannot be granted either way.
				_ => false,
			}
		};

		covers(&self.publish, &claims.publish.path) && covers(&self.subscribe, &claims.subscribe.path)
	}
}

/// The access a [`Claims`] grants at a specific path, with every prefix rebased so it
/// is relative to that path.
///
/// Produced by [`Claims::authorize`]. The empty prefix grants the path itself and
/// everything beneath it. The reference server's policy holds its anonymous and mTLS
/// rules as this pair too, authorized at `/`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Permissions {
	/// Prefixes the holder may subscribe to, relative to the authorized path.
	pub subscribe: Prefixes,

	/// Prefixes the holder may publish to, relative to the authorized path.
	pub publish: Prefixes,
}

impl Permissions {
	/// Access granted as these prefix sets.
	pub fn new(publish: impl Into<Prefixes>, subscribe: impl Into<Prefixes>) -> Self {
		Self {
			publish: publish.into(),
			subscribe: subscribe.into(),
		}
	}

	/// Whether nothing is granted, which is a refusal.
	pub fn is_empty(&self) -> bool {
		self.publish.is_empty() && self.subscribe.is_empty()
	}
}

/// The payload of a token: a root, plus the publish/subscribe prefixes granted beneath it.
///
/// Build one from [`Default`] with the `with_*` setters, sign it with
/// [`Key::sign`](crate::Key::sign), and scope it to a connection with
/// [`authorize`](Self::authorize). A prefix covers its path and everything beneath it:
/// `alice` is `alice` and every broadcast under it, and the empty prefix is everything
/// under the root. A grant holds no wildcard: `alice/**` or `*` fails to parse.
///
/// ```no_run
/// let claims = moq_auth::Claims::default()
///     .with_root("room/123")
///     .with_publish(["alice".parse().unwrap()])
///     .with_subscribe([moq_auth::Prefix::all()]);
/// ```
///
/// Grants are written as `publish` and `subscribe`. Legacy `moq-token` claims are read
/// too: each `put`/`get` entry is the same prefix. A capped role is written as
/// `{"path": [...], "rate": bps}`, which every older verifier refuses rather than drop
/// the cap. The registered `iss` and `sub` claims are read and ignored; any other field
/// fails verification, since it might narrow the grant, and a misspelled `root` would
/// otherwise widen it.
#[derive(Debug, Serialize, Deserialize, Default, Clone)]
#[serde(try_from = "crate::wire::Object<crate::wire::Claims>", into = "crate::wire::Claims")]
#[non_exhaustive]
pub struct Claims {
	/// The root for the publish/subscribe prefixes below.
	/// It's mostly for compression and is optional, defaulting to the empty string.
	pub root: String,

	/// What the holder may publish, relative to the root.
	pub publish: Access,

	/// What the holder may subscribe to, relative to the root. A cap bounds what it
	/// receives from its subscriptions.
	pub subscribe: Access,

	/// The expiration time of the token as a unix timestamp (`exp`).
	pub expires: Option<kio::clock::Wall>,

	/// The issued time of the token as a unix timestamp (`iat`).
	pub issued: Option<kio::clock::Wall>,

	/// The time before which the token is refused, as a unix timestamp (`nbf`).
	/// Enforced by [`Key::verify`](crate::Key::verify).
	pub not_before: Option<kio::clock::Wall>,

	/// The token's identifier (`jti`), which its issuer may record to revoke it.
	pub id: Option<crate::TokenId>,
}

impl Claims {
	/// Set the root that the publish/subscribe prefixes are relative to.
	pub fn with_root(mut self, root: impl Into<String>) -> Self {
		self.root = root.into();
		self
	}

	/// Grant publish access to these prefixes, relative to the root, keeping any cap.
	pub fn with_publish(mut self, prefixes: impl IntoIterator<Item = Prefix>) -> Self {
		self.publish.path = prefixes.into_iter().collect();
		self
	}

	/// Grant subscribe access to these prefixes, relative to the root, keeping any cap.
	pub fn with_subscribe(mut self, prefixes: impl IntoIterator<Item = Prefix>) -> Self {
		self.subscribe.path = prefixes.into_iter().collect();
		self
	}

	/// Cap what the holder may publish, in bits per second; `None` lifts the cap.
	pub fn with_publish_rate(mut self, rate: impl Into<Option<crate::Rate>>) -> Self {
		self.publish.rate = rate.into();
		self
	}

	/// Cap what the holder may receive from its subscriptions, in bits per second; `None`
	/// lifts the cap.
	pub fn with_subscribe_rate(mut self, rate: impl Into<Option<crate::Rate>>) -> Self {
		self.subscribe.rate = rate.into();
		self
	}

	/// Expire the token at this time. Enforced by [`Key::verify`](crate::Key::verify).
	///
	/// Accepts an `Option` so a caller can pass one through without unwrapping it.
	pub fn with_expires(mut self, at: impl Into<Option<kio::clock::Wall>>) -> Self {
		self.expires = at.into();
		self
	}

	/// Record when the token was issued. Purely informational; nothing enforces it.
	///
	/// Accepts an `Option` so a caller can pass one through without unwrapping it.
	pub fn with_issued(mut self, at: impl Into<Option<kio::clock::Wall>>) -> Self {
		self.issued = at.into();
		self
	}

	/// Identify the token, such as for its issuer to revoke it. Nothing invents one.
	///
	/// Accepts an `Option` so a caller can pass one through without unwrapping it.
	pub fn with_id(mut self, id: impl Into<Option<crate::TokenId>>) -> Self {
		self.id = id.into();
		self
	}

	/// Returns an error when the token grants nothing at all, making it useless.
	pub fn validate(&self) -> crate::Result<()> {
		if self.publish.path.is_empty() && self.subscribe.path.is_empty() {
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

		let scope = |prefixes: &Prefixes| -> crate::Result<Prefixes> {
			if prefix.is_empty() {
				// The path reaches into the grant; keep what each prefix covers below it.
				Ok(prefixes.rebase(suffix))
			} else {
				// The grant sits below the path; name it from there.
				Ok(prefixes.rooted(prefix)?)
			}
		};

		let permissions = Permissions {
			subscribe: scope(&self.subscribe.path)?,
			publish: scope(&self.publish.path)?,
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

	use std::time::Duration;

	fn prefixes(texts: &[&str]) -> Prefixes {
		texts.iter().map(|text| text.parse().unwrap()).collect()
	}

	/// A fractional expiry writes earlier and a fractional not-before later, so neither
	/// widens the window the claims asked for.
	#[test]
	fn fractional_times_narrow_the_window() {
		let at = kio::clock::Wall::UNIX_EPOCH + Duration::from_millis(1500);
		let claims = Claims {
			publish: prefixes(&[""]).into(),
			expires: Some(at),
			not_before: Some(at),
			..Default::default()
		};
		let json = serde_json::to_value(&claims).unwrap();
		assert_eq!((json["exp"].as_u64(), json["nbf"].as_u64()), (Some(1), Some(2)));
	}

	fn create_test_claims() -> Claims {
		Claims {
			root: "test-path".to_string(),
			publish: prefixes(&["test-pub"]).into(),
			subscribe: prefixes(&["test-sub"]).into(),
			expires: Some(kio::clock::wall() + Duration::from_secs(3600)),
			issued: Some(kio::clock::wall()),
			not_before: None,
			id: None,
		}
	}

	#[test]
	fn scope_allows_contained_claims() {
		let scope = Scope {
			root: "project".into(),
			publish: prefixes(&["live"]),
			subscribe: prefixes(&["watch"]),
		};
		let claims = Claims {
			root: "project/live/room".into(),
			publish: prefixes(&[""]).into(),
			..Default::default()
		};
		assert!(scope.allows(&claims));
	}

	#[test]
	fn scope_rejects_sibling_and_role_escalation() {
		let scope = Scope {
			root: "project".into(),
			publish: prefixes(&["live"]),
			subscribe: Prefixes::new(),
		};
		let sibling = Claims {
			root: "project/lively".into(),
			publish: prefixes(&[""]).into(),
			..Default::default()
		};
		let role = Claims {
			root: "project/live".into(),
			subscribe: prefixes(&[""]).into(),
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
			publish: prefixes(&["live"]),
			subscribe: Prefixes::new(),
		};

		for claims in [
			Claims {
				root: "project".into(),
				publish: prefixes(&["live/room"]).into(),
				..Default::default()
			},
			Claims {
				root: String::new(),
				publish: prefixes(&["project/live/room"]).into(),
				..Default::default()
			},
			Claims {
				root: "/project/live/".into(),
				publish: prefixes(&["room"]).into(),
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
			publish: prefixes(&["live"]),
			subscribe: Prefixes::new(),
		};

		// A root above the scope's does not widen it, even though `**` would grant
		// everything within the scope.
		let claims = Claims {
			root: String::new(),
			publish: prefixes(&[""]).into(),
			..Default::default()
		};
		assert!(!scope.allows(&claims));
	}

	#[test]
	fn scope_globstar_grants_everything_beneath_it() {
		let scope = Scope {
			root: "project".into(),
			publish: prefixes(&[""]),
			subscribe: Prefixes::new(),
		};
		let claims = Claims {
			root: "project/anything/deep".into(),
			publish: prefixes(&[""]).into(),
			..Default::default()
		};
		assert!(scope.allows(&claims));
	}

	#[test]
	fn scope_requires_every_requested_prefix() {
		// One allowed prefix does not carry an unallowed sibling along with it.
		let scope = Scope {
			root: "project".into(),
			publish: prefixes(&["live"]),
			subscribe: Prefixes::new(),
		};
		let claims = Claims {
			root: "project".into(),
			publish: prefixes(&["live/room", "other"]).into(),
			..Default::default()
		};
		assert!(!scope.allows(&claims));
	}

	#[test]
	fn scope_covers_by_segment() {
		// `live` covers itself and everything beneath it, but not `lively` or its parent.
		let scope = Scope {
			root: "project".into(),
			publish: prefixes(&["live"]),
			subscribe: Prefixes::new(),
		};
		let claims = |publish: &[&str]| Claims {
			root: "project".into(),
			publish: prefixes(publish).into(),
			..Default::default()
		};
		assert!(scope.allows(&claims(&["live"])));
		assert!(scope.allows(&claims(&["live/room"])));
		assert!(!scope.allows(&claims(&["lively"])));
		assert!(!scope.allows(&claims(&[""])));
	}

	#[test]
	fn scope_without_grants_is_useless() {
		assert!(matches!(Scope::default().validate(), Err(crate::Error::UselessScope)));
	}

	#[test]
	fn scope_refuses_null_grants() {
		assert!(serde_json::from_str::<Scope>(r#"{"put":null,"publish":["room"]}"#).is_err());
	}

	#[test]
	fn scope_reads_legacy_prefixes_as_prefixes() {
		let scope: Scope = serde_json::from_str(r#"{"root":"demo","put":["room"],"get":[""]}"#).unwrap();
		assert_eq!(scope.publish, prefixes(&["room"]));
		assert_eq!(scope.subscribe, prefixes(&[""]));
	}

	#[test]
	fn scope_writes_prefixes() {
		let both = Scope {
			root: "demo".into(),
			publish: prefixes(&["room"]),
			subscribe: prefixes(&[""]),
		};
		assert_eq!(
			serde_json::to_string(&both).unwrap(),
			r#"{"root":"demo","publish":["room"],"subscribe":[""]}"#
		);

		let publish = Scope {
			root: "demo".into(),
			publish: prefixes(&["room"]),
			subscribe: Prefixes::new(),
		};
		assert_eq!(
			serde_json::to_string(&publish).unwrap(),
			r#"{"root":"demo","publish":["room"]}"#
		);
	}

	#[test]
	fn scope_refuses_a_wildcard() {
		for json in [
			r#"{"publish":["room/**"]}"#,
			r#"{"publish":["**"]}"#,
			r#"{"subscribe":["*/chat"]}"#,
		] {
			let err = serde_json::from_str::<Scope>(json).unwrap_err();
			assert!(err.to_string().contains("wildcard"), "{json}: {err}");
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
			publish: prefixes(&["test-pub"]).into(),
			..Default::default()
		};

		assert!(claims.validate().is_ok());
	}

	#[test]
	fn test_claims_validation_only_subscribe() {
		let claims = Claims {
			root: "test-path".to_string(),
			subscribe: prefixes(&["test-sub"]).into(),
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
			publish: prefixes(&["camera1"]).into(),
			subscribe: prefixes(&["camera1", "camera2"]).into(),
			..Default::default()
		};
		assert_eq!(
			serde_json::to_string(&claims).unwrap(),
			r#"{"root":"live","publish":["camera1"],"subscribe":["camera1","camera2"]}"#
		);
	}

	#[test]
	fn test_claims_read_legacy_prefixes_as_subtrees() {
		let claims: Claims =
			serde_json::from_str(r#"{"root":"test","put":["pub1","/a//b/"],"get":"","exp":1700000000}"#).unwrap();
		assert_eq!(claims.publish.path, prefixes(&["pub1", "a/b"]));
		assert_eq!(claims.subscribe.path, prefixes(&[""]));
		assert!(claims.expires.is_some());
	}

	#[test]
	fn test_claims_write_patterns_and_id() {
		let subtrees = Claims {
			root: "live".into(),
			publish: prefixes(&["camera1"]).into(),
			subscribe: prefixes(&[""]).into(),
			..Default::default()
		}
		.with_id(crate::TokenId::decode("abc-123").unwrap());
		let json = serde_json::to_string(&subtrees).unwrap();
		assert_eq!(
			json,
			r#"{"root":"live","publish":["camera1"],"subscribe":[""],"jti":"abc-123"}"#
		);
		let back: Claims = serde_json::from_str(&json).unwrap();
		assert_eq!(back.publish, subtrees.publish);
		assert_eq!(back.subscribe, subtrees.subscribe);
		assert_eq!(back.id, subtrees.id);
	}

	#[test]
	fn test_claims_refuse_an_unbounded_id() {
		let long = format!(r#"{{"publish":[""],"jti":"{}"}}"#, "x".repeat(129));
		for json in [
			r#"{"publish":[""],"jti":7}"#,
			r#"{"publish":[""],"jti":null}"#,
			r#"{"publish":[""],"jti":""}"#,
			r#"{"publish":[""],"jti":"has space"}"#,
			long.as_str(),
		] {
			assert!(serde_json::from_str::<Claims>(json).is_err(), "{json}");
		}
	}

	#[test]
	fn test_claims_refuse_mixed_or_unknown_fields() {
		for json in [
			r#"{"root":"test","publish":["pub1"],"get":["sub1"]}"#,
			r#"{"root":"test","put":[],"subscribe":["sub1"]}"#,
			r#"{"root":"test","put":["pub1"],"cluster":true}"#,
			r#"{"root":"test","put":null,"publish":["pub1"]}"#,
			r#"{"root":"test","publish":null,"subscribe":["sub1"]}"#,
		] {
			assert!(serde_json::from_str::<Claims>(json).is_err(), "{json}");
		}
	}

	#[test]
	fn test_claims_refuse_a_wildcard_in_a_legacy_prefix() {
		// Legacy prefixes had no wildcards; a `*` would silently widen the grant.
		assert!(serde_json::from_str::<Claims>(r#"{"put":["a/*"]}"#).is_err());
	}

	#[test]
	fn test_claims_refuse_a_wildcard_loudly() {
		// A grant written as a pattern says so, and names the prefix to write instead.
		let err = serde_json::from_str::<Claims>(r#"{"publish":["alice/**"]}"#).unwrap_err();
		assert!(err.to_string().contains("`alice` covers `alice`"), "{err}");
		let err = serde_json::from_str::<Claims>(r#"{"subscribe":["**"]}"#).unwrap_err();
		assert!(err.to_string().contains("the empty prefix"), "{err}");
		for json in [
			r#"{"publish":["*"]}"#,
			r#"{"publish":["cam-*.hang"]}"#,
			r#"{"publish":["a/**/b"]}"#,
			r#"{"publish":{"path":["a/**"],"rate":1000}}"#,
		] {
			let err = serde_json::from_str::<Claims>(json).unwrap_err();
			assert!(err.to_string().contains("wildcard"), "{json}: {err}");
		}
	}

	#[test]
	fn test_claims_refuse_a_malformed_prefix() {
		for json in [r#"{"publish":["/a"]}"#, r#"{"publish":["a/"]}"#, r#"{"publish":["a//b"]}"#] {
			assert!(serde_json::from_str::<Claims>(json).is_err(), "{json}");
		}
	}

	#[test]
	fn test_claims_default() {
		let claims = Claims::default();
		assert_eq!(claims.root, "");
		assert!(claims.publish.path.is_empty());
		assert!(claims.subscribe.path.is_empty());
		assert_eq!(claims.expires, None);
		assert_eq!(claims.issued, None);
	}

	fn authorize_claims(root: &str, subscribe: &[&str], publish: &[&str]) -> Claims {
		Claims {
			root: root.to_string(),
			subscribe: prefixes(subscribe).into(),
			publish: prefixes(publish).into(),
			..Default::default()
		}
	}

	#[test]
	fn test_authorize_path_equals_root() {
		let claims = authorize_claims("room/123", &[""], &["alice"]);
		let permissions = claims.authorize("room/123").unwrap();

		assert_eq!(permissions.subscribe, prefixes(&[""]));
		assert_eq!(permissions.publish, prefixes(&["alice"]));
	}

	#[test]
	fn test_authorize_path_extends_root() {
		// Connecting below the root consumes the matching part of each grant.
		let claims = authorize_claims("room/123", &["bob"], &["alice"]);
		let permissions = claims.authorize("room/123/alice").unwrap();

		assert_eq!(permissions.subscribe, Prefixes::new());
		assert_eq!(permissions.publish, prefixes(&[""]));
	}

	#[test]
	fn test_authorize_within_a_prefix_grants_everything_there() {
		// A prefix reached at or beneath it is the empty prefix: this path and all below.
		let claims = authorize_claims("room", &[], &["alice"]);
		for path in ["room/alice", "room/alice/cam"] {
			let permissions = claims.authorize(path).unwrap();
			assert_eq!(permissions.publish, prefixes(&[""]), "{path}");
		}
	}

	#[test]
	fn test_authorize_path_is_parent_of_root() {
		// Connecting above the root prepends it, keeping the grants anchored.
		let claims = authorize_claims("demo", &[""], &["alice"]);
		let permissions = claims.authorize("/").unwrap();

		assert_eq!(permissions.subscribe, prefixes(&["demo"]));
		assert_eq!(permissions.publish, prefixes(&["demo/alice"]));
	}

	#[test]
	fn test_authorize_empty_root() {
		// A root-scoped token grants everything it lists, wherever it connects.
		let claims = authorize_claims("", &["demo"], &[]);
		let permissions = claims.authorize("demo/room").unwrap();

		assert_eq!(permissions.subscribe, prefixes(&[""]));
		assert_eq!(permissions.publish, Prefixes::new());
	}

	#[test]
	fn test_authorize_slashes_are_implicit() {
		let claims = authorize_claims("/room/123/", &["bob"], &[]);
		let permissions = claims.authorize("//room/123//").unwrap();

		assert_eq!(permissions.subscribe, prefixes(&["bob"]));
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
	fn test_authorize_reads_an_escaped_separator_as_content() {
		// `a\/b` is one segment holding a `/`, the spelling of a moq-transport field.
		let claims = authorize_claims("room", &[r"a\/b/c"], &[r"a\/b"]);
		let permissions = claims.authorize(r"room/a\/b").unwrap();
		assert_eq!(permissions.subscribe, prefixes(&["c"]));
		assert_eq!(permissions.publish, prefixes(&[""]));
		assert!(claims.authorize("room").unwrap().publish.matches(r"a\/b"));
		assert!(!claims.authorize("room").unwrap().publish.matches("a/b"));
		// `a\` ends inside that segment, so it is no path the root covers.
		assert!(matches!(
			authorize_claims(r"a\/b", &[""], &[]).authorize(r"a\"),
			Err(crate::Error::RootMismatch(_))
		));
	}

	#[test]
	fn test_authorize_rebases_each_prefix() {
		// Reached at `room`, a grant keeps only what lies beneath it, and anything above
		// it grants the whole room.
		let claims = authorize_claims("", &["room/a", "room/b/c", "lobby"], &[""]);
		let permissions = claims.authorize("room").unwrap();
		assert_eq!(permissions.subscribe, prefixes(&["a", "b/c"]));
		assert_eq!(permissions.publish, prefixes(&[""]));
	}
}
