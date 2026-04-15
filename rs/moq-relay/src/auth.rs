use anyhow::Context;
use axum::http;
use http_cache_reqwest::{Cache, CacheMode, HttpCache, HttpCacheOptions, MokaManager};
#[cfg(test)]
use moq_lite::AsPath;
use moq_lite::{Path, PathOwned, PathPrefixes};
use moq_token::{Key, KeyId};
use reqwest_middleware::ClientWithMiddleware;
use serde::{Deserialize, Serialize};
use serde_with::{OneOrMany, formats::PreferMany, serde_as};
use std::path::PathBuf;
use std::sync::Arc;
use url::Url;

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
#[non_exhaustive]
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

	#[error("missing key ID in token")]
	MissingKeyId,

	#[error(transparent)]
	InvalidKeyId(#[from] moq_token::KeyIdError),
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

/// TLS configuration for HTTP requests made by the auth client (JWK fetches
/// and public-API lookups).
///
/// Mirrors [`moq_native::ClientTls`] so the auth client can be configured
/// independently of the cluster client. Defaults to system roots with no
/// client identity, which is what most external auth endpoints expect.
#[derive(Clone, Default, Debug, clap::Args, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
#[non_exhaustive]
pub struct AuthTls {
	/// PEM file(s) of root CAs. If empty, the platform's native roots are used.
	#[serde(skip_serializing_if = "Vec::is_empty")]
	#[arg(id = "auth-tls-root", long = "auth-tls-root", env = "MOQ_AUTH_TLS_ROOT")]
	pub root: Vec<PathBuf>,

	/// Present a client certificate during the TLS handshake (mTLS).
	///
	/// Bundled PEM containing both the cert chain and the private key.
	#[serde(skip_serializing_if = "Option::is_none")]
	#[arg(id = "auth-tls-identity", long = "auth-tls-identity", env = "MOQ_AUTH_TLS_IDENTITY")]
	pub identity: Option<PathBuf>,

	/// Danger: Disable TLS certificate verification on auth requests.
	#[serde(skip_serializing_if = "Option::is_none")]
	#[arg(
		id = "auth-tls-disable-verify",
		long = "auth-tls-disable-verify",
		env = "MOQ_AUTH_TLS_DISABLE_VERIFY",
		default_missing_value = "true",
		num_args = 0..=1,
		require_equals = true,
		value_parser = clap::value_parser!(bool),
	)]
	pub disable_verify: Option<bool>,
}

