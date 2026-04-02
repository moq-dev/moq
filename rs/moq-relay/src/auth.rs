use anyhow::Context;
use axum::http;
use http_cache_reqwest::{Cache, CacheMode, HttpCache, HttpCacheOptions, MokaManager};
use moq_lite::{AsPath, Path, PathOwned};
use moq_token::{Key, KeyId};
use reqwest_middleware::ClientWithMiddleware;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;

/// Parameters extracted from an incoming connection URL for authentication.
#[derive(Default, Debug)]
pub struct AuthParams {
	/// The URL path identifying the broadcast root.
	pub path: String,
	/// A JWT token, if provided via the `jwt` query parameter.
	pub jwt: Option<String>,
	/// A cluster registration identifier, if provided via the `register` query parameter.
	pub register: Option<String>,
}

impl AuthParams {
	/// Creates params with just a path and no token or registration.
	pub fn new(path: impl Into<String>) -> Self {
		Self {
			path: path.into(),
			..Default::default()
		}
	}

	/// Extracts authentication parameters from a URL's path and query string.
	pub fn from_url(url: &url::Url) -> Self {
		let path = url.path().to_string();
		let mut jwt = None;
		let mut register = None;

		for (k, v) in url.query_pairs() {
			if v.is_empty() {
				continue;
			}
			match k.as_ref() {
				"jwt" => jwt = Some(v.into_owned()),
				"register" => register = Some(v.into_owned()),
				_ => {}
			}
		}

		Self { path, jwt, register }
	}
}

/// Errors returned when authentication or authorization fails.
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

	#[error("a cluster token was expected")]
	ExpectedCluster,

	#[error("key not found")]
	KeyNotFound,

	#[error("missing or invalid key ID in token")]
	MissingKeyId,
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

/// Configuration for JWT-based authentication.
#[derive(clap::Args, Clone, Debug, Serialize, Deserialize, Default)]
#[serde(default)]
#[non_exhaustive]
pub struct AuthConfig {
	/// A single JWK key file for authentication.
	/// No `kid` header is required in JWTs.
	#[arg(long = "auth-key", env = "MOQ_AUTH_KEY")]
	pub key: Option<String>,

	/// A directory path or base URL containing JWK files named by key ID.
	///
	/// File path: reads `{dir}/{kid}.jwk` from disk.
	/// URL: fetches `{url}/{kid}.jwk` with HTTP caching.
	#[arg(long = "auth-key-dir", env = "MOQ_AUTH_KEY_DIR")]
	pub key_dir: Option<String>,

	/// The prefix that will be public for reading and writing.
	/// If present, unauthorized users will be able to read and write to this prefix ONLY.
	/// If a user provides a token, then they can only access the prefix only if it is specified in the token.
	#[arg(long = "auth-public", env = "MOQ_AUTH_PUBLIC")]
	pub public: Option<String>,
}

impl AuthConfig {
	/// Initializes an [`Auth`] instance from this configuration.
	pub async fn init(self) -> anyhow::Result<Auth> {
		Auth::new(self).await
	}
}

/// The result of a successful authentication, containing the resolved
/// permissions for a connection.
#[derive(Debug)]
pub struct AuthToken {
	/// The root path this token is scoped to.
	pub root: PathOwned,
	/// Paths the holder is allowed to subscribe to, relative to `root`.
	pub subscribe: Vec<PathOwned>,
	/// Paths the holder is allowed to publish to, relative to `root`.
	pub publish: Vec<PathOwned>,
	/// Whether this token grants cluster-level privileges.
	pub cluster: bool,
	/// The cluster node name to register, if this is a cluster connection.
	pub register: Option<String>,
}

enum KeySource {
	/// A single key file. No kid required.
	File(PathBuf),
	/// A directory of key files, resolved by kid as `{dir}/{kid}.jwk`.
	Dir(PathBuf),
	/// A single key URL. No kid required.
	Url {
		url: url::Url,
		client: ClientWithMiddleware,
	},
	/// A base URL for kid-based key lookup, fetching `{base}/{kid}.jwk`.
	UrlDir {
		base: url::Url,
		client: ClientWithMiddleware,
	},
}

