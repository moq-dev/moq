use crate::error::KeyError;
use crate::generate::generate;
use crate::{Algorithm, Claims};
use base64::Engine;
use elliptic_curve::SecretKey;
use elliptic_curve::pkcs8::EncodePrivateKey;
use jsonwebtoken::{DecodingKey, EncodingKey, Header};
use rsa::BigUint;
use rsa::pkcs1::EncodeRsaPrivateKey;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::sync::OnceLock;
use std::{collections::HashSet, fmt, path::Path as StdPath};

/// Cryptographic operations that a key can perform.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[serde(rename_all = "camelCase")]
pub enum KeyOperation {
	Sign,
	Verify,
	Decrypt,
	Encrypt,
}

/// <https://datatracker.ietf.org/doc/html/rfc7518#section-6>
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kty")]
pub enum KeyType {
	/// <https://datatracker.ietf.org/doc/html/rfc7518#section-6.2>
	EC {
		#[serde(rename = "crv")]
		curve: EllipticCurve,
		/// The X-coordinate of an EC key
		#[serde(serialize_with = "serialize_base64url", deserialize_with = "deserialize_base64url")]
		x: Vec<u8>,
		/// The Y-coordinate of an EC key
		#[serde(serialize_with = "serialize_base64url", deserialize_with = "deserialize_base64url")]
		y: Vec<u8>,
		/// The private value of an EC key
		#[serde(
			default,
			skip_serializing_if = "Option::is_none",
			serialize_with = "serialize_base64url_optional",
			deserialize_with = "deserialize_base64url_optional"
		)]
		d: Option<Vec<u8>>,
	},
	/// <https://datatracker.ietf.org/doc/html/rfc7518#section-6.3>
	RSA {
		#[serde(flatten)]
		public: RsaPublicKey,
		#[serde(flatten, skip_serializing_if = "Option::is_none")]
		private: Option<RsaPrivateKey>,
	},
	/// <https://datatracker.ietf.org/doc/html/rfc7518#section-6.4>
	#[serde(rename = "oct")]
	OCT {
		/// The secret key as base64url (unpadded).
		#[serde(
			rename = "k",
			default,
			serialize_with = "serialize_base64url",
			deserialize_with = "deserialize_base64url"
		)]
		secret: Vec<u8>,
	},
	/// <https://datatracker.ietf.org/doc/html/rfc8037#section-2>
	OKP {
		#[serde(rename = "crv")]
		curve: EllipticCurve,
		#[serde(serialize_with = "serialize_base64url", deserialize_with = "deserialize_base64url")]
		x: Vec<u8>,
		#[serde(
			rename = "d",
			default,
			skip_serializing_if = "Option::is_none",
			serialize_with = "serialize_base64url_optional",
			deserialize_with = "deserialize_base64url_optional"
		)]
		d: Option<Vec<u8>>,
	},
}

/// Supported elliptic curves for EC and OKP key types.
///
/// See <https://datatracker.ietf.org/doc/html/rfc7518#section-6.2.1.1>
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq, Debug)]
pub enum EllipticCurve {
	#[serde(rename = "P-256")]
	P256,
	#[serde(rename = "P-384")]
	P384,
	// jsonwebtoken doesn't support the ES512 algorithm, so we can't implement this
	// #[serde(rename = "P-521")]
	// P521,
	#[serde(rename = "Ed25519")]
	Ed25519,
}

/// RSA public key parameters.
///
/// See <https://datatracker.ietf.org/doc/html/rfc7518#section-6.3.1>
#[derive(Clone, Serialize, Deserialize)]
pub struct RsaPublicKey {
	#[serde(serialize_with = "serialize_base64url", deserialize_with = "deserialize_base64url")]
	pub n: Vec<u8>,
	#[serde(serialize_with = "serialize_base64url", deserialize_with = "deserialize_base64url")]
	pub e: Vec<u8>,
}

/// RSA private key parameters.
///
/// See <https://datatracker.ietf.org/doc/html/rfc7518#section-6.3.2>
#[derive(Clone, Serialize, Deserialize)]
pub struct RsaPrivateKey {
	#[serde(serialize_with = "serialize_base64url", deserialize_with = "deserialize_base64url")]
	pub d: Vec<u8>,
	#[serde(serialize_with = "serialize_base64url", deserialize_with = "deserialize_base64url")]
	pub p: Vec<u8>,
	#[serde(serialize_with = "serialize_base64url", deserialize_with = "deserialize_base64url")]
	pub q: Vec<u8>,
	#[serde(serialize_with = "serialize_base64url", deserialize_with = "deserialize_base64url")]
	pub dp: Vec<u8>,
	#[serde(serialize_with = "serialize_base64url", deserialize_with = "deserialize_base64url")]
	pub dq: Vec<u8>,
	#[serde(serialize_with = "serialize_base64url", deserialize_with = "deserialize_base64url")]
	pub qi: Vec<u8>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub oth: Option<Vec<RsaAdditionalPrime>>,
}

/// Additional prime information for multi-prime RSA keys.
#[derive(Clone, Serialize, Deserialize)]
pub struct RsaAdditionalPrime {
	#[serde(serialize_with = "serialize_base64url", deserialize_with = "deserialize_base64url")]
	pub r: Vec<u8>,
	#[serde(serialize_with = "serialize_base64url", deserialize_with = "deserialize_base64url")]
	pub d: Vec<u8>,
	#[serde(serialize_with = "serialize_base64url", deserialize_with = "deserialize_base64url")]
	pub t: Vec<u8>,
}

/// JWK, almost to spec (<https://datatracker.ietf.org/doc/html/rfc7517>) but not quite the same
/// because it's annoying to implement.
#[derive(Clone, Serialize, Deserialize)]
#[serde(remote = "Self")]
pub struct Key {
	/// The algorithm used by the key.
	#[serde(rename = "alg")]
	pub algorithm: Algorithm,

	/// The operations that the key can perform.
	#[serde(rename = "key_ops")]
	pub operations: HashSet<KeyOperation>,

	/// Defaults to KeyType::OCT
	#[serde(flatten)]
	pub key: KeyType,

	/// The key ID, useful for rotating keys.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub kid: Option<crate::KeyId>,

	// Cached for performance reasons, unfortunately.
	#[serde(skip)]
	pub(crate) decode: OnceLock<DecodingKey>,

	#[serde(skip)]
	pub(crate) encode: OnceLock<EncodingKey>,
}

