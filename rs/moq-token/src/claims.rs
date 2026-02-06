use serde::{Deserialize, Deserializer, Serialize};
use serde_with::{TimestampSeconds, serde_as};

fn is_false(value: &bool) -> bool {
	!value
}

fn string_or_vec<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
	D: Deserializer<'de>,
{
	#[derive(Deserialize)]
	#[serde(untagged)]
	enum StringOrVec {
		String(String),
		Vec(Vec<String>),
	}

	match StringOrVec::deserialize(deserializer)? {
		StringOrVec::String(s) => Ok(vec![s]),
		StringOrVec::Vec(v) => Ok(v),
	}
}

#[serde_with::skip_serializing_none]
#[serde_as]
#[derive(Debug, Serialize, Deserialize, Default, Clone)]
#[serde(default)]
pub struct Claims {
	/// The root for the publish/subscribe options below.
	/// It's mostly for compression and is optional, defaulting to the empty string.
	#[serde(default, rename = "root", skip_serializing_if = "String::is_empty")]
	pub root: String,

	/// If specified, the user can publish any matching broadcasts.
	/// If not specified, the user will not publish any broadcasts.
	#[serde(
		default,
		rename = "put",
		skip_serializing_if = "Vec::is_empty",
		deserialize_with = "string_or_vec"
	)]
	pub publish: Vec<String>,

	/// If true, then this client is considered a cluster node.
	/// Both the client and server will only announce broadcasts from non-cluster clients.
	/// This avoids convoluted routing, as only the primary origin will announce.
	//
	// TODO This shouldn't be part of the token.
	#[serde(default, rename = "cluster", skip_serializing_if = "is_false")]
	pub cluster: bool,

	/// If specified, the user can subscribe to any matching broadcasts.
	/// If not specified, the user will not receive announcements and cannot subscribe to any broadcasts.
	// NOTE: This can't be renamed to "sub" because that's a reserved JWT field.
	#[serde(
		default,
		rename = "get",
		skip_serializing_if = "Vec::is_empty",
		deserialize_with = "string_or_vec"
	)]
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
	pub fn validate(&self) -> anyhow::Result<()> {
		if self.publish.is_empty() && self.subscribe.is_empty() {
			anyhow::bail!("no publish or subscribe allowed; token is useless");
		}

		Ok(())
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
			cluster: false,
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
			cluster: false,
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
			cluster: false,
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
			cluster: false,
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
			cluster: false,
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
			cluster: false,
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
			cluster: false,
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
			cluster: false,
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
			cluster: false,
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
			cluster: false,
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
			cluster: false,
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
			cluster: false,
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
		assert_eq!(deserialized.cluster, claims.cluster);
	}

	#[test]
	fn test_claims_default() {
		let claims = Claims::default();
		assert_eq!(claims.root, "");
		assert!(claims.publish.is_empty());
		assert!(claims.subscribe.is_empty());
		assert!(!claims.cluster);
		assert_eq!(claims.expires, None);
		assert_eq!(claims.issued, None);
	}

	#[test]
	fn test_is_false_helper() {
		assert!(is_false(&false));
		assert!(!is_false(&true));
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
