use crate::{Claims, Key, KeyOperation};
use anyhow::Context;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

/// JWK Set to spec <https://datatracker.ietf.org/doc/html/rfc7517#section-5>
#[derive(Default, Clone)]
pub struct KeySet {
	/// Vec of an arbitrary number of Json Web Keys
	pub keys: Vec<Arc<Key>>,
}

impl Serialize for KeySet {
	fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
	where
		S: Serializer,
	{
		// Serialize as a struct with a `keys` field
		use serde::ser::SerializeStruct;

		let mut state = serializer.serialize_struct("KeySet", 1)?;
		state.serialize_field("keys", &self.keys.iter().map(|k| k.as_ref()).collect::<Vec<_>>())?;
		state.end()
	}
}

impl<'de> Deserialize<'de> for KeySet {
	fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
	where
		D: Deserializer<'de>,
	{
		// Deserialize into a temporary Vec<Key>
		#[derive(Deserialize)]
		struct RawKeySet {
			keys: Vec<Key>,
		}

		let raw = RawKeySet::deserialize(deserializer)?;
		Ok(KeySet {
			keys: raw.keys.into_iter().map(Arc::new).collect(),
		})
	}
}

impl KeySet {
	#[allow(clippy::should_implement_trait)]
	pub fn from_str(s: &str) -> anyhow::Result<Self> {
		Ok(serde_json::from_str(s)?)
	}

	pub fn from_file<P: AsRef<Path>>(path: P) -> anyhow::Result<Self> {
		let json = std::fs::read_to_string(&path)?;
		Ok(serde_json::from_str(&json)?)
	}

	pub fn to_str(&self) -> anyhow::Result<String> {
		Ok(serde_json::to_string(&self)?)
	}

	pub fn to_file<P: AsRef<Path>>(&self, path: P) -> anyhow::Result<()> {
		let json = serde_json::to_string(&self)?;
		std::fs::write(path, json)?;
		Ok(())
	}

	pub fn to_public_set(&self) -> anyhow::Result<KeySet> {
		Ok(KeySet {
			keys: self
				.keys
				.iter()
				.map(|key| {
					key.as_ref()
						.to_public()
						.map(Arc::new)
						.map_err(|e| anyhow::anyhow!("failed to get public key from jwks: {:?}", e))
				})
				.collect::<Result<Vec<Arc<Key>>, _>>()?,
		})
	}

	pub fn find_key(&self, kid: &str) -> Option<Arc<Key>> {
		self.keys.iter().find(|k| k.kid.as_deref() == Some(kid)).cloned()
	}

	pub fn find_supported_key(&self, operation: &KeyOperation) -> Option<Arc<Key>> {
		self.keys.iter().find(|key| key.operations.contains(operation)).cloned()
	}

	pub fn encode(&self, payload: &Claims) -> anyhow::Result<String> {
		let key = self
			.find_supported_key(&KeyOperation::Sign)
			.context("cannot find signing key")?;
		key.encode(payload)
	}

	pub fn decode(&self, token: &str) -> anyhow::Result<Claims> {
		let header = jsonwebtoken::decode_header(token).context("failed to decode JWT header")?;

		let key = match header.kid {
			Some(kid) => self
				.find_key(kid.as_str())
				.ok_or_else(|| anyhow::anyhow!("cannot find key with kid {kid}")),
			None => {
				// If we only have one key we can use it without a kid
				if self.keys.len() == 1 {
					Ok(self.keys[0].clone())
				} else {
					anyhow::bail!("missing kid in JWT header")
				}
			}
		}?;

		key.decode(token)
	}
}