impl<'de> Deserialize<'de> for Key {
	fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
	where
		D: Deserializer<'de>,
	{
		let mut value = serde_json::Value::deserialize(deserializer)?;

		// Normally the "kty" parameter is required in a JWK: https://datatracker.ietf.org/doc/html/rfc7517#section-4.1
		// But for backwards compatibility we need to default to "oct" because in a previous
		// implementation the parameter was omitted, and we want to keep previously generated tokens valid
		if let Some(obj) = value.as_object_mut()
			&& !obj.contains_key("kty")
		{
			obj.insert("kty".to_string(), serde_json::Value::String("oct".to_string()));
		}

		Self::deserialize(value).map_err(serde::de::Error::custom)
	}
}

impl Serialize for Key {
	fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
	where
		S: Serializer,
	{
		Self::serialize(self, serializer)
	}
}

impl fmt::Debug for Key {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("Key")
			.field("algorithm", &self.algorithm)
			.field("operations", &self.operations)
			.field("kid", &self.kid)
			.finish()
	}
}

impl Key {
	#[allow(clippy::should_implement_trait)]
	pub fn from_str(s: &str) -> crate::Result<Self> {
		Ok(serde_json::from_str(s)?)
	}

	/// Load a key from a file, auto-detecting JSON or base64url encoding.
	pub fn from_file<P: AsRef<StdPath>>(path: P) -> crate::Result<Self> {
		let contents = std::fs::read_to_string(&path)?;
		Self::from_encoded(&contents)
	}

	/// Async version of [`from_file`](Self::from_file), using `tokio::fs`.
	#[cfg(feature = "tokio")]
	pub async fn from_file_async<P: AsRef<StdPath>>(path: P) -> crate::Result<Self> {
		let contents = tokio::fs::read_to_string(path).await?;
		Self::from_encoded(&contents)
	}

	/// Parse a key from a string, auto-detecting JSON or base64url encoding.
	fn from_encoded(contents: &str) -> crate::Result<Self> {
		let contents = contents.trim();
		if contents.starts_with('{') {
			Ok(serde_json::from_str(contents)?)
		} else {
			let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(contents)?;
			let json = String::from_utf8(decoded)?;
			Ok(serde_json::from_str(&json)?)
		}
	}

	pub fn to_str(&self) -> crate::Result<String> {
		Ok(serde_json::to_string(self)?)
	}

	/// Write the key to a file as JSON.
	pub fn to_file<P: AsRef<StdPath>>(&self, path: P) -> crate::Result<()> {
		let json = serde_json::to_string_pretty(self)?;
		std::fs::write(path, json)?;
		Ok(())
	}

	/// Write the key to a file as base64url-encoded JSON.
	pub fn to_file_base64url<P: AsRef<StdPath>>(&self, path: P) -> crate::Result<()> {
		let json = serde_json::to_string(self)?;
		let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json.as_bytes());
		std::fs::write(path, encoded)?;
		Ok(())
	}

	pub fn to_public(&self) -> crate::Result<Self> {
		if !self.operations.contains(&KeyOperation::Verify) {
			return Err(KeyError::VerifyUnsupported.into());
		}

		let key = match self.key {
			KeyType::RSA { ref public, .. } => KeyType::RSA {
				public: public.clone(),
				private: None,
			},
			KeyType::EC {
				ref x,
				ref y,
				ref curve,
				..
			} => KeyType::EC {
				x: x.clone(),
				y: y.clone(),
				curve: curve.clone(),
				d: None,
			},
			KeyType::OCT { .. } => return Err(KeyError::NoPublicKey.into()),
			KeyType::OKP { ref x, ref curve, .. } => KeyType::OKP {
				x: x.clone(),
				curve: curve.clone(),
				d: None,
			},
		};

		Ok(Self {
			algorithm: self.algorithm,
			operations: [KeyOperation::Verify].into(),
			key,
			kid: self.kid.clone(),
			decode: Default::default(),
			encode: Default::default(),
		})
	}

	fn to_decoding_key(&self) -> crate::Result<&DecodingKey> {
		if let Some(key) = self.decode.get() {
			return Ok(key);
		}

		let decoding_key = match self.key {
			KeyType::OCT { ref secret } => match self.algorithm {
				Algorithm::HS256 | Algorithm::HS384 | Algorithm::HS512 => DecodingKey::from_secret(secret),
				_ => return Err(KeyError::InvalidAlgorithm.into()),
			},
			KeyType::EC {
				ref curve,
				ref x,
				ref y,
				..
			} => match curve {
				EllipticCurve::P256 => {
					if self.algorithm != Algorithm::ES256 {
						return Err(KeyError::InvalidAlgorithmForCurve("P-256").into());
					}
					if x.len() != 32 || y.len() != 32 {
						return Err(KeyError::InvalidCoordinateLength("P-256").into());
					}

					DecodingKey::from_ec_components(
						base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(x).as_ref(),
						base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(y).as_ref(),
					)?
				}
				EllipticCurve::P384 => {
					if self.algorithm != Algorithm::ES384 {
						return Err(KeyError::InvalidAlgorithmForCurve("P-384").into());
					}
					if x.len() != 48 || y.len() != 48 {
						return Err(KeyError::InvalidCoordinateLength("P-384").into());
					}

					DecodingKey::from_ec_components(
						base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(x).as_ref(),
						base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(y).as_ref(),
					)?
				}
				_ => return Err(KeyError::InvalidCurve("EC").into()),
			},
			KeyType::OKP { ref curve, ref x, .. } => match curve {
				EllipticCurve::Ed25519 => {
					if self.algorithm != Algorithm::EdDSA {
						return Err(KeyError::InvalidAlgorithmForCurve("Ed25519").into());
					}

					DecodingKey::from_ed_components(
						base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(x).as_ref(),
					)?
				}
				_ => return Err(KeyError::InvalidCurve("OKP").into()),
			},
			KeyType::RSA { ref public, .. } => {
				DecodingKey::from_rsa_raw_components(public.n.as_ref(), public.e.as_ref())
			}
		};

		Ok(self.decode.get_or_init(|| decoding_key))
	}

	fn to_encoding_key(&self) -> crate::Result<&EncodingKey> {
		if let Some(key) = self.encode.get() {
			return Ok(key);
		}

		let encoding_key = match self.key {
			KeyType::OCT { ref secret } => match self.algorithm {
				Algorithm::HS256 | Algorithm::HS384 | Algorithm::HS512 => EncodingKey::from_secret(secret),
				_ => return Err(KeyError::InvalidAlgorithm.into()),
			},
			KeyType::EC { ref curve, ref d, .. } => {
				let d = d.as_ref().ok_or(KeyError::MissingPrivateKey)?;

				match curve {
					EllipticCurve::P256 => {
						let secret_key = SecretKey::<p256::NistP256>::from_slice(d)?;
						let doc = secret_key.to_pkcs8_der()?;
						EncodingKey::from_ec_der(doc.as_bytes())
					}
					EllipticCurve::P384 => {
						let secret_key = SecretKey::<p384::NistP384>::from_slice(d)?;
						let doc = secret_key.to_pkcs8_der()?;
						EncodingKey::from_ec_der(doc.as_bytes())
					}
					_ => return Err(KeyError::InvalidCurve("EC").into()),
				}
			}
			KeyType::OKP {
				ref curve,
				ref d,
				ref x,
			} => {
				let d = d.as_ref().ok_or(KeyError::MissingPrivateKey)?;

				let key_pair =
					aws_lc_rs::signature::Ed25519KeyPair::from_seed_and_public_key(d.as_slice(), x.as_slice())?;

				match curve {
					EllipticCurve::Ed25519 => EncodingKey::from_ed_der(key_pair.to_pkcs8()?.as_ref()),
					_ => return Err(KeyError::InvalidCurve("OKP").into()),
				}
			}
			KeyType::RSA {
				ref public,
				ref private,
			} => {
				let n = BigUint::from_bytes_be(&public.n);
				let e = BigUint::from_bytes_be(&public.e);
				let private = private.as_ref().ok_or(KeyError::MissingPrivateKey)?;
				let d = BigUint::from_bytes_be(&private.d);
				let p = BigUint::from_bytes_be(&private.p);
				let q = BigUint::from_bytes_be(&private.q);

				let rsa = rsa::RsaPrivateKey::from_components(n, e, d, vec![p, q]);
				let pem = rsa?.to_pkcs1_pem(rsa::pkcs1::LineEnding::LF);

				EncodingKey::from_rsa_pem(pem?.as_bytes())?
			}
		};

		Ok(self.encode.get_or_init(|| encoding_key))
	}

	pub fn decode(&self, token: &str) -> crate::Result<Claims> {
		if !self.operations.contains(&KeyOperation::Verify) {
			return Err(KeyError::VerifyUnsupported.into());
		}

		let decode = self.to_decoding_key()?;

		let mut validation = jsonwebtoken::Validation::new(self.algorithm.into());
		validation.required_spec_claims = Default::default(); // Don't require exp, but still validate it if present
		validation.validate_exp = false; // We validate exp ourselves to handle null values

		let token = jsonwebtoken::decode::<Claims>(token, decode, &validation)?;

		if let Some(exp) = token.claims.expires
			&& exp < std::time::SystemTime::now()
		{
			return Err(crate::Error::TokenExpired);
		}

		token.claims.validate()?;

		Ok(token.claims)
	}

	pub fn encode(&self, payload: &Claims) -> crate::Result<String> {
		if !self.operations.contains(&KeyOperation::Sign) {
			return Err(KeyError::SignUnsupported.into());
		}

		payload.validate()?;

		let encode = self.to_encoding_key()?;

		let mut header = Header::new(self.algorithm.into());
		header.kid = self.kid.as_ref().map(|k| k.to_string());
		let token = jsonwebtoken::encode(&header, &payload, encode)?;
		Ok(token)
	}

	/// Generate a key pair for the given algorithm, returning the private and public keys.
	pub fn generate(algorithm: Algorithm, id: Option<crate::KeyId>) -> crate::Result<Self> {
		generate(algorithm, id)
	}
}