impl AuthTls {
	/// Convert into a [`moq_native::ClientTls`] so we can reuse its
	/// rustls-building logic. The fields map one-to-one.
	fn to_client_tls(&self) -> moq_native::ClientTls {
		let mut tls = moq_native::ClientTls::default();
		tls.root = self.root.clone();
		tls.identity = self.identity.clone();
		tls.disable_verify = self.disable_verify;
		tls
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

	/// TLS configuration for outbound HTTP auth requests (JWK + public-API).
	#[command(flatten)]
	#[serde(default)]
	pub tls: AuthTls,

	/// Public (unauthenticated) access configuration.
	///
	/// CLI: `--auth-public <prefix>` sets both subscribe and publish for the prefix.
	/// TOML: Accepts a string, array, or table `{ subscribe = ..., publish = ... }`.
	/// Any value starting with `http://` or `https://` is treated as a URL endpoint.
	#[arg(long = "auth-public", env = "MOQ_AUTH_PUBLIC")]
	#[serde(default, deserialize_with = "PublicConfig::deserialize_option")]
	pub public: Option<PublicConfig>,

	/// Public (unauthenticated) subscribe access configuration.
	///
	/// CLI-only shorthand: `--auth-public-subscribe <prefix>` sets subscribe-only access.
	/// For TOML, use `[auth.public]` with separate `subscribe`/`publish` fields instead.
	#[arg(long = "auth-public-subscribe", env = "MOQ_AUTH_PUBLIC_SUBSCRIBE")]
	#[serde(skip)]
	pub public_subscribe: Option<PublicConfig>,

	/// Public (unauthenticated) publish access configuration.
	///
	/// CLI-only shorthand: `--auth-public-publish <prefix>` sets publish-only access.
	/// For TOML, use `[auth.public]` with separate `subscribe`/`publish` fields instead.
	#[arg(long = "auth-public-publish", env = "MOQ_AUTH_PUBLIC_PUBLISH")]
	#[serde(skip)]
	pub public_publish: Option<PublicConfig>,

	/// CLI-only shorthand: `--auth-public-api <url>` sets a URL endpoint that returns
	/// `{ subscribe: [...], publish: [...] }` per namespace. The connection namespace is
	/// appended to the URL. For TOML, use `[auth.public]` with an `api` field instead.
	#[arg(long = "auth-public-api", env = "MOQ_AUTH_PUBLIC_API")]
	#[serde(skip)]
	pub public_api: Option<String>,
}

/// Public access configuration.
///
/// TOML examples:
/// - `public = "anon"` → both subscribe and publish under "anon"
/// - `public = ["anon", "demo"]` → both subscribe and publish under both prefixes
/// - `[auth.public]` with `subscribe`/`publish` → separate static control
/// - `[auth.public]` with `api` → dynamic URL endpoint (with optional static fallbacks)
///
/// CLI: `--auth-public <prefix>` creates `Simple(vec![prefix])`.
#[derive(Clone, Debug)]
pub enum PublicConfig {
	/// One or more prefixes granting both subscribe and publish.
	#[deprecated = "Use the detailed config; this is for backwards compatibility only"]
	Simple(Vec<String>),
	/// Separate subscribe/publish prefixes and/or an API URL.
	Detailed(PublicDetailed),
}

/// Detailed public access configuration with separate subscribe/publish and optional API.
#[serde_as]
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PublicDetailed {
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	#[serde_as(as = "OneOrMany<_, PreferMany>")]
	pub subscribe: Vec<String>,
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	#[serde_as(as = "OneOrMany<_, PreferMany>")]
	pub publish: Vec<String>,
	/// A URL endpoint that returns `{ subscribe: [...], publish: [...] }`.
	/// The connection namespace is appended to the URL.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub api: Option<String>,
}

impl PublicConfig {
	/// Normalize into the detailed form.
	pub fn into_detailed(self) -> PublicDetailed {
		match self {
			#[allow(deprecated)]
			PublicConfig::Simple(prefixes) => PublicDetailed {
				subscribe: prefixes.clone(),
				publish: prefixes,
				api: None,
			},
			PublicConfig::Detailed(d) => d,
		}
	}

	/// Deserialize `Option<PublicConfig>` from TOML: dispatches based on value type.
	fn deserialize_option<'de, D>(deserializer: D) -> Result<Option<PublicConfig>, D::Error>
	where
		D: serde::Deserializer<'de>,
	{
		let value = Option::<toml::Value>::deserialize(deserializer)?;
		let Some(value) = value else {
			return Ok(None);
		};

		match value {
			#[allow(deprecated)]
			toml::Value::String(s) => Ok(Some(PublicConfig::Simple(vec![s]))),
			toml::Value::Array(arr) => {
				let strings: Vec<String> = arr
					.into_iter()
					.map(|v| v.try_into::<String>().map_err(serde::de::Error::custom))
					.collect::<Result<_, _>>()?;
				if strings.is_empty() {
					Ok(None)
				} else {
					#[allow(deprecated)]
					Ok(Some(PublicConfig::Simple(strings)))
				}
			}
			toml::Value::Table(table) => {
				let d: PublicDetailed = toml::Value::Table(table).try_into().map_err(serde::de::Error::custom)?;
				if d.subscribe.is_empty() && d.publish.is_empty() && d.api.is_none() {
					Ok(None)
				} else {
					Ok(Some(PublicConfig::Detailed(d)))
				}
			}
			other => Err(serde::de::Error::custom(format!(
				"expected string, array, or table for public config, got {other}"
			))),
		}
	}
}

/// Clap parses `--auth-public <value>` as a string.
impl std::str::FromStr for PublicConfig {
	type Err = std::convert::Infallible;
	fn from_str(s: &str) -> Result<Self, Self::Err> {
		#[allow(deprecated)]
		Ok(PublicConfig::Simple(vec![s.to_string()]))
	}
}

impl Serialize for PublicConfig {
	fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
	where
		S: serde::Serializer,
	{
		match self {
			#[allow(deprecated)]
			PublicConfig::Simple(v) if v.len() == 1 => v[0].serialize(serializer),
			#[allow(deprecated)]
			PublicConfig::Simple(v) => v.serialize(serializer),
			PublicConfig::Detailed(d) => d.serialize(serializer),
		}
	}
}

/// Response from a public access API endpoint.
#[derive(Debug, Deserialize)]
struct PublicResponse {
	#[serde(default)]
	subscribe: Vec<String>,
	#[serde(default)]
	publish: Vec<String>,
}

/// Resolved public access configuration.
#[derive(Clone, Default)]
struct PublicAccess {
	subscribe: PathPrefixes,
	publish: PathPrefixes,
	/// Optional API URL for dynamic resolution (namespace appended).
	api: Option<(url::Url, ClientWithMiddleware)>,
}

impl PublicAccess {
	fn is_empty(&self) -> bool {
		self.subscribe.is_empty() && self.publish.is_empty() && self.api.is_none()
	}

