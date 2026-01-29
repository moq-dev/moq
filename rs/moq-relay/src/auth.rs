use anyhow::Context;
use axum::http;
use moq_lite::{AsPath, Path, PathOwned};
use moq_token::KeySet;
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(thiserror::Error, Debug, Clone)]
pub enum AuthError {
	#[error("authentication is disabled")]
	UnexpectedToken,

	#[error("a token was expected")]
	ExpectedToken,

	#[error("failed to decode the token")]
	DecodeFailed,

	#[error("the path does not match the root")]
	IncorrectRoot,
}

impl From<AuthError> for http::StatusCode {
	fn from(_: AuthError) -> Self {
		http::StatusCode::UNAUTHORIZED
	}
}

impl axum::response::IntoResponse for AuthError {
	fn into_response(self) -> axum::response::Response {
		http::StatusCode::UNAUTHORIZED.into_response()
	}
}

#[derive(clap::Args, Clone, Debug, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct AuthConfig {
	/// Either the root authentication key or a URI to a JWK set.
	/// If present, all paths will require a token unless they are in the public list.
	#[arg(long = "auth-key", env = "MOQ_AUTH_KEY")]
	pub key: Option<String>,

	/// How often to refresh the JWK set (in seconds), will be ignored if the `key` is not a valid URI.
	/// If not provided, there won't be any refreshing, the JWK set will only be loaded once at startup.
	/// Minimum value: 30, defaults to None
	#[arg(long = "auth-refresh-interval", env = "MOQ_AUTH_REFRESH_INTERVAL")]
	pub refresh_interval: Option<u64>,

	/// The prefix that will be public for reading and writing.
	/// If present, unauthorized users will be able to read and write to this prefix ONLY.
	/// If a user provides a token, then they can only access the prefix only if it is specified in the token.
	#[arg(long = "auth-public", env = "MOQ_AUTH_PUBLIC")]
	pub public: Option<String>,
}

impl AuthConfig {
	pub async fn init(self) -> anyhow::Result<Auth> {
		Auth::new(self).await
	}
}

#[derive(Debug)]
pub struct AuthToken {
	pub root: PathOwned,
	pub subscribe: Vec<PathOwned>,
	pub publish: Vec<PathOwned>,
	pub cluster: bool,
}

const REFRESH_ERROR_INTERVAL: Duration = Duration::from_mins(5);

#[derive(Clone)]
pub struct Auth {
	key: Option<Arc<Mutex<KeySet>>>,
	public: Option<PathOwned>,
	refresh_task: Option<Arc<tokio::task::JoinHandle<()>>>,
}

impl Drop for Auth {
	fn drop(&mut self) {
		if let Some(handle) = self.refresh_task.as_ref()
			&& Arc::strong_count(handle) == 1
		{
			handle.abort();
		}
	}
}

impl Auth {
	fn compare_key_sets(previous: &KeySet, new: &KeySet) {
		for new_key in new.keys.iter() {
			if new_key.kid.is_some() && !previous.keys.iter().any(|k| k.kid == new_key.kid) {
				tracing::info!("found new JWK \"{}\"", new_key.kid.as_deref().unwrap())
			}
		}
		for old_key in previous.keys.iter() {
			if old_key.kid.is_some() && !new.keys.iter().any(|k| k.kid == old_key.kid) {
				tracing::info!("removed JWK \"{}\"", old_key.kid.as_deref().unwrap())
			}
		}
	}

	async fn refresh_key_set(jwks_uri: &str, key_set: &Mutex<KeySet>) -> anyhow::Result<()> {
		let new_keys = moq_token::load_keys(jwks_uri).await?;

		let mut key_set = key_set.lock().expect("keyset mutex poisoned");
		Self::compare_key_sets(&key_set, &new_keys);
		*key_set = new_keys;

		Ok(())
	}

	async fn refresh_task(interval: Duration, key_set: Arc<Mutex<KeySet>>, jwks_uri: String) {
		loop {
			tokio::time::sleep(interval).await;

			if let Err(e) = Self::refresh_key_set(&jwks_uri, key_set.as_ref()).await {
				if interval > REFRESH_ERROR_INTERVAL * 2 {
					tracing::error!(
						"failed to load JWKS, will retry in {} seconds: {:?}",
						REFRESH_ERROR_INTERVAL.as_secs(),
						e
					);
					tokio::time::sleep(REFRESH_ERROR_INTERVAL).await;

					if let Err(e) = Self::refresh_key_set(&jwks_uri, key_set.as_ref()).await {
						tracing::error!("failed to load JWKS again, giving up this time: {:?}", e);
					} else {
						tracing::info!("successfully loaded JWKS on the second try");
					}
				} else {
					// Don't retry because the next refresh is going to happen very soon
					tracing::error!("failed to refresh JWKS: {:?}", e);
				}
			}
		}
	}