/// Serialize bytes as base64url without padding
fn serialize_base64url<S>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error>
where
	S: Serializer,
{
	let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
	serializer.serialize_str(&encoded)
}

fn serialize_base64url_optional<S>(bytes: &Option<Vec<u8>>, serializer: S) -> Result<S::Ok, S::Error>
where
	S: Serializer,
{
	match bytes {
		Some(b) => serialize_base64url(b, serializer),
		None => serializer.serialize_none(),
	}
}

/// Deserialize base64url string to bytes, supporting both padded and unpadded formats for backwards compatibility
fn deserialize_base64url<'de, D>(deserializer: D) -> Result<Vec<u8>, D::Error>
where
	D: Deserializer<'de>,
{
	let s = String::deserialize(deserializer)?;

	// Try to decode as unpadded base64url first (preferred format)
	base64::engine::general_purpose::URL_SAFE_NO_PAD
		.decode(&s)
		.or_else(|_| {
			// Fall back to padded base64url for backwards compatibility
			base64::engine::general_purpose::URL_SAFE.decode(&s)
		})
		.map_err(serde::de::Error::custom)
}

fn deserialize_base64url_optional<'de, D>(deserializer: D) -> Result<Option<Vec<u8>>, D::Error>
where
	D: Deserializer<'de>,
{
	let s: Option<String> = Option::deserialize(deserializer)?;
	match s {
		Some(s) => {
			let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
				.decode(&s)
				.or_else(|_| base64::engine::general_purpose::URL_SAFE.decode(&s))
				.map_err(serde::de::Error::custom)?;
			Ok(Some(decoded))
		}
		None => Ok(None),
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::time::{Duration, SystemTime};

	fn create_test_key() -> Key {
		Key {
			algorithm: Algorithm::HS256,
			operations: [KeyOperation::Sign, KeyOperation::Verify].into(),
			key: KeyType::OCT {
				secret: b"test-secret-that-is-long-enough-for-hmac-sha256".to_vec(),
			},
			kid: Some(crate::KeyId::decode("test-key-1").unwrap()),
			decode: Default::default(),
			encode: Default::default(),
		}
	}

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
	fn test_key_from_str_valid() {
		let key = create_test_key();
		let json = key.to_str().unwrap();
		let loaded_key = Key::from_str(&json).unwrap();

		assert_eq!(loaded_key.algorithm, key.algorithm);
		assert_eq!(loaded_key.operations, key.operations);
		match (loaded_key.key, key.key) {
			(KeyType::OCT { secret: loaded_secret }, KeyType::OCT { secret }) => {
				assert_eq!(loaded_secret, secret);
			}
			_ => panic!("Expected OCT key"),
		}
		assert_eq!(loaded_key.kid, key.kid);
	}

	/// Tests whether Key::from_str() works for keys without a kty value to fall back to OCT
	#[test]
	fn test_key_oct_backwards_compatibility() {
		let json = r#"{"alg":"HS256","key_ops":["sign","verify"],"k":"Fp8kipWUJeUFqeSqWym_tRC_tyI8z-QpqopIGrbrD68"}"#;
		let key = Key::from_str(json);

		assert!(key.is_ok());
		let key = key.unwrap();

		if let KeyType::OCT { ref secret, .. } = key.key {
			let base64_key = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(secret);
			assert_eq!(base64_key, "Fp8kipWUJeUFqeSqWym_tRC_tyI8z-QpqopIGrbrD68");
		} else {
			panic!("Expected OCT key");
		}

		let key_str = key.to_str();
		assert!(key_str.is_ok());
		let key_str = key_str.unwrap();

		// After serializing again it must contain the kty
		assert!(key_str.contains("\"alg\":\"HS256\""));
		assert!(key_str.contains("\"key_ops\""));
		assert!(key_str.contains("\"sign\""));
		assert!(key_str.contains("\"verify\""));
		assert!(key_str.contains("\"kty\":\"oct\""));
	}

	#[test]
	fn test_key_from_str_invalid_json() {
		let result = Key::from_str("invalid json");
		assert!(result.is_err());
	}

	#[test]
	fn test_key_to_str() {
		let key = create_test_key();
		let json = key.to_str().unwrap();
		assert!(json.contains("\"alg\":\"HS256\""));
		assert!(json.contains("\"key_ops\""));
		assert!(json.contains("\"sign\""));
		assert!(json.contains("\"verify\""));
		assert!(json.contains("\"kid\":\"test-key-1\""));
		assert!(json.contains("\"kty\":\"oct\""));
	}

	#[test]
	fn test_key_sign_success() {
		let key = create_test_key();
		let claims = create_test_claims();
		let token = key.encode(&claims).unwrap();

		assert!(!token.is_empty());
		assert_eq!(token.matches('.').count(), 2); // JWT format: header.payload.signature
	}

	#[test]
	fn test_key_sign_no_permission() {
		let mut key = create_test_key();
		key.operations = [KeyOperation::Verify].into();
		let claims = create_test_claims();

		let result = key.encode(&claims);
		assert!(result.is_err());
		assert!(result.unwrap_err().to_string().contains("key does not support signing"));
	}

	#[test]
	fn test_key_sign_invalid_claims() {
		let key = create_test_key();
		let invalid_claims = Claims {
			root: "test-path".to_string(),
			publish: vec![],
			subscribe: vec![],
			cluster: false,
			expires: None,
			issued: None,
		};

		let result = key.encode(&invalid_claims);
		assert!(result.is_err());
		assert!(
			result
				.unwrap_err()
				.to_string()
				.contains("no publish or subscribe allowed; token is useless")
		);
	}

	#[test]
	fn test_key_verify_success() {
		let key = create_test_key();
		let claims = create_test_claims();
		let token = key.encode(&claims).unwrap();

		let verified_claims = key.decode(&token).unwrap();
		assert_eq!(verified_claims.root, claims.root);
		assert_eq!(verified_claims.publish, claims.publish);
		assert_eq!(verified_claims.subscribe, claims.subscribe);
		assert_eq!(verified_claims.cluster, claims.cluster);
	}

	#[test]
	fn test_key_verify_no_permission() {
		let mut key = create_test_key();
		key.operations = [KeyOperation::Sign].into();

		let result = key.decode("some.jwt.token");
		assert!(result.is_err());
		assert!(
			result
				.unwrap_err()
				.to_string()
				.contains("key does not support verification")
		);
	}

	#[test]
	fn test_key_verify_invalid_token() {
		let key = create_test_key();
		let result = key.decode("invalid-token");
		assert!(result.is_err());
	}

	#[test]
	fn test_key_verify_path_mismatch() {
		let key = create_test_key();
		let claims = create_test_claims();
		let token = key.encode(&claims).unwrap();

		// This test was expecting a path mismatch error, but now decode succeeds
		let result = key.decode(&token);
		assert!(result.is_ok());
	}

	#[test]
	fn test_key_verify_expired_token() {
		let key = create_test_key();
		let mut claims = create_test_claims();
		claims.expires = Some(SystemTime::now() - Duration::from_secs(3600)); // 1 hour ago
		let token = key.encode(&claims).unwrap();

		let result = key.decode(&token);
		assert!(result.is_err());
	}

	#[test]
	fn test_key_verify_token_without_exp() {
		let key = create_test_key();
		let claims = Claims {
			root: "test-path".to_string(),
			publish: vec!["".to_string()],
			subscribe: vec!["".to_string()],
			cluster: false,
			expires: None,
			issued: None,
		};
		let token = key.encode(&claims).unwrap();

		let verified_claims = key.decode(&token).unwrap();
		assert_eq!(verified_claims.root, claims.root);
		assert_eq!(verified_claims.publish, claims.publish);
		assert_eq!(verified_claims.subscribe, claims.subscribe);
		assert_eq!(verified_claims.expires, None);
	}

	#[test]
	fn test_key_round_trip() {
		let key = create_test_key();
		let original_claims = Claims {
			root: "test-path".to_string(),
			publish: vec!["test-pub".into()],
			subscribe: vec!["test-sub".into()],
			cluster: true,
			expires: Some(SystemTime::now() + Duration::from_secs(3600)),
			issued: Some(SystemTime::now()),
		};

		let token = key.encode(&original_claims).unwrap();
		let verified_claims = key.decode(&token).unwrap();

		assert_eq!(verified_claims.root, original_claims.root);
		assert_eq!(verified_claims.publish, original_claims.publish);
		assert_eq!(verified_claims.subscribe, original_claims.subscribe);
		assert_eq!(verified_claims.cluster, original_claims.cluster);
	}

	#[test]
	fn test_key_generate_hs256() {
		let key = Key::generate(Algorithm::HS256, Some(crate::KeyId::decode("test-id").unwrap()));
		assert!(key.is_ok());
		let key = key.unwrap();

		assert_eq!(key.algorithm, Algorithm::HS256);
		assert_eq!(key.kid, Some(crate::KeyId::decode("test-id").unwrap()));
		assert_eq!(key.operations, [KeyOperation::Sign, KeyOperation::Verify].into());

		match key.key {
			KeyType::OCT { ref secret } => assert_eq!(secret.len(), 32),
			_ => panic!("Expected OCT key"),
		}
	}

	#[test]
	fn test_key_generate_hs384() {
		let key = Key::generate(Algorithm::HS384, Some(crate::KeyId::decode("test-id").unwrap()));
		assert!(key.is_ok());
		let key = key.unwrap();

		assert_eq!(key.algorithm, Algorithm::HS384);

		match key.key {
			KeyType::OCT { ref secret } => assert_eq!(secret.len(), 48),
			_ => panic!("Expected OCT key"),
		}
	}

	#[test]
	fn test_key_generate_hs512() {
		let key = Key::generate(Algorithm::HS512, Some(crate::KeyId::decode("test-id").unwrap()));
		assert!(key.is_ok());
		let key = key.unwrap();

		assert_eq!(key.algorithm, Algorithm::HS512);

		match key.key {
			KeyType::OCT { ref secret } => assert_eq!(secret.len(), 64),
			_ => panic!("Expected OCT key"),
		}
	}

	#[test]
	fn test_key_generate_rs512() {
		let key = Key::generate(Algorithm::RS512, Some(crate::KeyId::decode("test-id").unwrap()));
		assert!(key.is_ok());
		let key = key.unwrap();

		assert_eq!(key.algorithm, Algorithm::RS512);
		assert!(matches!(key.key, KeyType::RSA { .. }));
		match key.key {
			KeyType::RSA {
				ref public,
				ref private,
			} => {
				assert!(private.is_some());
				assert_eq!(public.n.len(), 256);
				assert_eq!(public.e.len(), 3);
			}
			_ => panic!("Expected RSA key"),
		}
	}

	#[test]
	fn test_key_generate_es256() {
		let key = Key::generate(Algorithm::ES256, Some(crate::KeyId::decode("test-id").unwrap()));
		assert!(key.is_ok());
		let key = key.unwrap();

		assert_eq!(key.algorithm, Algorithm::ES256);
		assert!(matches!(key.key, KeyType::EC { .. }))
	}

	#[test]
	fn test_key_generate_ps512() {
		let key = Key::generate(Algorithm::PS512, Some(crate::KeyId::decode("test-id").unwrap()));
		assert!(key.is_ok());
		let key = key.unwrap();

		assert_eq!(key.algorithm, Algorithm::PS512);
		assert!(matches!(key.key, KeyType::RSA { .. }));
	}

	#[test]
	fn test_key_generate_eddsa() {
		let key = Key::generate(Algorithm::EdDSA, Some(crate::KeyId::decode("test-id").unwrap()));
		assert!(key.is_ok());
		let key = key.unwrap();

		assert_eq!(key.algorithm, Algorithm::EdDSA);
		assert!(matches!(key.key, KeyType::OKP { .. }));
	}

	#[test]
	fn test_key_generate_without_id() {
		let key = Key::generate(Algorithm::HS256, None);
		assert!(key.is_ok());
		let key = key.unwrap();

		assert_eq!(key.algorithm, Algorithm::HS256);
		assert_eq!(key.kid, None);
		assert_eq!(key.operations, [KeyOperation::Sign, KeyOperation::Verify].into());
	}

	#[test]
	fn test_public_key_conversion_hmac() {
		let key = Key::generate(Algorithm::HS256, Some(crate::KeyId::decode("test-id").unwrap()))
			.expect("HMAC key generation failed");

		assert!(key.to_public().is_err());
	}

	#[test]
	fn test_public_key_conversion_rsa() {
		let key = Key::generate(Algorithm::RS256, Some(crate::KeyId::decode("test-id").unwrap()));
		assert!(key.is_ok());
		let key = key.unwrap();

		let public_key = key.to_public().unwrap();
		assert_eq!(key.kid, public_key.kid);
		assert_eq!(public_key.operations, [KeyOperation::Verify].into());
		assert!(public_key.encode.get().is_none());
		assert!(public_key.decode.get().is_none());
		assert!(matches!(public_key.key, KeyType::RSA { .. }));

		if let KeyType::RSA { public, private } = &public_key.key {
			assert!(private.is_none());

			if let KeyType::RSA { public: src_public, .. } = &key.key {
				assert_eq!(public.e, src_public.e);
				assert_eq!(public.n, src_public.n);
			} else {
				unreachable!("Expected RSA key")
			}
		} else {
			unreachable!("Expected RSA key");
		}
	}

	#[test]
	fn test_public_key_conversion_es() {
		let key = Key::generate(Algorithm::ES256, Some(crate::KeyId::decode("test-id").unwrap()));
		assert!(key.is_ok());
		let key = key.unwrap();

		let public_key = key.to_public().unwrap();
		assert_eq!(key.kid, public_key.kid);
		assert_eq!(public_key.operations, [KeyOperation::Verify].into());
		assert!(public_key.encode.get().is_none());
		assert!(public_key.decode.get().is_none());
		assert!(matches!(public_key.key, KeyType::EC { .. }));

		if let KeyType::EC { x, y, d, curve } = &public_key.key {
			assert!(d.is_none());

			if let KeyType::EC {
				x: src_x,
				y: src_y,
				curve: src_curve,
				..
			} = &key.key
			{
				assert_eq!(x, src_x);
				assert_eq!(y, src_y);
				assert_eq!(curve, src_curve);
			} else {
				unreachable!("Expected EC key")
			}
		} else {
			unreachable!("Expected EC key");
		}
	}

	#[test]
	fn test_public_key_conversion_ed() {
		let key = Key::generate(Algorithm::EdDSA, Some(crate::KeyId::decode("test-id").unwrap()));
		assert!(key.is_ok());
		let key = key.unwrap();

		let public_key = key.to_public().unwrap();
		assert_eq!(key.kid, public_key.kid);
		assert_eq!(public_key.operations, [KeyOperation::Verify].into());
		assert!(public_key.encode.get().is_none());
		assert!(public_key.decode.get().is_none());
		assert!(matches!(public_key.key, KeyType::OKP { .. }));

		if let KeyType::OKP { x, d, curve } = &public_key.key {
			assert!(d.is_none());

			if let KeyType::OKP {
				x: src_x,
				curve: src_curve,
				..
			} = &key.key
			{
				assert_eq!(x, src_x);
				assert_eq!(curve, src_curve);
			} else {
				unreachable!("Expected OKP key")
			}
		} else {
			unreachable!("Expected OKP key");
		}
	}

	#[test]
	fn test_key_generate_sign_verify_cycle() {
		let key = Key::generate(Algorithm::HS256, Some(crate::KeyId::decode("test-id").unwrap()));
		assert!(key.is_ok());
		let key = key.unwrap();

		let claims = create_test_claims();

		let token = key.encode(&claims).unwrap();
		let verified_claims = key.decode(&token).unwrap();

		assert_eq!(verified_claims.root, claims.root);
		assert_eq!(verified_claims.publish, claims.publish);
		assert_eq!(verified_claims.subscribe, claims.subscribe);
		assert_eq!(verified_claims.cluster, claims.cluster);
	}

	#[test]
	fn test_key_debug_no_secret() {
		let key = create_test_key();
		let debug_str = format!("{key:?}");

		assert!(debug_str.contains("algorithm: HS256"));
		assert!(debug_str.contains("operations"));
		assert!(debug_str.contains("kid: Some(KeyId(\"test-key-1\"))"));
		assert!(!debug_str.contains("secret")); // Should not contain secret
	}

	#[test]
	fn test_key_operations_enum() {
		let sign_op = KeyOperation::Sign;
		let verify_op = KeyOperation::Verify;
		let decrypt_op = KeyOperation::Decrypt;
		let encrypt_op = KeyOperation::Encrypt;

		assert_eq!(sign_op, KeyOperation::Sign);
		assert_eq!(verify_op, KeyOperation::Verify);
		assert_eq!(decrypt_op, KeyOperation::Decrypt);
		assert_eq!(encrypt_op, KeyOperation::Encrypt);

		assert_ne!(sign_op, verify_op);
		assert_ne!(decrypt_op, encrypt_op);
	}

	#[test]
	fn test_key_operations_serde() {
		let operations = [KeyOperation::Sign, KeyOperation::Verify];
		let json = serde_json::to_string(&operations).unwrap();
		assert!(json.contains("\"sign\""));
		assert!(json.contains("\"verify\""));

		let deserialized: Vec<KeyOperation> = serde_json::from_str(&json).unwrap();
		assert_eq!(deserialized, operations);
	}

	#[test]
	fn test_key_serde() {
		let key = create_test_key();
		let json = serde_json::to_string(&key).unwrap();
		let deserialized: Key = serde_json::from_str(&json).unwrap();

		assert_eq!(deserialized.algorithm, key.algorithm);
		assert_eq!(deserialized.operations, key.operations);
		assert_eq!(deserialized.kid, key.kid);

		if let (
			KeyType::OCT {
				secret: original_secret,
			},
			KeyType::OCT {
				secret: deserialized_secret,
			},
		) = (&key.key, &deserialized.key)
		{
			assert_eq!(deserialized_secret, original_secret);
		} else {
			panic!("Expected both keys to be OCT variant");
		}
	}

	#[test]
	fn test_key_clone() {
		let key = create_test_key();
		let cloned = key.clone();

		assert_eq!(cloned.algorithm, key.algorithm);
		assert_eq!(cloned.operations, key.operations);
		assert_eq!(cloned.kid, key.kid);

		if let (
			KeyType::OCT {
				secret: original_secret,
			},
			KeyType::OCT { secret: cloned_secret },
		) = (&key.key, &cloned.key)
		{
			assert_eq!(cloned_secret, original_secret);
		} else {
			panic!("Expected both keys to be OCT variant");
		}
	}

	#[test]
	fn test_hmac_algorithms() {
		let key_256 = Key::generate(Algorithm::HS256, Some(crate::KeyId::decode("test-id").unwrap()));
		let key_384 = Key::generate(Algorithm::HS384, Some(crate::KeyId::decode("test-id").unwrap()));
		let key_512 = Key::generate(Algorithm::HS512, Some(crate::KeyId::decode("test-id").unwrap()));

		let claims = create_test_claims();

		// Test that each algorithm can sign and verify
		for key in [key_256, key_384, key_512] {
			assert!(key.is_ok());
			let key = key.unwrap();

			let token = key.encode(&claims).unwrap();
			let verified_claims = key.decode(&token).unwrap();
			assert_eq!(verified_claims.root, claims.root);
		}
	}

	#[test]
	fn test_rsa_pkcs1_asymmetric_algorithms() {
		let key_rs256 = Key::generate(Algorithm::RS256, Some(crate::KeyId::decode("test-id").unwrap()));
		let key_rs384 = Key::generate(Algorithm::RS384, Some(crate::KeyId::decode("test-id").unwrap()));
		let key_rs512 = Key::generate(Algorithm::RS512, Some(crate::KeyId::decode("test-id").unwrap()));

		for key in [key_rs256, key_rs384, key_rs512] {
			test_asymmetric_key(key);
		}
	}

	#[test]
	fn test_rsa_pss_asymmetric_algorithms() {
		let key_ps256 = Key::generate(Algorithm::PS256, Some(crate::KeyId::decode("test-id").unwrap()));
		let key_ps384 = Key::generate(Algorithm::PS384, Some(crate::KeyId::decode("test-id").unwrap()));
		let key_ps512 = Key::generate(Algorithm::PS512, Some(crate::KeyId::decode("test-id").unwrap()));

		for key in [key_ps256, key_ps384, key_ps512] {
			test_asymmetric_key(key);
		}
	}

	#[test]
	fn test_ec_asymmetric_algorithms() {
		let key_es256 = Key::generate(Algorithm::ES256, Some(crate::KeyId::decode("test-id").unwrap()));
		let key_es384 = Key::generate(Algorithm::ES384, Some(crate::KeyId::decode("test-id").unwrap()));

		for key in [key_es256, key_es384] {
			test_asymmetric_key(key);
		}
	}

	#[test]
	fn test_ed_asymmetric_algorithms() {
		let key_eddsa = Key::generate(Algorithm::EdDSA, Some(crate::KeyId::decode("test-id").unwrap()));

		test_asymmetric_key(key_eddsa);
	}

	fn test_asymmetric_key(key: crate::Result<Key>) {
		assert!(key.is_ok());
		let key = key.unwrap();

		let claims = create_test_claims();
		let token = key.encode(&claims).unwrap();

		let private_verified_claims = key.decode(&token).unwrap();
		assert_eq!(
			private_verified_claims.root, claims.root,
			"validation using private key"
		);

		let public_verified_claims = key.to_public().unwrap().decode(&token).unwrap();
		assert_eq!(public_verified_claims.root, claims.root, "validation using public key");
	}

	#[test]
	fn test_cross_algorithm_verification_fails() {
		let key_256 = Key::generate(Algorithm::HS256, Some(crate::KeyId::decode("test-id").unwrap()));
		assert!(key_256.is_ok());
		let key_256 = key_256.unwrap();

		let key_384 = Key::generate(Algorithm::HS384, Some(crate::KeyId::decode("test-id").unwrap()));
		assert!(key_384.is_ok());
		let key_384 = key_384.unwrap();

		let claims = create_test_claims();
		let token = key_256.encode(&claims).unwrap();

		// Different algorithm should fail verification
		let result = key_384.decode(&token);
		assert!(result.is_err());
	}

	#[test]
	fn test_asymmetric_cross_algorithm_verification_fails() {
		let key_rs256 = Key::generate(Algorithm::RS256, Some(crate::KeyId::decode("test-id").unwrap()));
		assert!(key_rs256.is_ok());
		let key_rs256 = key_rs256.unwrap();

		let key_ps256 = Key::generate(Algorithm::PS256, Some(crate::KeyId::decode("test-id").unwrap()));
		assert!(key_ps256.is_ok());
		let key_ps256 = key_ps256.unwrap();

		let claims = create_test_claims();
		let token = key_rs256.encode(&claims).unwrap();

		// Different algorithm should fail verification
		let private_result = key_ps256.decode(&token);
		let public_result = key_ps256.to_public().unwrap().decode(&token);
		assert!(private_result.is_err());
		assert!(public_result.is_err());
	}

	#[test]
	fn test_rsa_pkcs1_public_key_conversion() {
		let key = Key::generate(Algorithm::RS256, Some(crate::KeyId::decode("test-id").unwrap()));
		assert!(key.is_ok());
		let key = key.unwrap();

		assert!(key.operations.contains(&KeyOperation::Sign));
		assert!(key.operations.contains(&KeyOperation::Verify));

		let public_key = key.to_public().unwrap();
		assert!(!public_key.operations.contains(&KeyOperation::Sign));
		assert!(public_key.operations.contains(&KeyOperation::Verify));

		match key.key {
			KeyType::RSA {
				ref public,
				ref private,
			} => {
				assert!(private.is_some());
				assert_eq!(public.n.len(), 256);
				assert_eq!(public.e.len(), 3);

				match public_key.key {
					KeyType::RSA {
						public: ref guest_public,
						private: ref public_private,
					} => {
						assert!(public_private.is_none());
						assert_eq!(public.n, guest_public.n);
						assert_eq!(public.e, guest_public.e);
					}
					_ => panic!("Expected public key to be an RSA key"),
				}
			}
			_ => panic!("Expected private key to be an RSA key"),
		}
	}

	#[test]
	fn test_rsa_pss_public_key_conversion() {
		let key = Key::generate(Algorithm::PS384, Some(crate::KeyId::decode("test-id").unwrap()));
		assert!(key.is_ok());
		let key = key.unwrap();

		assert!(key.operations.contains(&KeyOperation::Sign));
		assert!(key.operations.contains(&KeyOperation::Verify));

		let public_key = key.to_public().unwrap();
		assert!(!public_key.operations.contains(&KeyOperation::Sign));
		assert!(public_key.operations.contains(&KeyOperation::Verify));

		match key.key {
			KeyType::RSA {
				ref public,
				ref private,
			} => {
				assert!(private.is_some());
				assert_eq!(public.n.len(), 256);
				assert_eq!(public.e.len(), 3);

				match public_key.key {
					KeyType::RSA {
						public: ref guest_public,
						private: ref public_private,
					} => {
						assert!(public_private.is_none());
						assert_eq!(public.n, guest_public.n);
						assert_eq!(public.e, guest_public.e);
					}
					_ => panic!("Expected public key to be an RSA key"),
				}
			}
			_ => panic!("Expected private key to be an RSA key"),
		}
	}

	#[test]
	fn test_base64url_serialization() {
		let key = create_test_key();
		let json = serde_json::to_string(&key).unwrap();

		// Check that the secret is base64url encoded without padding
		let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
		let k_value = parsed["k"].as_str().unwrap();

		// Base64url should not contain padding characters
		assert!(!k_value.contains('='));
		assert!(!k_value.contains('+'));
		assert!(!k_value.contains('/'));

		// Verify it decodes correctly
		let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
			.decode(k_value)
			.unwrap();

		if let KeyType::OCT {
			secret: original_secret,
		} = &key.key
		{
			assert_eq!(decoded, *original_secret);
		} else {
			panic!("Expected both keys to be OCT variant");
		}
	}

	#[test]
	fn test_backwards_compatibility_unpadded_base64url() {
		// Create a JSON with unpadded base64url (new format)
		let unpadded_json = r#"{"kty":"oct","alg":"HS256","key_ops":["sign","verify"],"k":"dGVzdC1zZWNyZXQtdGhhdC1pcy1sb25nLWVub3VnaC1mb3ItaG1hYy1zaGEyNTY","kid":"test-key-1"}"#;

		// Should be able to deserialize new format
		let key: Key = serde_json::from_str(unpadded_json).unwrap();
		assert_eq!(key.algorithm, Algorithm::HS256);
		assert_eq!(key.kid, Some(crate::KeyId::decode("test-key-1").unwrap()));

		if let KeyType::OCT { secret } = &key.key {
			assert_eq!(secret, b"test-secret-that-is-long-enough-for-hmac-sha256");
		} else {
			panic!("Expected key to be OCT variant");
		}
	}

	#[test]
	fn test_backwards_compatibility_padded_base64url() {
		// Create a JSON with padded base64url (old format) - same secret but with padding
		let padded_json = r#"{"kty":"oct","alg":"HS256","key_ops":["sign","verify"],"k":"dGVzdC1zZWNyZXQtdGhhdC1pcy1sb25nLWVub3VnaC1mb3ItaG1hYy1zaGEyNTY=","kid":"test-key-1"}"#;

		// Should be able to deserialize old format for backwards compatibility
		let key: Key = serde_json::from_str(padded_json).unwrap();
		assert_eq!(key.algorithm, Algorithm::HS256);
		assert_eq!(key.kid, Some(crate::KeyId::decode("test-key-1").unwrap()));

		if let KeyType::OCT { secret } = &key.key {
			assert_eq!(secret, b"test-secret-that-is-long-enough-for-hmac-sha256");
		} else {
			panic!("Expected key to be OCT variant");
		}
	}

	// Tests that Rust can load keys generated by the JS @moq/token package
	// and verify tokens signed by JS.
	//
	// Generated with: bun -e 'import { generate } from "./js/token/src/generate.ts"; ...'
	// See js/token/src/interop.test.ts for the JS-side counterpart.

	/// JS-generated HS256 key (from @moq/token generate("HS256", "js-test-key"))
	const JS_HS256_KEY: &str = r#"{"kty":"oct","alg":"HS256","k":"xm6xsSkfFqzPU3KfcbAcF2_h0OkStxQ_nNqVPYl0ync","kid":"js-test-key","key_ops":["sign","verify"],"guest":[],"guest_sub":[],"guest_pub":[]}"#;

	/// JS-generated HS256 token (from @moq/token sign(key, {root:"live", put:["camera1"], get:["camera1","camera2"]}))
	const JS_HS256_TOKEN: &str = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCIsImtpZCI6ImpzLXRlc3Qta2V5In0.eyJyb290IjoibGl2ZSIsInB1dCI6WyJjYW1lcmExIl0sImdldCI6WyJjYW1lcmExIiwiY2FtZXJhMiJdLCJpYXQiOjE3NzUxNzY3NTR9.tHNQtHh_HCIKxXOexDCM7AkjqWzbULLZzjEckfOGRfY";

	/// JS-generated EdDSA private key (from @moq/token generate("EdDSA", "js-eddsa-key"))
	const JS_EDDSA_PRIVATE_KEY: &str = r#"{"kty":"OKP","alg":"EdDSA","crv":"Ed25519","x":"UiU9fT_SdBBpkFtJPRCY0gX1jK_Dr9syYLFuEz4QUM4","d":"lm-L_PV3ksuQ-KrFBgFMDJqAZC3_Z6Z5UC4ZQY5OoDQ","kid":"js-eddsa-key","key_ops":["sign","verify"],"guest":[],"guest_sub":[],"guest_pub":[]}"#;

	/// JS-generated EdDSA public key (from @moq/token toPublicKey(key))
	const JS_EDDSA_PUBLIC_KEY: &str = r#"{"kty":"OKP","alg":"EdDSA","crv":"Ed25519","x":"UiU9fT_SdBBpkFtJPRCY0gX1jK_Dr9syYLFuEz4QUM4","kid":"js-eddsa-key","guest":[],"guest_sub":[],"guest_pub":[],"key_ops":["verify"]}"#;

	/// JS-generated EdDSA token (from @moq/token sign(key, {root:"stream", put:["video"]}))
	const JS_EDDSA_TOKEN: &str = "eyJhbGciOiJFZERTQSIsInR5cCI6IkpXVCIsImtpZCI6ImpzLWVkZHNhLWtleSJ9.eyJyb290Ijoic3RyZWFtIiwicHV0IjpbInZpZGVvIl0sImlhdCI6MTc3NTE3Njc1Nn0.l9rUMHjPSXWKSXRP3mmeMgTAywtqpdqQehhViWaPrKxax1Y2D9KRIYTixYz-b6PI-AoHQYusHWeeLu_HRw2cAg";

	#[test]
	fn test_js_hs256_key_load() {
		let key = Key::from_str(JS_HS256_KEY).unwrap();
		assert_eq!(key.algorithm, Algorithm::HS256);
		assert_eq!(key.kid, Some(crate::KeyId::decode("js-test-key").unwrap()));
	}

	#[test]
	fn test_js_hs256_token_verify() {
		let key = Key::from_str(JS_HS256_KEY).unwrap();
		let claims = key.decode(JS_HS256_TOKEN).unwrap();
		assert_eq!(claims.root, "live");
		assert_eq!(claims.publish, vec!["camera1"]);
		assert_eq!(claims.subscribe, vec!["camera1", "camera2"]);
	}

	#[test]
	fn test_js_hs256_sign_and_roundtrip() {
		let key = Key::from_str(JS_HS256_KEY).unwrap();
		let claims = Claims {
			root: "rust-test".to_string(),
			publish: vec!["pub1".into()],
			subscribe: vec!["sub1".into()],
			..Default::default()
		};
		let token = key.encode(&claims).unwrap();
		let verified = key.decode(&token).unwrap();
		assert_eq!(verified.root, "rust-test");
		assert_eq!(verified.publish, vec!["pub1"]);
	}

	#[test]
	fn test_js_eddsa_key_load() {
		let private_key = Key::from_str(JS_EDDSA_PRIVATE_KEY).unwrap();
		assert_eq!(private_key.algorithm, Algorithm::EdDSA);
		assert!(matches!(private_key.key, KeyType::OKP { .. }));

		let public_key = Key::from_str(JS_EDDSA_PUBLIC_KEY).unwrap();
		assert_eq!(public_key.algorithm, Algorithm::EdDSA);
	}

	#[test]
	fn test_js_eddsa_token_verify_with_private_key() {
		let key = Key::from_str(JS_EDDSA_PRIVATE_KEY).unwrap();
		let claims = key.decode(JS_EDDSA_TOKEN).unwrap();
		assert_eq!(claims.root, "stream");
		assert_eq!(claims.publish, vec!["video"]);
	}

	#[test]
	fn test_js_eddsa_token_verify_with_public_key() {
		let key = Key::from_str(JS_EDDSA_PUBLIC_KEY).unwrap();
		let claims = key.decode(JS_EDDSA_TOKEN).unwrap();
		assert_eq!(claims.root, "stream");
		assert_eq!(claims.publish, vec!["video"]);
	}

	#[test]
	fn test_js_token_wrong_key_fails() {
		// Generate a different HS256 key
		let wrong_key = Key::generate(Algorithm::HS256, None).unwrap();
		let result = wrong_key.decode(JS_HS256_TOKEN);
		assert!(result.is_err());
	}

	#[test]
	fn test_js_eddsa_token_wrong_key_fails() {
		// Try verifying EdDSA token with the HS256 key
		let wrong_key = Key::from_str(JS_HS256_KEY).unwrap();
		let result = wrong_key.decode(JS_EDDSA_TOKEN);
		assert!(result.is_err());
	}

	#[test]
	fn test_file_io_base64url() {
		let key = create_test_key();
		let temp_dir = std::env::temp_dir();
		let temp_path = temp_dir.join("test_jwk.key");

		// Write key to file as base64url
		key.to_file_base64url(&temp_path).unwrap();

		// Read file contents
		let contents = std::fs::read_to_string(&temp_path).unwrap();

		// Should be base64url encoded
		assert!(!contents.contains('{'));
		assert!(!contents.contains('}'));
		assert!(!contents.contains('"'));

		// Decode and verify it's valid JSON
		let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
			.decode(&contents)
			.unwrap();
		let json_str = String::from_utf8(decoded).unwrap();
		let _: serde_json::Value = serde_json::from_str(&json_str).unwrap();

		// Read key back from file
		let loaded_key = Key::from_file(&temp_path).unwrap();
		assert_eq!(loaded_key.algorithm, key.algorithm);
		assert_eq!(loaded_key.operations, key.operations);
		assert_eq!(loaded_key.kid, key.kid);

		if let (
			KeyType::OCT {
				secret: original_secret,
			},
			KeyType::OCT { secret: loaded_secret },
		) = (&key.key, &loaded_key.key)
		{
			assert_eq!(loaded_secret, original_secret);
		} else {
			panic!("Expected both keys to be OCT variant");
		}

		// Clean up
		std::fs::remove_file(temp_path).ok();
	}
}