struct KeyResolver {
	source: KeySource,
}

impl KeyResolver {
	fn new(source: KeySource) -> Self {
		Self { source }
	}

	async fn resolve(&self, kid: Option<&str>) -> Result<Arc<Key>, AuthError> {
		match &self.source {
			KeySource::File(path) => {
				let key = Key::from_file(path).map_err(|_| AuthError::KeyNotFound)?;
				Ok(Arc::new(key))
			}
			KeySource::Dir(dir) => {
				let kid = kid.ok_or(AuthError::MissingKeyId)?;
				let kid = KeyId::decode(kid).map_err(|_| AuthError::MissingKeyId)?;
				let path = dir.join(format!("{kid}.jwk"));
				let key = Key::from_file(&path).map_err(|_| AuthError::KeyNotFound)?;
				Ok(Arc::new(key))
			}
			KeySource::Url { url, client } => Self::fetch_key(client, url.clone()).await,
			KeySource::UrlDir { base, client } => {
				let kid = kid.ok_or(AuthError::MissingKeyId)?;
				let kid = KeyId::decode(kid).map_err(|_| AuthError::MissingKeyId)?;
				let url = base.join(&format!("{kid}.jwk")).map_err(|_| AuthError::KeyNotFound)?;
				Self::fetch_key(client, url).await
			}
		}
	}

	async fn fetch_key(client: &ClientWithMiddleware, url: url::Url) -> Result<Arc<Key>, AuthError> {
		let response = client.get(url.clone()).send().await.map_err(|e| {
			tracing::warn!(%url, %e, "failed to fetch key");
			AuthError::KeyNotFound
		})?;

		let response = response.error_for_status().map_err(|e| {
			tracing::warn!(%url, %e, "key endpoint returned error");
			AuthError::KeyNotFound
		})?;

		let body = response.text().await.map_err(|e| {
			tracing::warn!(%url, %e, "failed to read key response body");
			AuthError::KeyNotFound
		})?;

		let key: Key = serde_json::from_str(&body).map_err(|e| {
			tracing::warn!(%url, %e, "failed to parse key JSON");
			AuthError::DecodeFailed
		})?;

		Ok(Arc::new(key))
	}
}

/// Verifies JWT tokens and resolves connection permissions.
///
/// Clone this freely — the underlying state is shared via [`Arc`].
#[derive(Clone)]
pub struct Auth {
	resolver: Option<Arc<KeyResolver>>,
	public: Option<PathOwned>,
}

fn build_http_client() -> anyhow::Result<ClientWithMiddleware> {
	let client = reqwest::Client::builder()
		.timeout(std::time::Duration::from_secs(10))
		.build()
		.context("failed to build HTTP client")?;

	Ok(reqwest_middleware::ClientBuilder::new(client)
		.with(Cache(HttpCache {
			mode: CacheMode::Default,
			manager: MokaManager::default(),
			options: HttpCacheOptions::default(),
		}))
		.build())
}

fn parse_url(s: &str) -> Option<url::Url> {
	let url = url::Url::parse(s).ok()?;
	match url.scheme() {
		"http" | "https" => Some(url),
		_ => None,
	}
}

