use crate::{Claims, Key, KeyOperation};
use anyhow::Context;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::path::Path;
use std::sync::{Arc, RwLock};

pub trait KeyProvider {
	fn get_keys(&self) -> anyhow::Result<KeySet>;

	fn find_key(&self, kid: &str) -> anyhow::Result<Option<Arc<Key>>> {
		Ok(self
			.get_keys()?
			.keys
			.iter()
			.find(|k| k.kid.is_some() && k.kid.as_deref().unwrap() == kid)
			.cloned())
	}

	fn find_supported_key(&self, operation: &KeyOperation) -> anyhow::Result<Option<Arc<Key>>> {
		Ok(self
			.get_keys()?
			.keys
			.iter()
			.find(|key| key.operations.contains(operation))
			.cloned())
	}

	fn decode(&self, token: &str) -> anyhow::Result<Claims> {
		let header = jsonwebtoken::decode_header(token).context("failed to decode JWT header")?;

		let key_set = self.get_keys()?;
		let key = match header.kid {
			Some(kid) => key_set
				.find_key(kid.as_str())?
				.ok_or_else(|| anyhow::anyhow!("cannot find key with kid {kid}")),
			None => {
				if key_set.keys.len() == 1 {
					Ok(key_set.keys[0].clone())
				} else {
					anyhow::bail!("missing kid in JWT header")
				}
			}
		}?;

		key.decode(token)
	}
}

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
}

impl KeyProvider for KeySet {
	fn get_keys(&self) -> anyhow::Result<KeySet> {
		Ok(self.clone())
	}
}

/// JWK Set Loader that allows refreshing of a JWK Set
#[cfg(feature = "jwks-loader")]
pub struct KeySetLoader {
	jwks_uri: String,
	keys: RwLock<Option<KeySet>>,
}

#[cfg(feature = "jwks-loader")]
impl KeySetLoader {
	pub fn new(jwks_uri: String) -> Self {
		Self {
			jwks_uri,
			keys: RwLock::new(None), // start with no KeySet
		}
	}

	pub async fn refresh(&self) -> anyhow::Result<()> {
		// Fetch the JWKS JSON
		let jwks_json = reqwest::get(&self.jwks_uri)
			.await
			.with_context(|| format!("failed to GET JWKS from {}", self.jwks_uri))?
			.error_for_status()
			.with_context(|| format!("JWKS endpoint returned error: {}", self.jwks_uri))?
			.text()
			.await
			.context("failed to read JWKS response body")?;

		// Parse the JWKS into a KeySet
		let new_keys = KeySet::from_str(&jwks_json).context("Failed to parse JWKS into KeySet")?;

		// Replace the existing KeySet atomically
		*self.keys.write().expect("keys write lock poisoned") = Some(new_keys);

		Ok(())
	}
}

#[cfg(feature = "jwks-loader")]
impl KeyProvider for KeySetLoader {
	fn get_keys(&self) -> anyhow::Result<KeySet> {
		let guard = self.keys.read().expect("keys read lock poisoned");
		guard
			.as_ref()
			.cloned()
			.ok_or_else(|| anyhow::anyhow!("keys not loaded yet"))
	}
}