	pub async fn new(config: AuthConfig) -> anyhow::Result<Self> {
		if let Some(key) = config.key {
			if config.public.is_some() {
				anyhow::bail!("root key and public path cannot be configured together");
			}

			if key.starts_with("http://") || key.starts_with("https://") {
				Self::new_remote(key, config.refresh_interval).await
			} else {
				Self::new_local(key)
			}
		} else if let Some(public) = config.public {
			Ok(Self {
				key: None,
				public: Some(public.as_path().to_owned()),
				refresh_task: None,
			})
		} else {
			anyhow::bail!("no root key or public path configured");
		}
	}

	async fn new_remote(uri: String, refresh_interval: Option<u64>) -> anyhow::Result<Self> {
		// Start with an empty KeySet
		let key_set = Arc::new(Mutex::new(KeySet::default()));

		tracing::info!(%uri, "loading JWK set");

		Self::refresh_key_set(&uri, key_set.as_ref()).await?;

		let refresh_task = if let Some(refresh_interval_secs) = refresh_interval {
			anyhow::ensure!(refresh_interval_secs >= 30, "refresh_interval cannot be less than 30");

			Some(Arc::new(tokio::spawn(Self::refresh_task(
				Duration::from_secs(refresh_interval_secs),
				key_set.clone(),
				uri,
			))))
		} else {
			None
		};

		Ok(Self {
			key: Some(key_set),
			public: None,
			refresh_task,
		})
	}

	fn new_local(key: String) -> anyhow::Result<Self> {
		let key = moq_token::Key::from_file(&key).context("cannot load key")?;
		let key_set = Arc::new(Mutex::new(KeySet {
			keys: vec![Arc::new(key)],
		}));

		Ok(Self {
			key: Some(key_set),
			public: None,
			refresh_task: None,
		})
	}