impl Auth {
	/// Creates a new authenticator from the given configuration.
	pub async fn new(config: AuthConfig) -> anyhow::Result<Self> {
		let public = config.public.map(|p| p.as_path().to_owned());

		anyhow::ensure!(
			config.key.is_none() || config.key_dir.is_none(),
			"cannot specify both --auth-key and --auth-key-dir"
		);

		let source = if let Some(key) = config.key {
			let source = if let Some(url) = parse_url(&key) {
				KeySource::Url {
					url,
					client: build_http_client()?,
				}
			} else {
				let path = PathBuf::from(&key);
				anyhow::ensure!(path.is_file(), "auth-key path is not a file: {key}");
				KeySource::File(path)
			};
			Some(source)
		} else if let Some(key_dir) = config.key_dir {
			let source = if let Some(url) = parse_url(&key_dir) {
				KeySource::UrlDir {
					base: url,
					client: build_http_client()?,
				}
			} else {
				let path = PathBuf::from(&key_dir);
				anyhow::ensure!(path.is_dir(), "auth-key-dir path is not a directory: {key_dir}");
				KeySource::Dir(path)
			};
			Some(source)
		} else {
			None
		};

		let resolver = source.map(|s| Arc::new(KeyResolver::new(s)));

		if resolver.is_none() && public.is_none() {
			anyhow::bail!("no auth-key, auth-key-dir, or public path configured");
		}

		Ok(Self { resolver, public })
	}