#[cfg(feature = "jwks-loader")]
pub async fn load_keys(jwks_uri: &str) -> anyhow::Result<KeySet> {
	let client = reqwest::Client::builder()
		.timeout(Duration::from_secs(10))
		.build()
		.context("failed to build reqwest client")?;

	let jwks_json = client
		.get(jwks_uri)
		.send()
		.await
		.with_context(|| format!("failed to GET JWKS from {}", jwks_uri))?
		.error_for_status()
		.with_context(|| format!("JWKS endpoint returned error: {}", jwks_uri))?
		.text()
		.await
		.context("failed to read JWKS response body")?;

	// Parse the JWKS into a KeySet
	KeySet::from_str(&jwks_json).context("Failed to parse JWKS into KeySet")
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::Algorithm;
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

	fn create_test_key(kid: Option<String>) -> Key {
		Key::generate(Algorithm::ES256, kid).expect("failed to generate key")
	}

	#[test]
	fn test_keyset_from_str_valid() {
		let json = r#"{"keys":[{"kty":"oct","k":"2AJvfDJMVfWe9WMRPJP-4zCGN8F62LOy3dUr--rogR8","alg":"HS256","key_ops":["verify","sign"],"kid":"1"}]}"#;
		let set = KeySet::from_str(json);
		assert!(set.is_ok());
		let set = set.unwrap();
		assert_eq!(set.keys.len(), 1);
		assert_eq!(set.keys[0].kid.as_deref(), Some("1"));
		assert!(set.find_key("1").is_some());
	}

	#[test]
	fn test_keyset_from_str_invalid_json() {
		let result = KeySet::from_str("invalid json");
		assert!(result.is_err());
	}

	#[test]
	fn test_keyset_from_str_empty() {
		let json = r#"{"keys":[]}"#;
		let set = KeySet::from_str(json).unwrap();
		assert!(set.keys.is_empty());
	}

	#[test]
	fn test_keyset_to_str() {
		let key = create_test_key(Some("1".to_string()));
		let set = KeySet {
			keys: vec![Arc::new(key)],
		};

		let json = set.to_str().unwrap();
		assert!(json.contains("\"keys\""));
		assert!(json.contains("\"kid\":\"1\""));
	}

	#[test]
	fn test_keyset_serde_round_trip() {
		let key1 = create_test_key(Some("1".to_string()));
		let key2 = create_test_key(Some("2".to_string()));
		let set = KeySet {
			keys: vec![Arc::new(key1), Arc::new(key2)],
		};

		let json = set.to_str().unwrap();
		let deserialized = KeySet::from_str(&json).unwrap();

		assert_eq!(deserialized.keys.len(), 2);
		assert!(deserialized.find_key("1").is_some());
		assert!(deserialized.find_key("2").is_some());
	}

	#[test]
	fn test_find_key_success() {
		let key = create_test_key(Some("my-key".to_string()));
		let set = KeySet {
			keys: vec![Arc::new(key)],
		};

		let found = set.find_key("my-key");
		assert!(found.is_some());
		assert_eq!(found.unwrap().kid.as_deref(), Some("my-key"));
	}

	#[test]
	fn test_find_key_missing() {
		let key = create_test_key(Some("my-key".to_string()));
		let set = KeySet {
			keys: vec![Arc::new(key)],
		};

		let found = set.find_key("other-key");
		assert!(found.is_none());
	}

	#[test]
	fn test_find_key_no_kid() {
		let key = create_test_key(None);
		let set = KeySet {
			keys: vec![Arc::new(key)],
		};

		let found = set.find_key("any-key");
		assert!(found.is_none());
	}

	#[test]
	fn test_find_supported_key() {
		let mut sign_key = create_test_key(Some("sign".to_string()));
		sign_key.operations = [KeyOperation::Sign].into();

		let mut verify_key = create_test_key(Some("verify".to_string()));
		verify_key.operations = [KeyOperation::Verify].into();

		let set = KeySet {
			keys: vec![Arc::new(sign_key), Arc::new(verify_key)],
		};

		let found_sign = set.find_supported_key(&KeyOperation::Sign);
		assert!(found_sign.is_some());
		assert_eq!(found_sign.unwrap().kid.as_deref(), Some("sign"));

		let found_verify = set.find_supported_key(&KeyOperation::Verify);
		assert!(found_verify.is_some());
		assert_eq!(found_verify.unwrap().kid.as_deref(), Some("verify"));
	}

	#[test]
	fn test_to_public_set() {
		// Use asymmetric key (ES256) so we can separate public/private
		let key = create_test_key(Some("1".to_string()));

		let set = KeySet {
			keys: vec![Arc::new(key)],
		};

		let public_set = set.to_public_set().expect("failed to convert to public set");
		assert_eq!(public_set.keys.len(), 1);

		let public_key = &public_set.keys[0];
		assert_eq!(public_key.kid.as_deref(), Some("1"));
		assert!(public_key.operations.contains(&KeyOperation::Verify));
		assert!(!public_key.operations.contains(&KeyOperation::Sign));
	}

	#[test]
	fn test_to_public_set_fails_for_symmetric() {
		let key = Key::generate(Algorithm::HS256, Some("sym".to_string())).unwrap();
		let set = KeySet {
			keys: vec![Arc::new(key)],
		};

		let result = set.to_public_set();
		assert!(result.is_err());
	}

	#[test]
	fn test_encode_success() {
		let key = create_test_key(Some("1".to_string()));
		let set = KeySet {
			keys: vec![Arc::new(key)],
		};
		let claims = create_test_claims();

		let token = set.encode(&claims).unwrap();
		assert!(!token.is_empty());
	}

	#[test]
	fn test_encode_no_signing_key() {
		let mut key = create_test_key(Some("1".to_string()));
		key.operations = [KeyOperation::Verify].into();
		let set = KeySet {
			keys: vec![Arc::new(key)],
		};
		let claims = create_test_claims();

		let result = set.encode(&claims);
		assert!(result.is_err());
		assert!(result.unwrap_err().to_string().contains("cannot find signing key"));
	}

	#[test]
	fn test_decode_success_with_kid() {
		let key = create_test_key(Some("1".to_string()));
		let set = KeySet {
			keys: vec![Arc::new(key)],
		};
		let claims = create_test_claims();

		let token = set.encode(&claims).unwrap();
		let decoded = set.decode(&token).unwrap();

		assert_eq!(decoded.root, claims.root);
	}

	#[test]
	fn test_decode_success_single_key_no_kid() {
		// Create a key without KID
		let key = create_test_key(None);
		let claims = create_test_claims();

		// Encode using the key directly
		let token = key.encode(&claims).unwrap();

		let set = KeySet {
			keys: vec![Arc::new(key)],
		};

		// Decode using the set
		let decoded = set.decode(&token).unwrap();
		assert_eq!(decoded.root, claims.root);
	}

	#[test]
	fn test_decode_fail_multiple_keys_no_kid() {
		let key1 = create_test_key(None);
		let key2 = create_test_key(None);

		let set = KeySet {
			keys: vec![Arc::new(key1), Arc::new(key2)],
		};

		let claims = create_test_claims();
		// Encode with one of the keys directly
		let token = set.keys[0].encode(&claims).unwrap();

		let result = set.decode(&token);
		assert!(result.is_err());
		assert!(result.unwrap_err().to_string().contains("missing kid"));
	}

	#[test]
	fn test_decode_fail_unknown_kid() {
		let key1 = create_test_key(Some("1".to_string()));
		let key2 = create_test_key(Some("2".to_string()));

		let set1 = KeySet {
			keys: vec![Arc::new(key1)],
		};
		let set2 = KeySet {
			keys: vec![Arc::new(key2)],
		};

		let claims = create_test_claims();
		let token = set1.encode(&claims).unwrap();

		let result = set2.decode(&token);
		assert!(result.is_err());
		assert!(result.unwrap_err().to_string().contains("cannot find key with kid 1"));
	}

	#[test]
	fn test_file_io() {
		let key = create_test_key(Some("1".to_string()));
		let set = KeySet {
			keys: vec![Arc::new(key)],
		};

		let dir = std::env::temp_dir();
		// Use a random-ish name to avoid collisions
		let filename = format!(
			"test_keyset_{}.json",
			SystemTime::now()
				.duration_since(SystemTime::UNIX_EPOCH)
				.unwrap()
				.as_nanos()
		);
		let path = dir.join(filename);

		set.to_file(&path).expect("failed to write to file");

		let loaded = KeySet::from_file(&path).expect("failed to read from file");
		assert_eq!(loaded.keys.len(), 1);
		assert_eq!(loaded.keys[0].kid.as_deref(), Some("1"));

		// Clean up
		let _ = std::fs::remove_file(path);
	}
}