	// Parse the token from the user provided URL, returning the claims if successful.
	// If no token is provided, then the claims will use the public path if it is set.
	pub fn verify(&self, path: &str, token: Option<&str>) -> Result<AuthToken, AuthError> {
		// Find the token in the query parameters.
		// ?jwt=...
		let claims = if let Some(token) = token
			&& let Some(key) = self.key.as_deref()
		{
			key.lock()
				.expect("key mutex poisoned")
				.decode(token)
				.map_err(|_| AuthError::DecodeFailed)?
		} else if let Some(_token) = token {
			return Err(AuthError::UnexpectedToken);
		} else if let Some(public) = &self.public {
			moq_token::Claims {
				root: public.to_string(),
				subscribe: vec!["".to_string()],
				publish: vec!["".to_string()],
				..Default::default()
			}
		} else {
			return Err(AuthError::ExpectedToken);
		};

		// Get the path from the URL, removing any leading or trailing slashes.
		// We will automatically add a trailing slash when joining the path with the subscribe/publish roots.
		let root = Path::new(path);

		// Make sure the URL path matches the root path.
		let Some(suffix) = root.strip_prefix(&claims.root) else {
			return Err(AuthError::IncorrectRoot);
		};

		// If a more specific path is provided, reduce the permissions.
		let subscribe = claims
			.subscribe
			.into_iter()
			.filter_map(|p| {
				let p = Path::new(&p);
				if !p.is_empty() {
					p.strip_prefix(&suffix).map(|p| p.to_owned())
				} else {
					Some(p.to_owned())
				}
			})
			.collect();

		let publish = claims
			.publish
			.into_iter()
			.filter_map(|p| {
				let p = Path::new(&p);
				if !p.is_empty() {
					p.strip_prefix(&suffix).map(|p| p.to_owned())
				} else {
					Some(p.to_owned())
				}
			})
			.collect();

		Ok(AuthToken {
			root: root.to_owned(),
			subscribe,
			publish,
			cluster: claims.cluster,
		})
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use moq_token::{Algorithm, Key};
	use tempfile::NamedTempFile;

	fn create_test_key() -> anyhow::Result<(NamedTempFile, Key)> {
		let key_file = NamedTempFile::new()?;
		let key = Key::generate(Algorithm::HS256, None)?;
		key.to_file(key_file.path())?;
		Ok((key_file, key))
	}

	#[tokio::test]
	async fn test_anonymous_access_with_public_path() -> anyhow::Result<()> {
		// Test anonymous access to /anon path
		let auth = Auth::new(AuthConfig {
			public: Some("anon".to_string()),
			..Default::default()
		})
		.await?;

		// Should succeed for anonymous path
		let token = auth.verify("/anon", None)?;
		assert_eq!(token.root, "anon".as_path());
		assert_eq!(token.subscribe, vec!["".as_path()]);
		assert_eq!(token.publish, vec!["".as_path()]);

		// Should succeed for sub-paths under anonymous
		let token = auth.verify("/anon/room/123", None)?;
		assert_eq!(token.root, Path::new("anon/room/123").to_owned());
		assert_eq!(token.subscribe, vec![Path::new("").to_owned()]);
		assert_eq!(token.publish, vec![Path::new("").to_owned()]);

		Ok(())
	}

	#[tokio::test]
	async fn test_anonymous_access_fully_public() -> anyhow::Result<()> {
		// Test fully public access (public = "")
		let auth = Auth::new(AuthConfig {
			public: Some("".to_string()),
			..Default::default()
		})
		.await?;

		// Should succeed for any path
		let token = auth.verify("/any/path", None)?;
		assert_eq!(token.root, Path::new("any/path").to_owned());
		assert_eq!(token.subscribe, vec![Path::new("").to_owned()]);
		assert_eq!(token.publish, vec![Path::new("").to_owned()]);

		Ok(())
	}

	#[tokio::test]
	async fn test_anonymous_access_denied_wrong_prefix() -> anyhow::Result<()> {
		// Test anonymous access denied for wrong prefix
		let auth = Auth::new(AuthConfig {
			public: Some("anon".to_string()),
			..Default::default()
		})
		.await?;

		// Should fail for non-anonymous path
		let result = auth.verify("/secret", None);
		assert!(result.is_err());

		Ok(())
	}

	#[tokio::test]
	async fn test_no_token_no_public_path_fails() -> anyhow::Result<()> {
		let (key_file, _) = create_test_key()?;
		let auth = Auth::new(AuthConfig {
			key: Some(key_file.path().to_string_lossy().to_string()),
			..Default::default()
		})
		.await?;

		// Should fail when no token and no public path
		let result = auth.verify("/any/path", None);
		assert!(result.is_err());

		Ok(())
	}

	#[tokio::test]
	async fn test_token_provided_but_no_key_configured() -> anyhow::Result<()> {
		let auth = Auth::new(AuthConfig {
			public: Some("anon".to_string()),
			..Default::default()
		})
		.await?;

		// Should fail when token provided but no key configured
		let result = auth.verify("/any/path", Some("fake-token"));
		assert!(result.is_err());

		Ok(())
	}

	#[tokio::test]
	async fn test_jwt_token_basic_validation() -> anyhow::Result<()> {
		let (key_file, key) = create_test_key()?;
		let auth = Auth::new(AuthConfig {
			key: Some(key_file.path().to_string_lossy().to_string()),
			..Default::default()
		})
		.await?;

		// Create a token with basic permissions
		let claims = moq_token::Claims {
			root: "room/123".to_string(),
			subscribe: vec!["".to_string()],
			publish: vec!["alice".into()],
			..Default::default()
		};
		let token = key.encode(&claims)?;

		// Should succeed with valid token and matching path
		let token = auth.verify("/room/123", Some(&token))?;
		assert_eq!(token.root, "room/123".as_path());
		assert_eq!(token.subscribe, vec!["".as_path()]);
		assert_eq!(token.publish, vec!["alice".as_path()]);

		Ok(())
	}

	#[tokio::test]
	async fn test_jwt_token_wrong_root_path() -> anyhow::Result<()> {
		let (key_file, key) = create_test_key()?;
		let auth = Auth::new(AuthConfig {
			key: Some(key_file.path().to_string_lossy().to_string()),
			..Default::default()
		})
		.await?;

		// Create a token for room/123
		let claims = moq_token::Claims {
			root: "room/123".to_string(),
			subscribe: vec!["".to_string()],
			publish: vec!["".to_string()],
			..Default::default()
		};
		let token = key.encode(&claims)?;

		// Should fail when trying to access wrong path
		let result = auth.verify("/secret", Some(&token));
		assert!(result.is_err());

		Ok(())
	}

	#[tokio::test]
	async fn test_jwt_token_with_restricted_publish_subscribe() -> anyhow::Result<()> {
		let (key_file, key) = create_test_key()?;
		let auth = Auth::new(AuthConfig {
			key: Some(key_file.path().to_string_lossy().to_string()),
			..Default::default()
		})
		.await?;

		// Create a token with specific pub/sub restrictions
		let claims = moq_token::Claims {
			root: "room/123".to_string(),
			subscribe: vec!["bob".into()],
			publish: vec!["alice".into()],
			..Default::default()
		};
		let token = key.encode(&claims)?;

		// Verify the restrictions are preserved
		let token = auth.verify("/room/123", Some(&token))?;
		assert_eq!(token.root, "room/123".as_path());
		assert_eq!(token.subscribe, vec!["bob".as_path()]);
		assert_eq!(token.publish, vec!["alice".as_path()]);

		Ok(())
	}

	#[tokio::test]
	async fn test_jwt_token_read_only() -> anyhow::Result<()> {
		let (key_file, key) = create_test_key()?;
		let auth = Auth::new(AuthConfig {
			key: Some(key_file.path().to_string_lossy().to_string()),
			..Default::default()
		})
		.await?;

		// Create a read-only token (no publish permissions)
		let claims = moq_token::Claims {
			root: "room/123".to_string(),
			subscribe: vec!["".to_string()],
			publish: vec![],
			..Default::default()
		};
		let token = key.encode(&claims)?;

		let token = auth.verify("/room/123", Some(&token))?;
		assert_eq!(token.subscribe, vec!["".as_path()]);
		assert_eq!(token.publish, vec![]);

		Ok(())
	}

	#[tokio::test]
	async fn test_jwt_token_write_only() -> anyhow::Result<()> {
		let (key_file, key) = create_test_key()?;
		let auth = Auth::new(AuthConfig {
			key: Some(key_file.path().to_string_lossy().to_string()),
			..Default::default()
		})
		.await?;

		// Create a write-only token (no subscribe permissions)
		let claims = moq_token::Claims {
			root: "room/123".to_string(),
			subscribe: vec![],
			publish: vec!["bob".into()],
			..Default::default()
		};
		let token = key.encode(&claims)?;

		let token = auth.verify("/room/123", Some(&token))?;
		assert_eq!(token.subscribe, vec![]);
		assert_eq!(token.publish, vec!["bob".as_path()]);

		Ok(())
	}

	#[tokio::test]
	async fn test_claims_reduction_basic() -> anyhow::Result<()> {
		let (key_file, key) = create_test_key()?;
		let auth = Auth::new(AuthConfig {
			key: Some(key_file.path().to_string_lossy().to_string()),
			..Default::default()
		})
		.await?;

		// Create a token with root at room/123 and unrestricted pub/sub
		let claims = moq_token::Claims {
			root: "room/123".to_string(),
			subscribe: vec!["".to_string()],
			publish: vec!["".to_string()],
			..Default::default()
		};
		let token = key.encode(&claims)?;

		// Connect to more specific path room/123/alice
		let token = auth.verify("/room/123/alice", Some(&token))?;

		// Root should be updated to the more specific path
		assert_eq!(token.root, Path::new("room/123/alice"));
		// Empty permissions remain empty (full access under new root)
		assert_eq!(token.subscribe, vec!["".as_path()]);
		assert_eq!(token.publish, vec!["".as_path()]);

		Ok(())
	}

	#[tokio::test]
	async fn test_claims_reduction_with_publish_restrictions() -> anyhow::Result<()> {
		let (key_file, key) = create_test_key()?;
		let auth = Auth::new(AuthConfig {
			key: Some(key_file.path().to_string_lossy().to_string()),
			..Default::default()
		})
		.await?;

		// Token allows publishing only to alice/*
		let claims = moq_token::Claims {
			root: "room/123".to_string(),
			subscribe: vec!["".to_string()],
			publish: vec!["alice".into()],
			..Default::default()
		};
		let token = key.encode(&claims)?;

		// Connect to room/123/alice - should remove alice prefix from publish
		let token = auth.verify("/room/123/alice", Some(&token))?;

		assert_eq!(token.root, "room/123/alice".as_path());
		// Alice still can't subscribe to anything.
		assert_eq!(token.subscribe, vec!["".as_path()]);
		// alice prefix stripped, now can publish to everything under room/123/alice
		assert_eq!(token.publish, vec!["".as_path()]);

		Ok(())
	}

	#[tokio::test]
	async fn test_claims_reduction_with_subscribe_restrictions() -> anyhow::Result<()> {
		let (key_file, key) = create_test_key()?;
		let auth = Auth::new(AuthConfig {
			key: Some(key_file.path().to_string_lossy().to_string()),
			..Default::default()
		})
		.await?;

		// Token allows subscribing only to bob/*
		let claims = moq_token::Claims {
			root: "room/123".to_string(),
			subscribe: vec!["bob".into()],
			publish: vec!["".to_string()],
			..Default::default()
		};
		let token = key.encode(&claims)?;

		// Connect to room/123/bob - should remove bob prefix from subscribe
		let token = auth.verify("/room/123/bob", Some(&token))?;

		assert_eq!(token.root, "room/123/bob".as_path());
		// bob prefix stripped, now can subscribe to everything under room/123/bob
		assert_eq!(token.subscribe, vec!["".as_path()]);
		assert_eq!(token.publish, vec!["".as_path()]);

		Ok(())
	}

	#[tokio::test]
	async fn test_claims_reduction_loses_access() -> anyhow::Result<()> {
		let (key_file, key) = create_test_key()?;
		let auth = Auth::new(AuthConfig {
			key: Some(key_file.path().to_string_lossy().to_string()),
			..Default::default()
		})
		.await?;

		// Token allows publishing to alice/* and subscribing to bob/*
		let claims = moq_token::Claims {
			root: "room/123".to_string(),
			subscribe: vec!["bob".into()],
			publish: vec!["alice".into()],
			..Default::default()
		};
		let token = key.encode(&claims)?;

		// Connect to room/123/alice - loses ability to subscribe to bob
		let verified = auth.verify("/room/123/alice", Some(&token))?;

		assert_eq!(verified.root, "room/123/alice".as_path());
		// Can't subscribe to bob anymore (alice doesn't have bob prefix)
		assert_eq!(verified.subscribe, vec![]);
		// Can publish to everything under alice
		assert_eq!(verified.publish, vec!["".as_path()]);

		// Connect to room/123/bob - loses ability to publish to alice
		let token = auth.verify("/room/123/bob", Some(&token))?;

		assert_eq!(token.root, "room/123/bob".as_path());
		// Can subscribe to everything under bob
		assert_eq!(token.subscribe, vec!["".as_path()]);
		// Can't publish to alice anymore (bob doesn't have alice prefix)
		assert_eq!(token.publish, vec![]);

		Ok(())
	}

	#[tokio::test]
	async fn test_claims_reduction_nested_paths() -> anyhow::Result<()> {
		let (key_file, key) = create_test_key()?;
		let auth = Auth::new(AuthConfig {
			key: Some(key_file.path().to_string_lossy().to_string()),
			..Default::default()
		})
		.await?;

		// Token with nested publish/subscribe paths
		let claims = moq_token::Claims {
			root: "room/123".to_string(),
			subscribe: vec!["users/bob/screen".into()],
			publish: vec!["users/alice/camera".into()],
			..Default::default()
		};
		let token = key.encode(&claims)?;

		// Connect to room/123/users - permissions should be reduced
		let verified = auth.verify("/room/123/users", Some(&token))?;

		assert_eq!(verified.root, "room/123/users".as_path());
		// users prefix removed from paths
		assert_eq!(verified.subscribe, vec!["bob/screen".as_path()]);
		assert_eq!(verified.publish, vec!["alice/camera".as_path()]);

		// Connect to room/123/users/alice - further reduction
		let token = auth.verify("/room/123/users/alice", Some(&token))?;

		assert_eq!(token.root, "room/123/users/alice".as_path());
		// Can't subscribe (alice doesn't have bob prefix)
		assert_eq!(token.subscribe, vec![]);
		// users/alice prefix removed, left with camera
		assert_eq!(token.publish, vec!["camera".as_path()]);

		Ok(())
	}

	#[tokio::test]
	async fn test_claims_reduction_preserves_read_write_only() -> anyhow::Result<()> {
		let (key_file, key) = create_test_key()?;
		let auth = Auth::new(AuthConfig {
			key: Some(key_file.path().to_string_lossy().to_string()),
			..Default::default()
		})
		.await?;

		// Read-only token
		let claims = moq_token::Claims {
			root: "room/123".to_string(),
			subscribe: vec!["alice".into()],
			publish: vec![],
			..Default::default()
		};
		let token = key.encode(&claims)?;

		// Connect to more specific path
		let token = auth.verify("/room/123/alice", Some(&token))?;

		// Should remain read-only
		assert_eq!(token.subscribe, vec!["".as_path()]);
		assert_eq!(token.publish, vec![]);

		// Write-only token
		let claims = moq_token::Claims {
			root: "room/123".to_string(),
			subscribe: vec![],
			publish: vec!["alice".into()],
			..Default::default()
		};
		let token = key.encode(&claims)?;

		let verified = auth.verify("/room/123/alice", Some(&token))?;

		// Should remain write-only
		assert_eq!(verified.subscribe, vec![]);
		assert_eq!(verified.publish, vec!["".as_path()]);

		Ok(())
	}
}