	/// Check if the given path is already fully covered by static prefixes for both directions.
	fn static_covers(&self, path: &Path) -> bool {
		let sub_covered = self.subscribe.iter().any(|p| path.has_prefix(p));
		let pub_covered = self.publish.iter().any(|p| path.has_prefix(p));
		sub_covered && pub_covered
	}
}

impl AuthConfig {
	/// Initializes an [`Auth`] instance from this configuration.
	pub async fn init(self) -> anyhow::Result<Auth> {
		Auth::new(self).await
	}

	/// True when no JWT key, public access rules, or public API are configured.
	///
	/// An empty config is invalid on its own — callers should reject it unless
	/// some other authentication mechanism (e.g. mTLS peer auth) is enabled.
	pub fn is_empty(&self) -> bool {
		self.key.is_none()
			&& self.key_dir.is_none()
			&& self.public.is_none()
			&& self.public_subscribe.is_none()
			&& self.public_publish.is_none()
			&& self.public_api.is_none()
	}
}

/// The result of a successful authentication, containing the resolved
/// permissions for a connection.
#[derive(Debug)]
pub struct AuthToken {
	/// The root path this token is scoped to.
	pub root: PathOwned,
	/// Paths the holder is allowed to subscribe to, relative to `root`.
	pub subscribe: PathPrefixes,
	/// Paths the holder is allowed to publish to, relative to `root`.
	pub publish: PathPrefixes,
	/// Whether this token grants cluster-level privileges.
	pub cluster: bool,
	/// The cluster node name to register, if this is a cluster connection.
	pub register: Option<String>,
}

impl AuthToken {
	/// Construct a token for a peer that was authenticated at the TLS layer
	/// via mTLS. These peers are granted full (root-scoped) publish and
	/// subscribe access plus cluster privileges.
	///
	/// `node` is the peer's cluster node name. It is bound to the cert —
	/// either the first DNS SAN directly, or the SAN with a `:port` suffix
	/// supplied at connect time (DNS SANs cannot carry ports).
	pub fn from_peer(node: String) -> Self {
		Self {
			root: PathOwned::default(),
			subscribe: PathPrefixes::from(vec![Path::new("").to_owned()]),
			publish: PathPrefixes::from(vec![Path::new("").to_owned()]),
			cluster: true,
			register: Some(node),
		}
	}
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
				let key = Key::from_file_async(path).await.map_err(|_| AuthError::KeyNotFound)?;
				Ok(Arc::new(key))
			}
			KeySource::Dir(dir) => {
				let kid = kid.ok_or(AuthError::MissingKeyId)?;
				let kid = KeyId::decode(kid)?;
				let path = dir.join(format!("{kid}.jwk"));
				let key = Key::from_file_async(&path).await.map_err(|_| AuthError::KeyNotFound)?;
				Ok(Arc::new(key))
			}
			KeySource::Url { url, client } => Self::fetch_key(client, url.clone()).await,
			KeySource::UrlDir { base, client } => {
				let kid = kid.ok_or(AuthError::MissingKeyId)?;
				let kid = KeyId::decode(kid)?;
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

		let key = Key::from_str(&body).map_err(|e| {
			tracing::warn!(%url, %e, "failed to parse key");
			AuthError::DecodeFailed
		})?;

		Ok(Arc::new(key))
	}
}

/// Verifies JWT tokens and resolves connection permissions.
///
/// Clone this freely — the underlying state is shared via [`Arc`].
///
/// The default value rejects every JWT/anonymous request — useful as a
/// no-op stub when authentication is delegated entirely to mTLS peer certs.
#[derive(Clone, Default)]
pub struct Auth {
	resolver: Option<Arc<KeyResolver>>,
	/// Public (unauthenticated) access with static prefixes and/or an API.
	public: PublicAccess,
}

