use crate::{Claims, Key, KeyOperation};
use anyhow::Context;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::path::Path;
use std::sync::Arc;

/// JWK Set to spec https://datatracker.ietf.org/doc/html/rfc7517#section-5
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
		self.keys
			.iter()
			.find(|k| k.kid.is_some() && k.kid.as_deref().unwrap() == kid)
			.cloned()
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
	// Fetch the JWKS JSON
	let jwks_json = reqwest::get(jwks_uri)
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