	/// Parse the token from the user provided URL, returning the claims if successful.
	/// If no token is provided, then the claims will use the public path if it is set.
	pub async fn verify(&self, params: &AuthParams) -> Result<AuthToken, AuthError> {
		let claims = if let Some(token) = params.jwt.as_deref() {
			let Some(resolver) = &self.resolver else {
				return Err(AuthError::UnexpectedToken);
			};

			// Extract kid from JWT header (may be None for single-key modes)
			let header = jsonwebtoken::decode_header(token).map_err(|_| AuthError::DecodeFailed)?;

			// Resolve the key (kid requirement depends on the source type)
			let key = resolver.resolve(header.kid.as_deref()).await?;

			// Verify the token with the resolved key
			key.decode(token).map_err(|_| AuthError::DecodeFailed)?
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
		let root = Path::new(&params.path);

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

		let register = match (params.register.as_deref(), claims.cluster) {
			(Some(node), true) => Some(node.to_owned()),
			(Some(_), false) => return Err(AuthError::ExpectedCluster),
			_ => None,
		};

		Ok(AuthToken {
			root: root.to_owned(),
			subscribe,
			publish,
			cluster: claims.cluster,
			register,
		})
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use moq_token::{Algorithm, Key};
	use tempfile::TempDir;

	fn create_test_key_with_kid(kid: &str) -> Key {
		Key::generate(Algorithm::HS256, Some(moq_token::KeyId::decode(kid).unwrap())).unwrap()
	}

	fn setup_key_dir(keys: &[(&str, &Key)]) -> TempDir {
		let dir = TempDir::new().unwrap();
		for (kid, key) in keys {
			let path = dir.path().join(format!("{kid}.jwk"));
			key.to_file(&path).unwrap();
		}
		dir
	}

	#[tokio::test]
	async fn test_anonymous_access_with_public_path() -> anyhow::Result<()> {
		let auth = Auth::new(AuthConfig {
			public: Some("anon".to_string()),
			..Default::default()
		})
		.await?;

		let token = auth.verify(&AuthParams::new("/anon")).await?;
		assert_eq!(token.root, "anon".as_path());
		assert_eq!(token.subscribe, vec!["".as_path()]);
		assert_eq!(token.publish, vec!["".as_path()]);

		let token = auth.verify(&AuthParams::new("/anon/room/123")).await?;
		assert_eq!(token.root, Path::new("anon/room/123").to_owned());
		assert_eq!(token.subscribe, vec![Path::new("").to_owned()]);
		assert_eq!(token.publish, vec![Path::new("").to_owned()]);

		Ok(())
	}

	#[tokio::test]
	async fn test_anonymous_access_fully_public() -> anyhow::Result<()> {
		let auth = Auth::new(AuthConfig {
			public: Some("".to_string()),
			..Default::default()
		})
		.await?;

		let token = auth.verify(&AuthParams::new("/any/path")).await?;
		assert_eq!(token.root, Path::new("any/path").to_owned());
		assert_eq!(token.subscribe, vec![Path::new("").to_owned()]);
		assert_eq!(token.publish, vec![Path::new("").to_owned()]);

		Ok(())
	}

	#[tokio::test]
	async fn test_anonymous_access_denied_wrong_prefix() -> anyhow::Result<()> {
		let auth = Auth::new(AuthConfig {
			public: Some("anon".to_string()),
			..Default::default()
		})
		.await?;

		let result = auth.verify(&AuthParams::new("/secret")).await;
		assert!(result.is_err());

		Ok(())
	}

	#[tokio::test]
	async fn test_no_token_no_public_path_fails() -> anyhow::Result<()> {
		let key = create_test_key_with_kid("test-key");
		let dir = setup_key_dir(&[("test-key", &key)]);

		let auth = Auth::new(AuthConfig {
			key_dir: Some(dir.path().to_string_lossy().to_string()),
			..Default::default()
		})
		.await?;

		let result = auth.verify(&AuthParams::new("/any/path")).await;
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

		let result = auth
			.verify(&AuthParams {
				path: "/any/path".into(),
				jwt: Some("fake-token".into()),
				..Default::default()
			})
			.await;
		assert!(result.is_err());

		Ok(())
	}

	#[tokio::test]
	async fn test_jwt_token_basic_validation() -> anyhow::Result<()> {
		let key = create_test_key_with_kid("test-key");
		let dir = setup_key_dir(&[("test-key", &key)]);

		let auth = Auth::new(AuthConfig {
			key_dir: Some(dir.path().to_string_lossy().to_string()),
			..Default::default()
		})
		.await?;

		let claims = moq_token::Claims {
			root: "room/123".to_string(),
			subscribe: vec!["".to_string()],
			publish: vec!["alice".into()],
			..Default::default()
		};
		let token = key.encode(&claims)?;

		let token = auth
			.verify(&AuthParams {
				path: "/room/123".into(),
				jwt: Some(token),
				..Default::default()
			})
			.await?;
		assert_eq!(token.root, "room/123".as_path());
		assert_eq!(token.subscribe, vec!["".as_path()]);
		assert_eq!(token.publish, vec!["alice".as_path()]);

		Ok(())
	}

	#[tokio::test]
	async fn test_jwt_token_wrong_root_path() -> anyhow::Result<()> {
		let key = create_test_key_with_kid("test-key");
		let dir = setup_key_dir(&[("test-key", &key)]);

		let auth = Auth::new(AuthConfig {
			key_dir: Some(dir.path().to_string_lossy().to_string()),
			..Default::default()
		})
		.await?;

		let claims = moq_token::Claims {
			root: "room/123".to_string(),
			subscribe: vec!["".to_string()],
			publish: vec!["".to_string()],
			..Default::default()
		};
		let token = key.encode(&claims)?;

		let result = auth
			.verify(&AuthParams {
				path: "/secret".into(),
				jwt: Some(token),
				..Default::default()
			})
			.await;
		assert!(result.is_err());

		Ok(())
	}

	#[tokio::test]
	async fn test_jwt_token_with_restricted_publish_subscribe() -> anyhow::Result<()> {
		let key = create_test_key_with_kid("test-key");
		let dir = setup_key_dir(&[("test-key", &key)]);

		let auth = Auth::new(AuthConfig {
			key_dir: Some(dir.path().to_string_lossy().to_string()),
			..Default::default()
		})
		.await?;

		let claims = moq_token::Claims {
			root: "room/123".to_string(),
			subscribe: vec!["bob".into()],
			publish: vec!["alice".into()],
			..Default::default()
		};
		let token = key.encode(&claims)?;

		let token = auth
			.verify(&AuthParams {
				path: "/room/123".into(),
				jwt: Some(token),
				..Default::default()
			})
			.await?;
		assert_eq!(token.root, "room/123".as_path());
		assert_eq!(token.subscribe, vec!["bob".as_path()]);
		assert_eq!(token.publish, vec!["alice".as_path()]);

		Ok(())
	}

	#[tokio::test]
	async fn test_jwt_token_read_only() -> anyhow::Result<()> {
		let key = create_test_key_with_kid("test-key");
		let dir = setup_key_dir(&[("test-key", &key)]);

		let auth = Auth::new(AuthConfig {
			key_dir: Some(dir.path().to_string_lossy().to_string()),
			..Default::default()
		})
		.await?;

		let claims = moq_token::Claims {
			root: "room/123".to_string(),
			subscribe: vec!["".to_string()],
			publish: vec![],
			..Default::default()
		};
		let token = key.encode(&claims)?;

		let token = auth
			.verify(&AuthParams {
				path: "/room/123".into(),
				jwt: Some(token),
				..Default::default()
			})
			.await?;
		assert_eq!(token.subscribe, vec!["".as_path()]);
		assert_eq!(token.publish, vec![]);

		Ok(())
	}

	#[tokio::test]
	async fn test_jwt_token_write_only() -> anyhow::Result<()> {
		let key = create_test_key_with_kid("test-key");
		let dir = setup_key_dir(&[("test-key", &key)]);

		let auth = Auth::new(AuthConfig {
			key_dir: Some(dir.path().to_string_lossy().to_string()),
			..Default::default()
		})
		.await?;

		let claims = moq_token::Claims {
			root: "room/123".to_string(),
			subscribe: vec![],
			publish: vec!["bob".into()],
			..Default::default()
		};
		let token = key.encode(&claims)?;

		let token = auth
			.verify(&AuthParams {
				path: "/room/123".into(),
				jwt: Some(token),
				..Default::default()
			})
			.await?;
		assert_eq!(token.subscribe, vec![]);
		assert_eq!(token.publish, vec!["bob".as_path()]);

		Ok(())
	}

	#[tokio::test]
	async fn test_claims_reduction_basic() -> anyhow::Result<()> {
		let key = create_test_key_with_kid("test-key");
		let dir = setup_key_dir(&[("test-key", &key)]);

		let auth = Auth::new(AuthConfig {
			key_dir: Some(dir.path().to_string_lossy().to_string()),
			..Default::default()
		})
		.await?;

		let claims = moq_token::Claims {
			root: "room/123".to_string(),
			subscribe: vec!["".to_string()],
			publish: vec!["".to_string()],
			..Default::default()
		};
		let token = key.encode(&claims)?;

		let token = auth
			.verify(&AuthParams {
				path: "/room/123/alice".into(),
				jwt: Some(token),
				..Default::default()
			})
			.await?;

		assert_eq!(token.root, Path::new("room/123/alice"));
		assert_eq!(token.subscribe, vec!["".as_path()]);
		assert_eq!(token.publish, vec!["".as_path()]);

		Ok(())
	}

	#[tokio::test]
	async fn test_claims_reduction_with_publish_restrictions() -> anyhow::Result<()> {
		let key = create_test_key_with_kid("test-key");
		let dir = setup_key_dir(&[("test-key", &key)]);

		let auth = Auth::new(AuthConfig {
			key_dir: Some(dir.path().to_string_lossy().to_string()),
			..Default::default()
		})
		.await?;

		let claims = moq_token::Claims {
			root: "room/123".to_string(),
			subscribe: vec!["".to_string()],
			publish: vec!["alice".into()],
			..Default::default()
		};
		let token = key.encode(&claims)?;

		let token = auth
			.verify(&AuthParams {
				path: "/room/123/alice".into(),
				jwt: Some(token),
				..Default::default()
			})
			.await?;

		assert_eq!(token.root, "room/123/alice".as_path());
		assert_eq!(token.subscribe, vec!["".as_path()]);
		assert_eq!(token.publish, vec!["".as_path()]);

		Ok(())
	}

	#[tokio::test]
	async fn test_claims_reduction_with_subscribe_restrictions() -> anyhow::Result<()> {
		let key = create_test_key_with_kid("test-key");
		let dir = setup_key_dir(&[("test-key", &key)]);

		let auth = Auth::new(AuthConfig {
			key_dir: Some(dir.path().to_string_lossy().to_string()),
			..Default::default()
		})
		.await?;

		let claims = moq_token::Claims {
			root: "room/123".to_string(),
			subscribe: vec!["bob".into()],
			publish: vec!["".to_string()],
			..Default::default()
		};
		let token = key.encode(&claims)?;

		let token = auth
			.verify(&AuthParams {
				path: "/room/123/bob".into(),
				jwt: Some(token),
				..Default::default()
			})
			.await?;

		assert_eq!(token.root, "room/123/bob".as_path());
		assert_eq!(token.subscribe, vec!["".as_path()]);
		assert_eq!(token.publish, vec!["".as_path()]);

		Ok(())
	}

	#[tokio::test]
	async fn test_claims_reduction_loses_access() -> anyhow::Result<()> {
		let key = create_test_key_with_kid("test-key");
		let dir = setup_key_dir(&[("test-key", &key)]);

		let auth = Auth::new(AuthConfig {
			key_dir: Some(dir.path().to_string_lossy().to_string()),
			..Default::default()
		})
		.await?;

		let claims = moq_token::Claims {
			root: "room/123".to_string(),
			subscribe: vec!["bob".into()],
			publish: vec!["alice".into()],
			..Default::default()
		};
		let token = key.encode(&claims)?;

		let verified = auth
			.verify(&AuthParams {
				path: "/room/123/alice".into(),
				jwt: Some(token.clone()),
				..Default::default()
			})
			.await?;

		assert_eq!(verified.root, "room/123/alice".as_path());
		assert_eq!(verified.subscribe, vec![]);
		assert_eq!(verified.publish, vec!["".as_path()]);

		let verified = auth
			.verify(&AuthParams {
				path: "/room/123/bob".into(),
				jwt: Some(token),
				..Default::default()
			})
			.await?;

		assert_eq!(verified.root, "room/123/bob".as_path());
		assert_eq!(verified.subscribe, vec!["".as_path()]);
		assert_eq!(verified.publish, vec![]);

		Ok(())
	}

	#[tokio::test]
	async fn test_claims_reduction_nested_paths() -> anyhow::Result<()> {
		let key = create_test_key_with_kid("test-key");
		let dir = setup_key_dir(&[("test-key", &key)]);

		let auth = Auth::new(AuthConfig {
			key_dir: Some(dir.path().to_string_lossy().to_string()),
			..Default::default()
		})
		.await?;

		let claims = moq_token::Claims {
			root: "room/123".to_string(),
			subscribe: vec!["users/bob/screen".into()],
			publish: vec!["users/alice/camera".into()],
			..Default::default()
		};
		let token = key.encode(&claims)?;

		let verified = auth
			.verify(&AuthParams {
				path: "/room/123/users".into(),
				jwt: Some(token.clone()),
				..Default::default()
			})
			.await?;

		assert_eq!(verified.root, "room/123/users".as_path());
		assert_eq!(verified.subscribe, vec!["bob/screen".as_path()]);
		assert_eq!(verified.publish, vec!["alice/camera".as_path()]);

		let verified = auth
			.verify(&AuthParams {
				path: "/room/123/users/alice".into(),
				jwt: Some(token),
				..Default::default()
			})
			.await?;

		assert_eq!(verified.root, "room/123/users/alice".as_path());
		assert_eq!(verified.subscribe, vec![]);
		assert_eq!(verified.publish, vec!["camera".as_path()]);

		Ok(())
	}

	#[tokio::test]
	async fn test_claims_reduction_preserves_read_write_only() -> anyhow::Result<()> {
		let key = create_test_key_with_kid("test-key");
		let dir = setup_key_dir(&[("test-key", &key)]);

		let auth = Auth::new(AuthConfig {
			key_dir: Some(dir.path().to_string_lossy().to_string()),
			..Default::default()
		})
		.await?;

		let claims = moq_token::Claims {
			root: "room/123".to_string(),
			subscribe: vec!["alice".into()],
			publish: vec![],
			..Default::default()
		};
		let token = key.encode(&claims)?;

		let verified = auth
			.verify(&AuthParams {
				path: "/room/123/alice".into(),
				jwt: Some(token),
				..Default::default()
			})
			.await?;

		assert_eq!(verified.subscribe, vec!["".as_path()]);
		assert_eq!(verified.publish, vec![]);

		let claims = moq_token::Claims {
			root: "room/123".to_string(),
			subscribe: vec![],
			publish: vec!["alice".into()],
			..Default::default()
		};
		let token = key.encode(&claims)?;

		let verified = auth
			.verify(&AuthParams {
				path: "/room/123/alice".into(),
				jwt: Some(token),
				..Default::default()
			})
			.await?;

		assert_eq!(verified.subscribe, vec![]);
		assert_eq!(verified.publish, vec!["".as_path()]);

		Ok(())
	}

	#[tokio::test]
	async fn test_key_resolver_file_missing_key() -> anyhow::Result<()> {
		let dir = TempDir::new()?;
		let auth = Auth::new(AuthConfig {
			key_dir: Some(dir.path().to_string_lossy().to_string()),
			..Default::default()
		})
		.await?;

		let key = create_test_key_with_kid("nonexistent");
		let claims = moq_token::Claims {
			root: "test".to_string(),
			subscribe: vec!["".to_string()],
			..Default::default()
		};
		let token = key.encode(&claims)?;

		let result = auth
			.verify(&AuthParams {
				path: "/test".into(),
				jwt: Some(token),
				..Default::default()
			})
			.await;
		assert!(matches!(result, Err(AuthError::KeyNotFound)));

		Ok(())
	}

	#[tokio::test]
	async fn test_key_resolver_multiple_keys() -> anyhow::Result<()> {
		let key1 = create_test_key_with_kid("key-1");
		let key2 = create_test_key_with_kid("key-2");
		let dir = setup_key_dir(&[("key-1", &key1), ("key-2", &key2)]);

		let auth = Auth::new(AuthConfig {
			key_dir: Some(dir.path().to_string_lossy().to_string()),
			..Default::default()
		})
		.await?;

		// Sign with key-1
		let claims = moq_token::Claims {
			root: "room/1".to_string(),
			subscribe: vec!["".to_string()],
			..Default::default()
		};
		let token1 = key1.encode(&claims)?;

		let verified = auth
			.verify(&AuthParams {
				path: "/room/1".into(),
				jwt: Some(token1),
				..Default::default()
			})
			.await?;
		assert_eq!(verified.root, "room/1".as_path());

		// Sign with key-2
		let claims = moq_token::Claims {
			root: "room/2".to_string(),
			subscribe: vec!["".to_string()],
			..Default::default()
		};
		let token2 = key2.encode(&claims)?;

		let verified = auth
			.verify(&AuthParams {
				path: "/room/2".into(),
				jwt: Some(token2),
				..Default::default()
			})
			.await?;
		assert_eq!(verified.root, "room/2".as_path());

		Ok(())
	}

	#[tokio::test]
	async fn test_kid_validation() {
		assert!(KeyId::decode("abc-123_DEF").is_ok());
		assert!(KeyId::decode("").is_err());
		assert!(KeyId::decode("../etc/passwd").is_err());
		assert!(KeyId::decode("key with spaces").is_err());
		assert!(KeyId::decode("key/slash").is_err());
	}

	#[tokio::test]
	async fn test_jwt_without_kid_rejected() -> anyhow::Result<()> {
		// Generate a key without a kid
		let key = Key::generate(Algorithm::HS256, None)?;
		let dir = TempDir::new()?;

		let auth = Auth::new(AuthConfig {
			key_dir: Some(dir.path().to_string_lossy().to_string()),
			..Default::default()
		})
		.await?;

		let claims = moq_token::Claims {
			root: "test".to_string(),
			subscribe: vec!["".to_string()],
			..Default::default()
		};
		let token = key.encode(&claims)?;

		let result = auth
			.verify(&AuthParams {
				path: "/test".into(),
				jwt: Some(token),
				..Default::default()
			})
			.await;
		assert!(matches!(result, Err(AuthError::MissingKeyId)));

		Ok(())
	}
}