impl Auth {
	pub async fn new(config: AuthConfig) -> anyhow::Result<Self> {
		anyhow::ensure!(
			config.key.is_none() || config.key_dir.is_none(),
			"cannot specify both --auth-key and --auth-key-dir"
		);

		let tls = config.tls.to_client_tls().build()?;

		let source = if let Some(key) = config.key {
			let source = if let Ok(url) = Url::parse(&key) {
				KeySource::Url {
					url,
					client: Self::build_client(&tls)?,
				}
			} else {
				let path = PathBuf::from(&key);
				anyhow::ensure!(path.is_file(), "auth-key path is not a file: {key}");
				KeySource::File(path)
			};
			Some(source)
		} else if let Some(key_dir) = config.key_dir {
			let source = if let Ok(mut url) = Url::parse(&key_dir) {
				// Ensure trailing slash so Url::join appends rather than replaces the last segment
				if !url.path().ends_with('/') {
					url.set_path(&format!("{}/", url.path()));
				}
				KeySource::UrlDir {
					base: url,
					client: Self::build_client(&tls)?,
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

		// Resolve public access by merging all three config sources.
		let mut subscribe = Vec::new();
		let mut publish = Vec::new();
		let mut api = None;

		if let Some(config) = config.public {
			let d = config.into_detailed();
			subscribe.extend(d.subscribe.iter().map(|s| Path::new(s).to_owned()));
			publish.extend(d.publish.iter().map(|s| Path::new(s).to_owned()));
			if let Some(url_str) = d.api {
				let mut url = Url::parse(&url_str).context("invalid public API URL")?;
				if !url.path().ends_with('/') {
					url.set_path(&format!("{}/", url.path()));
				}
				api = Some((url, Self::build_client(&tls)?));
			}
		}

		if let Some(config) = config.public_subscribe {
			let d = config.into_detailed();
			subscribe.extend(d.subscribe.iter().map(|s| Path::new(s).to_owned()));
		}

		if let Some(config) = config.public_publish {
			let d = config.into_detailed();
			publish.extend(d.publish.iter().map(|s| Path::new(s).to_owned()));
		}

		if let Some(url_str) = config.public_api {
			anyhow::ensure!(
				api.is_none(),
				"cannot specify --auth-public-api alongside [auth.public] api"
			);
			let mut url = Url::parse(&url_str).context("invalid --auth-public-api URL")?;
			if !url.path().ends_with('/') {
				url.set_path(&format!("{}/", url.path()));
			}
			api = Some((url, Self::build_client(&tls)?));
		}

		let public = PublicAccess {
			subscribe: PathPrefixes::from(subscribe),
			publish: PathPrefixes::from(publish),
			api,
		};

		if resolver.is_none() && public.is_empty() {
			anyhow::bail!("no auth-key, auth-key-dir, or public path configured");
		}

		Ok(Self { resolver, public })
	}

	async fn fetch_public_response(client: &ClientWithMiddleware, url: &url::Url) -> Result<PublicResponse, AuthError> {
		let response = client.get(url.clone()).send().await.map_err(|e| {
			tracing::warn!(%url, %e, "failed to fetch public access");
			AuthError::ExpectedToken
		})?;

		let response = response.error_for_status().map_err(|_| AuthError::ExpectedToken)?;

		let body = response.text().await.map_err(|e| {
			tracing::warn!(%url, %e, "failed to read public access response");
			AuthError::ExpectedToken
		})?;

		serde_json::from_str(&body).map_err(|e| {
			tracing::warn!(%url, %e, "failed to parse public access response");
			AuthError::DecodeFailed
		})
	}

	/// Parse the token from the user provided URL, returning the claims if successful.
	/// If no token is provided, then the claims will use the public access configuration.
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
		} else if !self.public.is_empty() {
			// No JWT — use public access (static prefixes + optional API).
			let root = Path::new(&params.path);
			let mut subscribe: Vec<String> = self.public.subscribe.iter().map(|p| p.to_string()).collect();
			let mut publish: Vec<String> = self.public.publish.iter().map(|p| p.to_string()).collect();

			// If an API is configured and static prefixes don't already cover this path,
			// fetch additional permissions for this namespace.
			if let Some((base, client)) = &self.public.api {
				if !self.public.static_covers(&root) {
					let namespace = root.to_string();
					match base.join(&namespace) {
						Ok(url) => match Self::fetch_public_response(client, &url).await {
							Ok(response) => {
								subscribe.extend(response.subscribe);
								publish.extend(response.publish);
							}
							Err(e) => {
								tracing::debug!(%url, %e, "public access API denied or failed");
							}
						},
						Err(e) => {
							tracing::warn!(%base, %e, "failed to construct public access URL");
						}
					}
				}
			}

			if subscribe.is_empty() && publish.is_empty() {
				return Err(AuthError::ExpectedToken);
			}

			moq_token::Claims {
				root: "".to_string(),
				subscribe,
				publish,
				..Default::default()
			}
		} else {
			return Err(AuthError::ExpectedToken);
		};

		// Get the path from the URL, removing any leading or trailing slashes.
		let root = Path::new(&params.path);
		let claims_root = Path::new(&claims.root);

		// The URL path and the token root must overlap:
		// - URL extends root (e.g. URL="/demo/room", root="demo") → suffix narrows permissions
		// - URL is parent of root (e.g. URL="/", root="demo") → prefix widens permission paths
		let (suffix, prefix) = if let Some(suffix) = root.strip_prefix(&claims_root) {
			(suffix, Path::new(""))
		} else if let Some(prefix) = claims_root.strip_prefix(&root) {
			(Path::new(""), prefix)
		} else {
			return Err(AuthError::IncorrectRoot);
		};

		let scope = |paths: Vec<String>| -> PathPrefixes {
			paths
				.into_iter()
				.filter_map(|p| {
					let p = prefix.join(&p);
					if p.is_empty() {
						return Some(p);
					}
					if let Some(remaining) = p.strip_prefix(&suffix) {
						Some(remaining.into_owned())
					} else if suffix.has_prefix(&p) {
						Some(Path::new("").into_owned())
					} else {
						None
					}
				})
				.collect()
		};

		let subscribe = scope(claims.subscribe);
		let publish = scope(claims.publish);

		let register = match (params.register.as_deref(), claims.cluster) {
			(Some(node), true) => Some(node.to_owned()),
			(Some(_), false) => return Err(AuthError::ExpectedCluster),
			_ => None,
		};

		// Reject connections that end up with no permissions after reduction
		if subscribe.is_empty() && publish.is_empty() && !claims.cluster {
			return Err(AuthError::IncorrectRoot);
		}

		Ok(AuthToken {
			root: root.to_owned(),
			subscribe,
			publish,
			cluster: claims.cluster,
			register,
		})
	}

	fn build_client(tls: &rustls::ClientConfig) -> anyhow::Result<ClientWithMiddleware> {
		let client = reqwest::Client::builder()
			.timeout(std::time::Duration::from_secs(10))
			.use_preconfigured_tls(tls.clone())
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
}

#[cfg(test)]
mod tests {
	use super::*;
	use moq_token::{Algorithm, Key, KeyId};
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

	fn simple_public(prefix: &str) -> Option<PublicConfig> {
		#[allow(deprecated)]
		Some(PublicConfig::Simple(vec![prefix.to_string()]))
	}

	fn detailed_public(subscribe: &[&str], publish: &[&str]) -> Option<PublicConfig> {
		Some(PublicConfig::Detailed(PublicDetailed {
			subscribe: subscribe.iter().map(|s| s.to_string()).collect(),
			publish: publish.iter().map(|s| s.to_string()).collect(),
			api: None,
		}))
	}

	#[tokio::test]
	async fn test_anonymous_access_with_public_path() -> anyhow::Result<()> {
		let auth = Auth::new(AuthConfig {
			public: simple_public("anon"),
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
			public: simple_public(""),
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
			public: simple_public("anon"),
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
			public: simple_public("anon"),
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
	async fn test_public_subscribe_only() -> anyhow::Result<()> {
		let auth = Auth::new(AuthConfig {
			public: detailed_public(&["demo"], &[]),
			..Default::default()
		})
		.await?;

		// Anonymous access to / — can subscribe under demo/
		let token = auth.verify(&AuthParams::new("/")).await?;
		assert_eq!(token.root, "".as_path());
		assert_eq!(token.subscribe, vec!["demo".as_path()]);
		assert_eq!(token.publish, vec![]);

		// Anonymous access to /demo — subscribe reduces to ""
		let token = auth.verify(&AuthParams::new("/demo")).await?;
		assert_eq!(token.root, "demo".as_path());
		assert_eq!(token.subscribe, vec!["".as_path()]);
		assert_eq!(token.publish, vec![]);

		// Anonymous access to /demo/room/123 — still allowed (subpath of public prefix)
		let token = auth.verify(&AuthParams::new("/demo/room/123")).await?;
		assert_eq!(token.root, Path::new("demo/room/123").to_owned());
		assert_eq!(token.subscribe, vec!["".as_path()]);
		assert_eq!(token.publish, vec![]);

		// Anonymous access to /other — should fail (not under public prefix)
		let result = auth.verify(&AuthParams::new("/other")).await;
		assert!(result.is_err());

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
	async fn test_public_publish_only() -> anyhow::Result<()> {
		let auth = Auth::new(AuthConfig {
			public: detailed_public(&[], &["demo"]),
			..Default::default()
		})
		.await?;

		// Anonymous access to / — can publish under demo/
		let token = auth.verify(&AuthParams::new("/")).await?;
		assert_eq!(token.subscribe, vec![]);
		assert_eq!(token.publish, vec!["demo".as_path()]);

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

	#[tokio::test]
	async fn test_public_detailed_both() -> anyhow::Result<()> {
		let auth = Auth::new(AuthConfig {
			public: detailed_public(&["demo"], &["demo"]),
			..Default::default()
		})
		.await?;

		let token = auth.verify(&AuthParams::new("/")).await?;
		assert_eq!(token.subscribe, vec!["demo".as_path()]);
		assert_eq!(token.publish, vec!["demo".as_path()]);

		// Connecting to /demo reduces both to ""
		let token = auth.verify(&AuthParams::new("/demo")).await?;
		assert_eq!(token.subscribe, vec!["".as_path()]);
		assert_eq!(token.publish, vec!["".as_path()]);

		Ok(())
	}

	#[tokio::test]
	async fn test_public_empty_string_allows_everything() -> anyhow::Result<()> {
		let auth = Auth::new(AuthConfig {
			public: simple_public(""),
			..Default::default()
		})
		.await?;

		// Anonymous access to any path gets full pub/sub
		let token = auth.verify(&AuthParams::new("/anything/here")).await?;
		assert_eq!(token.subscribe, vec!["".as_path()]);
		assert_eq!(token.publish, vec!["".as_path()]);

		Ok(())
	}

	#[tokio::test]
	async fn test_public_with_jwt_still_works() -> anyhow::Result<()> {
		let key = create_test_key_with_kid("test-key");
		let key_file = tempfile::NamedTempFile::new()?;
		key.to_file(key_file.path())?;

		let auth = Auth::new(AuthConfig {
			key: Some(key_file.path().to_string_lossy().to_string()),
			public: detailed_public(&["demo"], &[]),
			..Default::default()
		})
		.await?;

		// JWT tokens should still work normally
		let claims = moq_token::Claims {
			root: "secret".to_string(),
			subscribe: vec!["".to_string()],
			publish: vec!["alice".into()],
			..Default::default()
		};
		let jwt = key.encode(&claims)?;

		let token = auth
			.verify(&AuthParams {
				path: "/secret".into(),
				jwt: Some(jwt),
				..Default::default()
			})
			.await?;
		assert_eq!(token.root, "secret".as_path());
		assert_eq!(token.subscribe, vec!["".as_path()]);
		assert_eq!(token.publish, vec!["alice".as_path()]);

		Ok(())
	}

	#[tokio::test]
	async fn test_jwt_connect_to_parent_of_root() -> anyhow::Result<()> {
		let key = create_test_key_with_kid("test-key");
		let dir = setup_key_dir(&[("test-key", &key)]);

		let auth = Auth::new(AuthConfig {
			key_dir: Some(dir.path().to_string_lossy().to_string()),
			..Default::default()
		})
		.await?;

		// Token with root="demo", connecting to "/"
		let claims = moq_token::Claims {
			root: "demo".to_string(),
			subscribe: vec!["".to_string()],
			publish: vec!["alice".into()],
			..Default::default()
		};
		let token = key.encode(&claims)?;

		let verified = auth
			.verify(&AuthParams {
				path: "/".into(),
				jwt: Some(token),
				..Default::default()
			})
			.await?;

		// Root is "/" (empty), permissions are prefixed with "demo"
		assert_eq!(verified.root, "".as_path());
		assert_eq!(verified.subscribe, vec!["demo".as_path()]);
		assert_eq!(verified.publish, vec!["demo/alice".as_path()]);

		Ok(())
	}

	#[tokio::test]
	async fn test_jwt_connect_to_partial_parent_of_root() -> anyhow::Result<()> {
		let key = create_test_key_with_kid("test-key");
		let dir = setup_key_dir(&[("test-key", &key)]);

		let auth = Auth::new(AuthConfig {
			key_dir: Some(dir.path().to_string_lossy().to_string()),
			..Default::default()
		})
		.await?;

		// Token with root="room/123", connecting to "/room"
		let claims = moq_token::Claims {
			root: "room/123".to_string(),
			subscribe: vec!["".to_string()],
			publish: vec!["alice".into()],
			..Default::default()
		};
		let token = key.encode(&claims)?;

		let verified = auth
			.verify(&AuthParams {
				path: "/room".into(),
				jwt: Some(token),
				..Default::default()
			})
			.await?;

		// Permissions are prefixed with the remaining "123"
		assert_eq!(verified.root, "room".as_path());
		assert_eq!(verified.subscribe, vec!["123".as_path()]);
		assert_eq!(verified.publish, vec!["123/alice".as_path()]);

		Ok(())
	}

	#[tokio::test]
	async fn test_jwt_connect_to_unrelated_path_rejected() -> anyhow::Result<()> {
		let key = create_test_key_with_kid("test-key");
		let dir = setup_key_dir(&[("test-key", &key)]);

		let auth = Auth::new(AuthConfig {
			key_dir: Some(dir.path().to_string_lossy().to_string()),
			..Default::default()
		})
		.await?;

		// Token with root="demo", connecting to "/other"
		let claims = moq_token::Claims {
			root: "demo".to_string(),
			subscribe: vec!["".to_string()],
			publish: vec!["".to_string()],
			..Default::default()
		};
		let token = key.encode(&claims)?;

		let result = auth
			.verify(&AuthParams {
				path: "/other".into(),
				jwt: Some(token),
				..Default::default()
			})
			.await;
		assert!(matches!(result, Err(AuthError::IncorrectRoot)));

		Ok(())
	}

	#[tokio::test]
	async fn test_jwt_root_empty_subscribe_scoped_rejects_unrelated() -> anyhow::Result<()> {
		let key = create_test_key_with_kid("test-key");
		let dir = setup_key_dir(&[("test-key", &key)]);

		let auth = Auth::new(AuthConfig {
			key_dir: Some(dir.path().to_string_lossy().to_string()),
			..Default::default()
		})
		.await?;

		// Token with root="", subscribe=["demo"] — only demo/ is accessible
		let claims = moq_token::Claims {
			root: "".to_string(),
			subscribe: vec!["demo".to_string()],
			publish: vec![],
			..Default::default()
		};
		let token = key.encode(&claims)?;

		// Connecting to /other should fail — no permissions remain after filtering
		let result = auth
			.verify(&AuthParams {
				path: "/other".into(),
				jwt: Some(token),
				..Default::default()
			})
			.await;
		assert!(matches!(result, Err(AuthError::IncorrectRoot)));

		Ok(())
	}

	#[test]
	fn test_toml_public_string() {
		let config: AuthConfig = toml::from_str(r#"public = "anon""#).unwrap();
		let d = config.public.unwrap().into_detailed();
		assert_eq!(d.subscribe, vec!["anon"]);
		assert_eq!(d.publish, vec!["anon"]);
	}

	#[test]
	fn test_toml_public_empty_string() {
		let config: AuthConfig = toml::from_str(r#"public = """#).unwrap();
		let d = config.public.unwrap().into_detailed();
		assert_eq!(d.subscribe, vec![""]);
		assert_eq!(d.publish, vec![""]);
	}

	#[test]
	fn test_toml_public_array() {
		let config: AuthConfig = toml::from_str(r#"public = ["anon", "demo"]"#).unwrap();
		let d = config.public.unwrap().into_detailed();
		assert_eq!(d.subscribe, vec!["anon", "demo"]);
		assert_eq!(d.publish, vec!["anon", "demo"]);
	}

	#[test]
	fn test_toml_public_table_both() {
		let config: AuthConfig = toml::from_str(
			r#"[public]
subscribe = "demo"
publish = "anon"
"#,
		)
		.unwrap();
		let d = config.public.unwrap().into_detailed();
		assert_eq!(d.subscribe, vec!["demo"]);
		assert_eq!(d.publish, vec!["anon"]);
	}

	#[test]
	fn test_toml_public_table_arrays() {
		let config: AuthConfig = toml::from_str(
			r#"[public]
subscribe = ["anon", "demo"]
publish = ["anon"]
"#,
		)
		.unwrap();
		let d = config.public.unwrap().into_detailed();
		assert_eq!(d.subscribe, vec!["anon", "demo"]);
		assert_eq!(d.publish, vec!["anon"]);
	}

	#[test]
	fn test_toml_public_table_subscribe_only() {
		let config: AuthConfig = toml::from_str(
			r#"[public]
subscribe = "demo"
"#,
		)
		.unwrap();
		let d = config.public.unwrap().into_detailed();
		assert_eq!(d.subscribe, vec!["demo"]);
		assert!(d.publish.is_empty());
	}

	#[test]
	fn test_toml_public_table_publish_only() {
		let config: AuthConfig = toml::from_str(
			r#"[public]
publish = ["anon", "demo"]
"#,
		)
		.unwrap();
		let d = config.public.unwrap().into_detailed();
		assert!(d.subscribe.is_empty());
		assert_eq!(d.publish, vec!["anon", "demo"]);
	}

	#[test]
	fn test_toml_public_not_set() {
		let config: AuthConfig = toml::from_str("").unwrap();
		assert!(config.public.is_none());
	}

	#[test]
	fn test_toml_public_url_string() {
		let config: AuthConfig = toml::from_str(r#"public = "https://api.example.com/access""#).unwrap();
		let d = config.public.unwrap().into_detailed();
		assert_eq!(d.subscribe, vec!["https://api.example.com/access"]);
		assert_eq!(d.publish, vec!["https://api.example.com/access"]);
	}

	#[test]
	fn test_toml_public_table_api() {
		let config: AuthConfig = toml::from_str(
			r#"[public]
api = "https://api.example.com/access"
"#,
		)
		.unwrap();
		let d = config.public.unwrap().into_detailed();
		assert_eq!(d.api.as_deref(), Some("https://api.example.com/access"));
		assert!(d.subscribe.is_empty());
		assert!(d.publish.is_empty());
	}

	#[test]
	fn test_toml_public_table_api_with_static() {
		let config: AuthConfig = toml::from_str(
			r#"[public]
subscribe = "anon"
publish = "anon"
api = "https://api.example.com/access"
"#,
		)
		.unwrap();
		let d = config.public.unwrap().into_detailed();
		assert_eq!(d.subscribe, vec!["anon"]);
		assert_eq!(d.publish, vec!["anon"]);
		assert_eq!(d.api.as_deref(), Some("https://api.example.com/access"));
	}

	#[test]
	fn test_clap_public_from_str() {
		let config: PublicConfig = "anon".parse().unwrap();
		let d = config.into_detailed();
		assert_eq!(d.subscribe, vec!["anon"]);
		assert_eq!(d.publish, vec!["anon"]);
	}

	#[test]
	fn test_clap_public_url_from_str() {
		let config: PublicConfig = "https://api.example.com/access".parse().unwrap();
		let d = config.into_detailed();
		assert_eq!(d.subscribe, vec!["https://api.example.com/access"]);
		assert_eq!(d.publish, vec!["https://api.example.com/access"]);
	}

	#[tokio::test]
	async fn test_public_subscribe_flag_merged() -> anyhow::Result<()> {
		// Simulates: --auth-public anon --auth-public-subscribe demo
		let auth = Auth::new(AuthConfig {
			public: simple_public("anon"),
			public_subscribe: simple_public("demo"),
			..Default::default()
		})
		.await?;

		// /anon gets full pub+sub from --auth-public
		let token = auth.verify(&AuthParams::new("/anon")).await?;
		assert_eq!(token.subscribe, vec!["".as_path()]);
		assert_eq!(token.publish, vec!["".as_path()]);

		// /demo gets subscribe-only from --auth-public-subscribe
		let token = auth.verify(&AuthParams::new("/demo")).await?;
		assert_eq!(token.subscribe, vec!["".as_path()]);
		assert_eq!(token.publish, vec![]);

		// /secret gets nothing
		let result = auth.verify(&AuthParams::new("/secret")).await;
		assert!(result.is_err());

		Ok(())
	}

	#[tokio::test]
	async fn test_public_publish_flag_merged() -> anyhow::Result<()> {
		// Simulates: --auth-public anon --auth-public-publish uploads
		let auth = Auth::new(AuthConfig {
			public: simple_public("anon"),
			public_publish: simple_public("uploads"),
			..Default::default()
		})
		.await?;

		// /uploads gets publish-only from --auth-public-publish
		let token = auth.verify(&AuthParams::new("/uploads")).await?;
		assert_eq!(token.subscribe, vec![]);
		assert_eq!(token.publish, vec!["".as_path()]);

		Ok(())
	}
}
