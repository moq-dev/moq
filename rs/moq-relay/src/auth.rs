use anyhow::Context;
use axum::http;
#[cfg(test)]
use moq_net::AsPath;
use moq_net::{Path, PathOwned, PathPrefixes, Tier};
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
}

impl AuthParams {
	/// Creates params with just a path and no token.
	pub fn new(path: impl Into<String>) -> Self {
		Self {
			path: path.into(),
			..Default::default()
		}
	}

	/// Extracts authentication parameters from a URL's path and query string.
	///
	/// When the URL host matches one of `domains` as `<labels>.<suffix>`, the
	/// labels are prepended to the URL path in DNS-reverse order (broadest
	/// scope first), so `team.customer.cdn.moq.dev/foo` with suffix
	/// `cdn.moq.dev` routes to `/customer/team/foo`. An exact-suffix or
	/// non-matching host is left as-is (plain path-based routing).
	///
	/// `domains` must be pre-canonicalized by [`Auth::new`] (lowercased and
	/// prefixed with `.`).
	pub(crate) fn from_url(url: &url::Url, domains: &[String]) -> Self {
		// url.path() always starts with '/' for http/https/wss URLs.
		let path = match match_domain(url.host_str(), domains) {
			Some(slug) => format!("/{slug}{}", url.path()),
			None => url.path().to_string(),
		};

		let mut jwt = None;

		for (k, v) in url.query_pairs() {
			if v.is_empty() {
				continue;
			}
			if k.as_ref() == "jwt" {
				jwt = Some(v.into_owned());
			}
		}

		Self { path, jwt }
	}
}

/// If `host` matches any configured suffix as `<labels>.<suffix>`, returns
/// the labels joined with `/` in DNS-reverse order so the broadest scope
/// becomes the outermost path segment. With suffix `cdn.moq.dev`:
///
/// - `customer.cdn.moq.dev`      → `Some("customer")`
/// - `team.customer.cdn.moq.dev` → `Some("customer/team")`
///
/// An exact match against a suffix or a host that matches no suffix returns
/// `None` (plain path-based routing).
///
/// `domains` must be pre-validated, ASCII-lowercased, and `.`-prefixed (e.g.
/// `".cdn.moq.dev"`); [`Auth::new`] does this once at startup so a single
/// `strip_suffix` covers both exact (slug = `""`) and slug match.
fn match_domain(host: Option<&str>, domains: &[String]) -> Option<String> {
	let host = host?;
	// Most relays don't configure --auth-domain; skip the lowercase alloc
	// when there's nothing to match against.
	if domains.is_empty() {
		return None;
	}
	// Pre-pend '.' to the host so the dot-prefixed suffixes match exact and
	// slug hosts identically.
	let host_lc = format!(".{}", host.to_ascii_lowercase());
	for suffix in domains {
		if let Some(slug) = host_lc.strip_suffix(suffix) {
			if slug.is_empty() {
				return None;
			}
			// Drop the leading '.' left by strip_suffix, then reverse the
			// labels and join with '/' — DNS nests broader scopes rightward,
			// so reversing puts the broadest label first in the path.
			return Some(slug.trim_start_matches('.').rsplit('.').collect::<Vec<_>>().join("/"));
		}
	}
	None
}

/// Errors returned when authentication or authorization fails.
#[derive(thiserror::Error, Debug)]
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

	#[error("key not found")]
	KeyNotFound,

	#[error("missing key ID in token")]
	MissingKeyId,

	#[error("auth API request failed: {0}")]
	ApiUnavailable(#[from] reqwest_middleware::Error),

	#[error("auth API response was invalid: {0}")]
	ApiInvalidResponse(#[from] serde_json::Error),

	#[error("invalid URL: {0}")]
	InvalidUrl(#[from] url::ParseError),

	#[error(transparent)]
	InvalidKeyId(#[from] moq_token::KeyIdError),
}

// reqwest::Error → AuthError flows through reqwest_middleware::Error so callers can use `?`
// on both .send() (returns reqwest_middleware::Error) and .error_for_status() / .text()
// (return reqwest::Error) without a manual map_err.
impl From<reqwest::Error> for AuthError {
	fn from(e: reqwest::Error) -> Self {
		Self::ApiUnavailable(e.into())
	}
}

impl From<&AuthError> for http::StatusCode {
	fn from(err: &AuthError) -> Self {
		match err {
			// Upstream auth API unreachable or misconfigured — this is a server-side
			// problem, not a credential problem.
			AuthError::ApiUnavailable(_) | AuthError::ApiInvalidResponse(_) => http::StatusCode::BAD_GATEWAY,
			AuthError::InvalidUrl(_) => http::StatusCode::INTERNAL_SERVER_ERROR,
			_ => http::StatusCode::UNAUTHORIZED,
		}
	}
}

impl From<AuthError> for http::StatusCode {
	fn from(err: AuthError) -> Self {
		Self::from(&err)
	}
}

impl axum::response::IntoResponse for AuthError {
	fn into_response(self) -> axum::response::Response {
		http::StatusCode::from(self).into_response()
	}
}

/// TLS configuration for HTTP requests made by the auth client (JWK fetches
/// and public-API lookups).
///
/// Mirrors [`moq_native::tls::Client`] so the auth client can be configured
/// independently of the cluster client. Defaults to system roots with no
/// client identity, which is what most external auth endpoints expect.
#[serde_as]
#[derive(Clone, Default, Debug, clap::Args, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
#[non_exhaustive]
pub struct AuthTls {
	/// PEM file(s) of root CAs. If empty, the platform's native roots are used.
	/// In config files, accepts either a single string or a TOML array.
	#[serde(skip_serializing_if = "Vec::is_empty")]
	#[arg(id = "auth-tls-root", long = "auth-tls-root", env = "MOQ_AUTH_TLS_ROOT")]
	#[serde_as(as = "OneOrMany<_>")]
	pub root: Vec<PathBuf>,

	/// PEM file containing the client certificate chain for mTLS.
	#[serde(skip_serializing_if = "Option::is_none")]
	#[arg(id = "auth-tls-cert", long = "auth-tls-cert", env = "MOQ_AUTH_TLS_CERT")]
	pub cert: Option<PathBuf>,

	/// PEM file containing the private key for mTLS.
	#[serde(skip_serializing_if = "Option::is_none")]
	#[arg(id = "auth-tls-key", long = "auth-tls-key", env = "MOQ_AUTH_TLS_KEY")]
	pub key: Option<PathBuf>,

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
	/// Convert into a [`moq_native::tls::Client`] so we can reuse its
	/// rustls-building logic. The fields map one-to-one.
	fn to_client_tls(&self) -> anyhow::Result<moq_native::tls::Client> {
		match (&self.cert, &self.key) {
			(Some(_), None) => anyhow::bail!("--auth-tls-cert requires --auth-tls-key"),
			(None, Some(_)) => anyhow::bail!("--auth-tls-key requires --auth-tls-cert"),
			_ => {}
		}

		let mut tls = moq_native::tls::Client::default();
		tls.root = self.root.clone();
		tls.cert = self.cert.clone();
		tls.key = self.key.clone();
		tls.disable_verify = self.disable_verify;
		Ok(tls)
	}
}

/// Configuration for JWT-based authentication.
#[serde_as]
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
	///
	/// DEPRECATED (URL form): prefer the unified `--auth-api`, which resolves the
	/// key in the same call as public access and the alias. The file-directory
	/// form remains supported for standalone relays.
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
	///
	/// DEPRECATED: prefer the unified `--auth-api`, which returns public access in
	/// the same call as the key and alias.
	#[arg(long = "auth-public-api", env = "MOQ_AUTH_PUBLIC_API")]
	#[serde(skip)]
	pub public_api: Option<String>,

	/// Domain suffixes for subdomain-based (SNI) slug routing.
	///
	/// When an incoming connection's URL host is `<labels>.<suffix>` for one
	/// of these suffixes, the labels are reversed and prepended to the URL
	/// path before auth runs (DNS nests broader scopes rightward, so
	/// reversing puts the broadest label first in the path). With suffix
	/// `cdn.moq.dev`:
	///
	/// - `customer.cdn.moq.dev/foo`      → `cdn.moq.dev/customer/foo`
	/// - `team.customer.cdn.moq.dev/foo` → `cdn.moq.dev/customer/team/foo`
	///
	/// A host that exactly matches a suffix contributes no slug. Hosts that
	/// don't match any suffix fall back to plain path-based routing.
	///
	/// Pass `--auth-domain` multiple times to configure more than one suffix
	/// — useful for serving multiple regions or product domains from one
	/// relay. Overlapping suffixes are resolved longest-first. For example,
	/// with `["cdn.moq.dev", "usw.cdn.moq.dev"]`, `customer.usw.cdn.moq.dev`
	/// matches the more specific `usw.cdn.moq.dev` (slug `customer`,
	/// path `/customer/foo`) rather than `cdn.moq.dev` (slug `usw/customer`,
	/// path `/usw/customer/foo`).
	///
	/// In config files, accepts either a single string or a TOML array.
	#[arg(long = "auth-domain", env = "MOQ_AUTH_DOMAIN")]
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	#[serde_as(as = "OneOrMany<_>")]
	pub domains: Vec<String>,

	/// Base URL of a unified auth API that resolves everything the relay needs to
	/// authorize a connection in ONE call, replacing per-call `--auth-key-dir`
	/// (URL form) + `--auth-public-api`.
	///
	/// Mutually exclusive with `--auth-key`, `--auth-key-dir`, `--auth-public`,
	/// and `--auth-public-api` (configuring both is a startup error).
	/// `--auth-domain` still applies (subdomain->path runs first).
	///
	/// Per connection the relay issues `GET <base>?root=<path>&kid=<kid>&mtls=true`
	/// over the same cached, mTLS-gated HTTP client used by the other auth fetches.
	/// `root` is the connection path (slashes preserved); `kid` is sent only when
	/// the connection carries a JWT (value from its header); `mtls=true` is sent
	/// only when the peer presented a verified client cert. All three are query
	/// params (never path segments), so the base URL is used verbatim. The
	/// response is a JSON object whose fields are ALL optional:
	///
	/// - `alias`: the canonical full root to scope this connection to (the path
	///   with its first segment resolved to the project's stable id, the rest
	///   preserved, e.g. `demo/room/cam` -> `x7k2qp/room/cam`). Used verbatim;
	///   the server controls the whole mapping. Absent -> the request path is
	///   used unchanged.
	/// - `public`: `{ "subscribe": [...], "publish": [...] }` anonymous access
	///   prefixes, relative to the root, used when there is no JWT. Absent ->
	///   no public access.
	/// - `key`: the verifying JWK (a JSON object, deserialized directly) for the
	///   requested `kid`. Absent -> key-not-found (the JWT is rejected).
	/// - `tier`: the billing tier label (e.g. `internal`, `region/sjc`). The relay
	///   forwards `mtls=true` and lets the API decide. Absent defaults per
	///   connection: `internal` for mTLS peers (trusted), the default (unprefixed)
	///   tier for JWT/public. So the API can bucket a first-party token to
	///   `internal`, or a cert-verified connection back to the default tier. An
	///   empty label selects the default tier. The legacy `internal: bool` field
	///   is still accepted (`true` -> `internal`, `false` -> default tier) when
	///   `tier` is absent.
	///
	/// FAILS CLOSED: any network error, non-2xx status, or parse error rejects
	/// the connection. Unlike the standalone flags, the verifying key itself
	/// comes from this call, so there is no safe fallback; the response cache
	/// (`Cache-Control` from the endpoint) softens transient failures.
	///
	/// Example: `https://api.moq.dev/cluster/auth` (called as
	/// `?root=demo/room&kid=abc&mtls=true`).
	#[arg(long = "auth-api", env = "MOQ_AUTH_API")]
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub auth_api: Option<String>,

	/// Default billing tier label for mTLS peers when the auth API doesn't
	/// return one (or no `--auth-api` is configured). Default `internal`. An
	/// empty value selects the default (unprefixed) tier.
	#[arg(long = "auth-mtls-tier", env = "MOQ_AUTH_MTLS_TIER")]
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub mtls_tier: Option<String>,
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

/// Response from a public access API endpoint, and the `public` field of the
/// unified [`AuthApiResponse`].
#[derive(Debug, Default, Deserialize)]
struct PublicResponse {
	#[serde(default)]
	subscribe: Vec<String>,
	#[serde(default)]
	publish: Vec<String>,
}

/// Response from the unified `--auth-api` endpoint. Every field is optional; the
/// relay defaults anything absent (see [`AuthConfig::auth_api`]).
#[derive(Debug, Default, Deserialize)]
struct AuthApiResponse {
	/// Canonical full root to scope to; absent -> use the request path as-is.
	#[serde(default)]
	alias: Option<String>,
	/// Anonymous access prefixes; absent -> no public access.
	#[serde(default)]
	public: Option<PublicResponse>,
	/// Verifying JWK for the requested kid (deserialized directly via
	/// moq-token's serde); absent -> not found.
	#[serde(default)]
	key: Option<Key>,
	/// Billing tier label for this connection (e.g. `internal`, `region/sjc`).
	/// The relay sends `mtls=true` when the peer presented a verified client
	/// cert and lets the API decide. Absent defaults per path: `internal` for
	/// mTLS peers (trusted), the default (unprefixed) tier otherwise. An empty
	/// label is the default tier.
	#[serde(default)]
	tier: Option<String>,
	/// Legacy boolean form of `tier`, kept for backward compatibility with auth
	/// APIs written against the original contract. `true` maps to the `internal`
	/// tier, `false` to the default (unprefixed) tier. Ignored when `tier` is
	/// also present.
	#[serde(default)]
	internal: Option<bool>,
}

impl AuthApiResponse {
	/// Billing tier this response selects, honoring the legacy `internal: bool`
	/// field when the newer `tier` label is absent. `None` leaves the choice to
	/// the relay's per-connection default.
	fn tier(&self) -> Option<Tier> {
		if let Some(label) = &self.tier {
			return Some(Tier::new(label.clone()));
		}
		self.internal.map(|internal| {
			if internal {
				Tier::new("internal")
			} else {
				Tier::default()
			}
		})
	}
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
			&& self.auth_api.is_none()
	}
}

/// The result of a successful authentication, containing the resolved
/// permissions for a connection.
///
/// Marked `#[non_exhaustive]` so additional context fields (cluster tier flags,
/// rate-limit info, etc.) can be added without bumping the major version.
/// External consumers must build tokens through library APIs (e.g. via
/// [`Auth::verify`]) rather than by struct literal.
#[derive(Debug)]
#[non_exhaustive]
pub struct AuthToken {
	/// The root path this token is scoped to.
	pub root: PathOwned,
	/// Paths the holder is allowed to subscribe to, relative to `root`.
	pub subscribe: PathPrefixes,
	/// Paths the holder is allowed to publish to, relative to `root`.
	pub publish: PathPrefixes,
	/// Billing tier this session's stats record under. Chosen by business logic
	/// (the auth API's `tier` field), defaulting to `internal` for trusted mTLS
	/// peers and the default tier otherwise, so cluster peers can be billed
	/// separately from end-user traffic.
	pub tier: Tier,
	/// When the credential backing this session expires, if it has an expiry.
	///
	/// For JWT auth this is the token's `exp` claim; for mTLS it's the peer
	/// certificate's `notAfter`. The relay closes the session once this passes
	/// instead of trusting a credential that was only checked at connect time.
	pub expires: Option<std::time::SystemTime>,
}

impl AuthToken {
	/// Construct a token for a peer that was authenticated at the TLS layer
	/// via mTLS. These peers are granted full publish and subscribe access
	/// within `root`. The billing tier is left at the default; the caller (mTLS
	/// handshake, internal listener, or cluster dial) sets it from config. The cert's trust chain
	/// (verified against the configured CA) is the only credential we require;
	/// nothing else in the cert is inspected.
	///
	/// `root` is the API-resolved canonical root for the connection URL path, the
	/// same scoping a JWT gets. An mTLS publisher dialing `/demo` therefore
	/// announces under its canonical root, not the cluster root. Cluster peers
	/// dial `/`, which typically resolves to an empty root and keeps unscoped
	/// access.
	pub fn unrestricted(root: PathOwned) -> Self {
		Self {
			root,
			subscribe: PathPrefixes::from(vec![Path::new("").to_owned()]),
			publish: PathPrefixes::from(vec![Path::new("").to_owned()]),
			tier: Tier::default(),
			// Filled in by the caller from the peer certificate's notAfter.
			expires: None,
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
		let response = client.get(url).send().await?;

		if response.status() == http::StatusCode::NOT_FOUND {
			return Err(AuthError::KeyNotFound);
		}

		let body = response.error_for_status()?.text().await?;
		let key = Key::from_str(&body).map_err(|_| AuthError::DecodeFailed)?;
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
	/// Domain suffixes for subdomain-based slug routing. See [`AuthConfig::domains`].
	domains: Arc<[String]>,
	/// Optional unified auth API: one call per connection resolves the key,
	/// public access, and alias together. Mutually exclusive with the standalone
	/// key/public sources. See [`AuthConfig::auth_api`].
	auth_api: Option<(url::Url, ClientWithMiddleware)>,
	/// Billing tier recorded for an mTLS peer when the auth API doesn't return a
	/// tier (or none is configured). See [`AuthConfig::mtls_tier`]; default
	/// `internal`, set via [`Auth::with_mtls_tier`].
	mtls_tier: Tier,
}

impl Auth {
	pub async fn new(config: AuthConfig) -> anyhow::Result<Self> {
		anyhow::ensure!(
			config.key.is_none() || config.key_dir.is_none(),
			"cannot specify both --auth-key and --auth-key-dir"
		);

		// The unified --auth-api supplies key + public + alias itself, so it
		// can't be combined with the standalone key/public sources.
		anyhow::ensure!(
			config.auth_api.is_none()
				|| (config.key.is_none()
					&& config.key_dir.is_none()
					&& config.public.is_none()
					&& config.public_subscribe.is_none()
					&& config.public_publish.is_none()
					&& config.public_api.is_none()),
			"--auth-api cannot be combined with --auth-key/--auth-key-dir/--auth-public/--auth-public-api"
		);

		let tls = config.tls.to_client_tls()?.build()?;

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
				tracing::warn!("--auth-key-dir with a URL is deprecated; prefer the unified --auth-api");
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
			tracing::warn!("--auth-public-api is deprecated; prefer the unified --auth-api");
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

		if resolver.is_none() && public.is_empty() && config.auth_api.is_none() {
			anyhow::bail!("no auth-key, auth-key-dir, auth-api, or public path configured");
		}

		// Canonicalize domain suffixes once at startup: lowercase and prefix
		// with '.' so `match_domain` can do plain dot-prefixed strip_suffix
		// per request without per-call allocations. Sort longest-first so
		// overlapping configurations (e.g. ["moq.dev", "cdn.moq.dev"]) match
		// the most specific suffix rather than letting the configured order
		// silently decide.
		let mut domains: Vec<String> = Vec::with_capacity(config.domains.len());
		for d in config.domains {
			let d = d.trim_start_matches('.').to_ascii_lowercase();
			anyhow::ensure!(!d.is_empty(), "auth-domain suffix must not be empty");
			domains.push(format!(".{d}"));
		}
		domains.sort_by_key(|d| std::cmp::Reverse(d.len()));

		// The connection path, kid, and mtls flag all go in the query string, so
		// the base URL is used verbatim (no trailing-slash / path-append handling).
		let auth_api = if let Some(url_str) = config.auth_api {
			let url = Url::parse(&url_str).context("invalid --auth-api URL")?;
			Some((url, Self::build_client(&tls)?))
		} else {
			None
		};

		Ok(Self {
			resolver,
			public,
			domains: Arc::from(domains.into_boxed_slice()),
			auth_api,
			mtls_tier: crate::trusted_tier(config.mtls_tier),
		})
	}

	/// Override the mTLS fallback billing tier (default `internal`). For the
	/// mTLS-only stub built via [`Auth::default`], where there is no
	/// [`AuthConfig`] to carry `--auth-mtls-tier`. An empty label selects the
	/// default (unprefixed) tier.
	pub fn with_mtls_tier(mut self, tier: Option<String>) -> Self {
		self.mtls_tier = crate::trusted_tier(tier);
		self
	}

	/// Build [`AuthParams`] from an incoming connection URL, applying any
	/// configured subdomain-based slug routing.
	pub(crate) fn params_from_url(&self, url: &url::Url) -> AuthParams {
		AuthParams::from_url(url, &self.domains)
	}

	/// Resolve the canonical root and billing tier for an mTLS peer via the
	/// unified `--auth-api`. mTLS peers are already trusted (the cert is the
	/// credential), so this only fetches the alias + tier.
	///
	/// Fails OPEN only when there is no auth API configured: the cert is the
	/// credential and there is nothing to resolve, so the path is used unchanged
	/// at the internal tier. Otherwise the API is the source of truth for every
	/// connection, including the root (`/`), so it can alias and tier root peers
	/// too. An API error therefore FAILS CLOSED (returns `Err`) rather than
	/// accepting the connection with the path unresolved. Accepting it would route
	/// the broadcast to the literal vanity path (e.g. `demo`) instead of its
	/// canonical root (e.g. `x7k2qp`), producing a zombie session: the publisher
	/// believes it is connected and never reconnects, but nothing is ever served.
	/// Failing closed lets the client retry and self-heal once the API recovers.
	pub(crate) async fn resolve_mtls(&self, path: &str) -> Result<(String, Tier), AuthError> {
		let Some((base, client)) = &self.auth_api else {
			return Ok((path.to_string(), self.mtls_tier.clone()));
		};

		let resp = Self::fetch_auth_api(client, base, path, None, true).await?;
		// Fall back to the configured mTLS tier when the API omits one.
		let tier = resp.tier().unwrap_or_else(|| self.mtls_tier.clone());
		Ok((resp.alias.unwrap_or_else(|| path.to_string()), tier))
	}

	/// Build the unified auth-API request URL. The connection path (`root`), the
	/// JWT `kid`, and the `mtls` flag are all query params on the base URL — never
	/// path segments — so client-controlled values are percent-encoded by
	/// `query_pairs_mut` and can't retarget the path/query.
	fn auth_api_url(base: &url::Url, path: &str, kid: Option<&str>, mtls: bool) -> url::Url {
		let mut url = base.clone();
		{
			let mut q = url.query_pairs_mut();
			q.append_pair("root", path.trim_matches('/'));
			if let Some(kid) = kid {
				q.append_pair("kid", kid);
			}
			if mtls {
				q.append_pair("mtls", "true");
			}
		}
		url
	}

	/// One unified auth-API call. Fails CLOSED (any network / non-2xx / parse
	/// error is an `Err`): with `--auth-api` the verifying key comes from here,
	/// so there is no safe fallback.
	async fn fetch_auth_api(
		client: &ClientWithMiddleware,
		base: &url::Url,
		path: &str,
		kid: Option<&str>,
		mtls: bool,
	) -> Result<AuthApiResponse, AuthError> {
		let url = Self::auth_api_url(base, path, kid, mtls);
		let body = client.get(url).send().await?.error_for_status()?.text().await?;
		serde_json::from_str(&body).map_err(AuthError::from)
	}

	/// Verify a connection via the unified `--auth-api`: one call returns the
	/// alias (root), public access, and verifying key.
	async fn verify_via_api(
		&self,
		base: &url::Url,
		client: &ClientWithMiddleware,
		params: &AuthParams,
	) -> Result<AuthToken, AuthError> {
		// A JWT's kid selects the verifying key; extract it (no kid -> the API
		// returns no key -> we reject below).
		let kid = match params.jwt.as_deref() {
			Some(token) => {
				jsonwebtoken::decode_header(token)
					.map_err(|_| AuthError::DecodeFailed)?
					.kid
			}
			None => None,
		};

		let resp = Self::fetch_auth_api(client, base, &params.path, kid.as_deref(), false).await?;
		// Resolve the tier before consuming `resp`'s other fields below.
		// Non-mTLS connections default to the unprefixed tier; the API may bucket
		// specific ones (e.g. a first-party dashboard token to `internal`).
		let tier = resp.tier().unwrap_or_default();
		// Absent alias -> use the request path unchanged.
		let root = resp.alias.unwrap_or_else(|| params.path.clone());

		let claims = if let Some(token) = params.jwt.as_deref() {
			let key = resp.key.ok_or(AuthError::KeyNotFound)?;
			key.decode(token).map_err(|_| AuthError::DecodeFailed)?
		} else {
			let public = resp.public.unwrap_or_default();
			if public.subscribe.is_empty() && public.publish.is_empty() {
				return Err(AuthError::ExpectedToken);
			}
			// Public prefixes are relative to the connection root, so anchor the
			// claims there (mirrors the standalone --auth-public-api path).
			moq_token::Claims {
				root: root.clone(),
				subscribe: public.subscribe,
				publish: public.publish,
				..Default::default()
			}
		};

		let mut token = Self::finalize(&root, claims)?;
		token.tier = tier;
		Ok(token)
	}

	async fn fetch_public_response(client: &ClientWithMiddleware, url: &url::Url) -> Result<PublicResponse, AuthError> {
		let body = client.get(url.clone()).send().await?.error_for_status()?.text().await?;
		serde_json::from_str(&body).map_err(AuthError::from)
	}

	/// Parse the token from the user provided URL, returning the claims if successful.
	/// If no token is provided, then the claims will use the public access configuration.
	#[allow(deprecated)] // `claims.cluster` is deprecated but still accepted for backwards compat
	pub async fn verify(&self, params: &AuthParams) -> Result<AuthToken, AuthError> {
		// The unified API resolves key + public + alias in one call.
		if let Some((base, client)) = &self.auth_api {
			return self.verify_via_api(base, client, params).await;
		}

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

			// Use static config if any static prefix overlaps the request path in either
			// direction (request is under a public prefix, or request is a parent of one).
			let overlaps = |p: &Path| root.has_prefix(p) || p.has_prefix(&root);
			if self.public.subscribe.iter().any(&overlaps) || self.public.publish.iter().any(overlaps) {
				moq_token::Claims {
					root: "".to_string(),
					subscribe: self.public.subscribe.iter().map(|p| p.to_string()).collect(),
					publish: self.public.publish.iter().map(|p| p.to_string()).collect(),
					..Default::default()
				}
			} else if let Some((base, client)) = &self.public.api {
				// No static overlap — fetch from API. Response paths are relative to the namespace.
				let namespace = root.to_string();
				let url = base.join(&namespace)?;
				let response = Self::fetch_public_response(client, &url).await?;
				moq_token::Claims {
					root: namespace,
					subscribe: response.subscribe,
					publish: response.publish,
					..Default::default()
				}
			} else {
				return Err(AuthError::ExpectedToken);
			}
		} else {
			return Err(AuthError::ExpectedToken);
		};

		Self::finalize(&params.path, claims)
	}

	/// Reduce verified `claims` against the connection `root_str` into an
	/// [`AuthToken`]. The connection path and the token root must overlap; the
	/// permission prefixes are re-based onto the connection root and any that
	/// fall outside it are dropped. Shared by the standalone and `--auth-api`
	/// paths.
	fn finalize(root_str: &str, claims: moq_token::Claims) -> Result<AuthToken, AuthError> {
		let root = Path::new(root_str);
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

		// Reject connections that end up with no permissions after reduction.
		if subscribe.is_empty() && publish.is_empty() {
			return Err(AuthError::IncorrectRoot);
		}

		Ok(AuthToken {
			root: root.to_owned(),
			subscribe,
			publish,
			tier: Tier::default(),
			expires: claims.expires,
		})
	}

	fn build_client(tls: &rustls::ClientConfig) -> anyhow::Result<ClientWithMiddleware> {
		crate::http_client::build(tls)
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
			})
			.await?;
		assert_eq!(token.root, "room/123".as_path());
		assert_eq!(token.subscribe, vec!["".as_path()]);
		assert_eq!(token.publish, vec!["alice".as_path()]);

		Ok(())
	}

	#[tokio::test]
	async fn test_jwt_expiry_carried_through() -> anyhow::Result<()> {
		let key = create_test_key_with_kid("test-key");
		let dir = setup_key_dir(&[("test-key", &key)]);

		let auth = Auth::new(AuthConfig {
			key_dir: Some(dir.path().to_string_lossy().to_string()),
			..Default::default()
		})
		.await?;

		// JWT `exp` has second granularity, so use a whole-second expiry to avoid
		// rounding ambiguity on the round-trip.
		let want = std::time::SystemTime::now()
			.duration_since(std::time::UNIX_EPOCH)?
			.as_secs()
			+ 3600;
		let expires = std::time::UNIX_EPOCH + std::time::Duration::from_secs(want);
		let claims = moq_token::Claims {
			root: "room/123".to_string(),
			subscribe: vec!["".to_string()],
			publish: vec!["alice".into()],
			expires: Some(expires),
			..Default::default()
		};
		let token = key.encode(&claims)?;

		let token = auth
			.verify(&AuthParams {
				path: "/room/123".into(),
				jwt: Some(token),
			})
			.await?;

		// The `exp` claim survives finalize() so the relay can close on expiry.
		let got = token.expires.expect("expiry should be carried through");
		assert_eq!(got.duration_since(std::time::UNIX_EPOCH)?.as_secs(), want);

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
			})
			.await?;

		assert_eq!(token.root, Path::new("room/123/alice"));
		assert_eq!(token.subscribe, vec!["".as_path()]);
		assert_eq!(token.publish, vec!["".as_path()]);

		Ok(())
	}

	#[tokio::test]
	async fn test_claims_reduction_with_publisher_restrictions() -> anyhow::Result<()> {
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
			})
			.await?;

		assert_eq!(verified.root, "room/123/alice".as_path());
		assert_eq!(verified.subscribe, vec![]);
		assert_eq!(verified.publish, vec!["".as_path()]);

		let verified = auth
			.verify(&AuthParams {
				path: "/room/123/bob".into(),
				jwt: Some(token),
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
			})
			.await?;

		assert_eq!(verified.root, "room/123/users".as_path());
		assert_eq!(verified.subscribe, vec!["bob/screen".as_path()]);
		assert_eq!(verified.publish, vec!["alice/camera".as_path()]);

		let verified = auth
			.verify(&AuthParams {
				path: "/room/123/users/alice".into(),
				jwt: Some(token),
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

	// ---------------------------------------------------------------------
	// HTTP-based tests (URL key-dir + public API) using wiremock.
	// ---------------------------------------------------------------------

	use wiremock::matchers::{method, path as path_matcher, query_param};
	use wiremock::{Mock, MockServer, ResponseTemplate};

	/// Serialize a key as JSON for serving from a mock URL endpoint.
	fn jwk_body(key: &Key) -> String {
		serde_json::to_string(key).unwrap()
	}

	/// Build an Auth wired to a wiremock server's `/keys/` URL key-dir.
	async fn auth_with_url_key_dir(server: &MockServer) -> Auth {
		Auth::new(AuthConfig {
			key_dir: Some(format!("{}/keys/", server.uri())),
			..Default::default()
		})
		.await
		.unwrap()
	}

	/// Build an Auth wired to a wiremock server's `/public/` URL with optional static prefixes.
	async fn auth_with_public_api(server: &MockServer, static_subscribe: &[&str], static_publish: &[&str]) -> Auth {
		Auth::new(AuthConfig {
			public: Some(PublicConfig::Detailed(PublicDetailed {
				subscribe: static_subscribe.iter().map(|s| s.to_string()).collect(),
				publish: static_publish.iter().map(|s| s.to_string()).collect(),
				api: Some(format!("{}/public/", server.uri())),
			})),
			..Default::default()
		})
		.await
		.unwrap()
	}

	#[tokio::test]
	async fn test_url_key_resolves_via_http() -> anyhow::Result<()> {
		let server = MockServer::start().await;
		let key = create_test_key_with_kid("test-key");

		Mock::given(method("GET"))
			.and(path_matcher("/keys/test-key.jwk"))
			.respond_with(ResponseTemplate::new(200).set_body_string(jwk_body(&key)))
			.mount(&server)
			.await;

		let auth = auth_with_url_key_dir(&server).await;

		let claims = moq_token::Claims {
			root: "room/1".to_string(),
			subscribe: vec!["".to_string()],
			..Default::default()
		};
		let token = key.encode(&claims)?;

		let verified = auth
			.verify(&AuthParams {
				path: "/room/1".into(),
				jwt: Some(token),
			})
			.await?;
		assert_eq!(verified.root, "room/1".as_path());
		Ok(())
	}

	#[tokio::test]
	async fn test_url_key_dir_404_returns_key_not_found() -> anyhow::Result<()> {
		let server = MockServer::start().await;
		let key = create_test_key_with_kid("missing");

		Mock::given(method("GET"))
			.respond_with(ResponseTemplate::new(404))
			.mount(&server)
			.await;

		let auth = auth_with_url_key_dir(&server).await;

		let claims = moq_token::Claims {
			root: "room/1".to_string(),
			subscribe: vec!["".to_string()],
			..Default::default()
		};
		let token = key.encode(&claims)?;
		let result = auth
			.verify(&AuthParams {
				path: "/room/1".into(),
				jwt: Some(token),
			})
			.await;
		assert!(matches!(result, Err(AuthError::KeyNotFound)));
		Ok(())
	}

	#[tokio::test]
	async fn test_url_key_dir_500_returns_api_unavailable() -> anyhow::Result<()> {
		let server = MockServer::start().await;
		let key = create_test_key_with_kid("test-key");

		Mock::given(method("GET"))
			.respond_with(ResponseTemplate::new(500))
			.mount(&server)
			.await;

		let auth = auth_with_url_key_dir(&server).await;

		let claims = moq_token::Claims {
			root: "room/1".to_string(),
			subscribe: vec!["".to_string()],
			..Default::default()
		};
		let token = key.encode(&claims)?;
		let result = auth
			.verify(&AuthParams {
				path: "/room/1".into(),
				jwt: Some(token),
			})
			.await;
		assert!(matches!(result, Err(AuthError::ApiUnavailable(_))));
		Ok(())
	}

	#[tokio::test]
	async fn test_url_key_dir_network_error_returns_api_unavailable() -> anyhow::Result<()> {
		// Unreachable port — TCP connect refused.
		let auth = Auth::new(AuthConfig {
			key_dir: Some("http://127.0.0.1:1/keys/".to_string()),
			..Default::default()
		})
		.await?;

		let key = create_test_key_with_kid("test-key");
		let claims = moq_token::Claims {
			root: "room/1".to_string(),
			subscribe: vec!["".to_string()],
			..Default::default()
		};
		let token = key.encode(&claims)?;
		let result = auth
			.verify(&AuthParams {
				path: "/room/1".into(),
				jwt: Some(token),
			})
			.await;
		assert!(matches!(result, Err(AuthError::ApiUnavailable(_))));
		Ok(())
	}

	#[tokio::test]
	async fn test_url_key_dir_invalid_body_returns_decode_failed() -> anyhow::Result<()> {
		let server = MockServer::start().await;
		let key = create_test_key_with_kid("test-key");

		Mock::given(method("GET"))
			.and(path_matcher("/keys/test-key.jwk"))
			.respond_with(ResponseTemplate::new(200).set_body_string("not a jwk"))
			.mount(&server)
			.await;

		let auth = auth_with_url_key_dir(&server).await;

		let claims = moq_token::Claims {
			root: "room/1".to_string(),
			subscribe: vec!["".to_string()],
			..Default::default()
		};
		let token = key.encode(&claims)?;
		let result = auth
			.verify(&AuthParams {
				path: "/room/1".into(),
				jwt: Some(token),
			})
			.await;
		assert!(matches!(result, Err(AuthError::DecodeFailed)));
		Ok(())
	}

	#[tokio::test]
	async fn test_url_key_caching_dedups_requests() -> anyhow::Result<()> {
		let server = MockServer::start().await;
		let key = create_test_key_with_kid("test-key");

		// expect(1): the cache should serve the second request from memory.
		Mock::given(method("GET"))
			.and(path_matcher("/keys/test-key.jwk"))
			.respond_with(
				ResponseTemplate::new(200)
					.insert_header("Cache-Control", "public, max-age=300")
					.set_body_string(jwk_body(&key)),
			)
			.expect(1)
			.mount(&server)
			.await;

		let auth = auth_with_url_key_dir(&server).await;

		let claims = moq_token::Claims {
			root: "room/1".to_string(),
			subscribe: vec!["".to_string()],
			..Default::default()
		};
		let token = key.encode(&claims)?;

		for _ in 0..2 {
			auth.verify(&AuthParams {
				path: "/room/1".into(),
				jwt: Some(token.clone()),
			})
			.await?;
		}
		// Mock::expect(1) is asserted on drop of the server.
		Ok(())
	}

	// ---------------------------------------------------------------------
	// Public-access API tests
	// ---------------------------------------------------------------------

	#[tokio::test]
	async fn test_public_api_returns_relative_paths() -> anyhow::Result<()> {
		let server = MockServer::start().await;

		Mock::given(method("GET"))
			.and(path_matcher("/public/foo"))
			.respond_with(ResponseTemplate::new(200).set_body_string(r#"{"subscribe":[""],"publish":[""]}"#))
			.mount(&server)
			.await;

		let auth = auth_with_public_api(&server, &[], &[]).await;
		let token = auth.verify(&AuthParams::new("/foo")).await?;
		assert_eq!(token.root, "foo".as_path());
		assert_eq!(token.subscribe, vec!["".as_path()]);
		assert_eq!(token.publish, vec!["".as_path()]);
		Ok(())
	}

	#[tokio::test]
	async fn test_public_api_with_subpath_prefixes() -> anyhow::Result<()> {
		let server = MockServer::start().await;

		Mock::given(method("GET"))
			.and(path_matcher("/public/demo"))
			.respond_with(ResponseTemplate::new(200).set_body_string(r#"{"subscribe":["viewer"],"publish":[]}"#))
			.mount(&server)
			.await;

		let auth = auth_with_public_api(&server, &[], &[]).await;
		let token = auth.verify(&AuthParams::new("/demo")).await?;
		assert_eq!(token.root, "demo".as_path());
		assert_eq!(token.subscribe, vec!["viewer".as_path()]);
		assert!(token.publish.is_empty());
		Ok(())
	}

	#[tokio::test]
	async fn test_public_api_skipped_when_static_overlaps() -> anyhow::Result<()> {
		let server = MockServer::start().await;

		// expect(0): the static prefix already covers /demo, so the API must NOT be called.
		Mock::given(method("GET"))
			.respond_with(ResponseTemplate::new(500))
			.expect(0)
			.mount(&server)
			.await;

		let auth = auth_with_public_api(&server, &["demo"], &[]).await;
		let token = auth.verify(&AuthParams::new("/demo")).await?;
		assert_eq!(token.subscribe, vec!["".as_path()]);
		Ok(())
	}

	#[tokio::test]
	async fn test_public_api_called_when_no_static_overlap() -> anyhow::Result<()> {
		let server = MockServer::start().await;

		// expect(1): the static prefix "other" doesn't overlap with /demo, so the API IS called.
		Mock::given(method("GET"))
			.and(path_matcher("/public/demo"))
			.respond_with(ResponseTemplate::new(200).set_body_string(r#"{"subscribe":[""],"publish":[]}"#))
			.expect(1)
			.mount(&server)
			.await;

		let auth = auth_with_public_api(&server, &["other"], &[]).await;
		auth.verify(&AuthParams::new("/demo")).await?;
		Ok(())
	}

	#[tokio::test]
	async fn test_public_api_skipped_for_parent_of_static_prefix() -> anyhow::Result<()> {
		let server = MockServer::start().await;

		// Static "demo" overlaps with connection root "/" via the bidirectional check
		// (p.has_prefix(&root) where p="demo", root=""). API must NOT be called.
		Mock::given(method("GET"))
			.respond_with(ResponseTemplate::new(500))
			.expect(0)
			.mount(&server)
			.await;

		let auth = auth_with_public_api(&server, &["demo"], &[]).await;
		let token = auth.verify(&AuthParams::new("/")).await?;
		// Connecting to root with static "demo" → subscribe scoped under demo/.
		assert_eq!(token.subscribe, vec!["demo".as_path()]);
		Ok(())
	}

	#[tokio::test]
	async fn test_public_api_unreachable_returns_api_unavailable() -> anyhow::Result<()> {
		let auth = Auth::new(AuthConfig {
			public: Some(PublicConfig::Detailed(PublicDetailed {
				subscribe: vec![],
				publish: vec![],
				api: Some("http://127.0.0.1:1/public/".to_string()),
			})),
			..Default::default()
		})
		.await?;

		let result = auth.verify(&AuthParams::new("/demo")).await;
		assert!(matches!(result, Err(AuthError::ApiUnavailable(_))));
		Ok(())
	}

	#[tokio::test]
	async fn test_public_api_404_returns_api_unavailable() -> anyhow::Result<()> {
		let server = MockServer::start().await;
		Mock::given(method("GET"))
			.respond_with(ResponseTemplate::new(404))
			.mount(&server)
			.await;

		let auth = auth_with_public_api(&server, &[], &[]).await;
		let result = auth.verify(&AuthParams::new("/demo")).await;
		assert!(matches!(result, Err(AuthError::ApiUnavailable(_))));
		Ok(())
	}

	#[tokio::test]
	async fn test_public_api_invalid_json_returns_invalid_response() -> anyhow::Result<()> {
		// Malformed upstream JSON is an upstream failure (502), not a bad-credential
		// (401): the auth API answered, but with garbage.
		let server = MockServer::start().await;
		Mock::given(method("GET"))
			.respond_with(ResponseTemplate::new(200).set_body_string("not json"))
			.mount(&server)
			.await;

		let auth = auth_with_public_api(&server, &[], &[]).await;
		let result = auth.verify(&AuthParams::new("/demo")).await;
		assert!(matches!(result, Err(AuthError::ApiInvalidResponse(_))));
		assert_eq!(
			http::StatusCode::from(result.unwrap_err()),
			http::StatusCode::BAD_GATEWAY
		);
		Ok(())
	}

	// ---------------------------------------------------------------------
	// mTLS test: stand up a real HTTPS server requiring + verifying client
	// certs, and assert that --auth-tls-cert/--auth-tls-key present the cert.
	// ---------------------------------------------------------------------

	use rcgen::{CertificateParams, KeyPair};
	use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
	use rustls::server::WebPkiClientVerifier;
	use std::sync::Arc as StdArc;

	struct MtlsFixture {
		_dir: TempDir,
		ca_pem_path: PathBuf,
		client_cert_path: PathBuf,
		client_key_path: PathBuf,
		base_url: String,
		key: Key,
	}

	/// Spin up an HTTPS server on 127.0.0.1 that requires a client cert signed
	/// by our test CA and serves `/keys/test-key.jwk`. Returns paths to the CA
	/// PEM and the client cert/key files so callers can configure `Auth`.
	async fn mtls_fixture() -> MtlsFixture {
		// Install a default crypto provider for rustls. Idempotent across tests.
		let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

		// 1. Generate a CA.
		let ca_kp = KeyPair::generate().unwrap();
		let mut ca_params = CertificateParams::new(vec![]).unwrap();
		ca_params.distinguished_name.push(rcgen::DnType::CommonName, "Test CA");
		ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
		let ca_cert = ca_params.self_signed(&ca_kp).unwrap();
		let ca_issuer = rcgen::Issuer::from_params(&ca_params, &ca_kp);

		// 2. Server cert (SAN: 127.0.0.1) signed by the CA.
		let server_kp = KeyPair::generate().unwrap();
		let mut server_params = CertificateParams::new(vec!["127.0.0.1".to_string()]).unwrap();
		server_params
			.distinguished_name
			.push(rcgen::DnType::CommonName, "test-server");
		let server_cert = server_params.signed_by(&server_kp, &ca_issuer).unwrap();

		// 3. Client cert signed by the CA.
		let client_kp = KeyPair::generate().unwrap();
		let mut client_params = CertificateParams::new(vec![]).unwrap();
		client_params
			.distinguished_name
			.push(rcgen::DnType::CommonName, "test-client");
		let client_cert = client_params.signed_by(&client_kp, &ca_issuer).unwrap();

		// 4. Write CA + client cert/key to temp files.
		let dir = TempDir::new().unwrap();
		let ca_pem_path = dir.path().join("ca.pem");
		let client_cert_path = dir.path().join("client.cert.pem");
		let client_key_path = dir.path().join("client.key.pem");
		std::fs::write(&ca_pem_path, ca_cert.pem()).unwrap();
		std::fs::write(&client_cert_path, client_cert.pem()).unwrap();
		std::fs::write(&client_key_path, client_kp.serialize_pem()).unwrap();

		// 5. Build a rustls ServerConfig requiring + verifying client certs against the CA.
		let mut roots = rustls::RootCertStore::empty();
		roots.add(CertificateDer::from(ca_cert.der().to_vec())).unwrap();
		let verifier = WebPkiClientVerifier::builder(StdArc::new(roots)).build().unwrap();
		let server_cert_der = CertificateDer::from(server_cert.der().to_vec());
		let server_key_der = PrivatePkcs8KeyDer::from(server_kp.serialize_der());
		let server_config = rustls::ServerConfig::builder()
			.with_client_cert_verifier(verifier)
			.with_single_cert(vec![server_cert_der], PrivateKeyDer::Pkcs8(server_key_der))
			.unwrap();

		// 6. Spawn an axum server on a random port.
		let key = create_test_key_with_kid("test-key");
		let body = jwk_body(&key);
		let app = axum::Router::new().route("/keys/test-key.jwk", axum::routing::get(move || async move { body }));
		let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
		listener.set_nonblocking(true).unwrap();
		let addr = listener.local_addr().unwrap();
		let tls_config = axum_server::tls_rustls::RustlsConfig::from_config(StdArc::new(server_config));
		let handle = axum_server::Handle::new();
		let serve_handle = handle.clone();
		tokio::spawn(async move {
			axum_server::from_tcp_rustls(listener, tls_config)
				.unwrap()
				.handle(serve_handle)
				.serve(app.into_make_service())
				.await
				.unwrap();
		});

		// Wait for the server to be ready to accept connections.
		handle.listening().await;

		MtlsFixture {
			_dir: dir,
			ca_pem_path,
			client_cert_path,
			client_key_path,
			base_url: format!("https://{addr}"),
			key,
		}
	}

	#[tokio::test]
	async fn test_mtls_identity_is_presented() -> anyhow::Result<()> {
		let fx = mtls_fixture().await;

		// With identity: the server accepts the connection and returns the JWK.
		let auth_with_identity = Auth::new(AuthConfig {
			key_dir: Some(format!("{}/keys/", fx.base_url)),
			tls: AuthTls {
				root: vec![fx.ca_pem_path.clone()],
				cert: Some(fx.client_cert_path.clone()),
				key: Some(fx.client_key_path.clone()),
				disable_verify: None,
			},
			..Default::default()
		})
		.await?;

		let claims = moq_token::Claims {
			root: "room/1".to_string(),
			subscribe: vec!["".to_string()],
			..Default::default()
		};
		let token = fx.key.encode(&claims)?;
		let verified = auth_with_identity
			.verify(&AuthParams {
				path: "/room/1".into(),
				jwt: Some(token.clone()),
			})
			.await?;
		assert_eq!(verified.root, "room/1".as_path());

		// Without identity: the server should reject the TLS handshake → ApiUnavailable.
		let auth_no_identity = Auth::new(AuthConfig {
			key_dir: Some(format!("{}/keys/", fx.base_url)),
			tls: AuthTls {
				root: vec![fx.ca_pem_path.clone()],
				cert: None,
				key: None,
				disable_verify: None,
			},
			..Default::default()
		})
		.await?;
		let result = auth_no_identity
			.verify(&AuthParams {
				path: "/room/1".into(),
				jwt: Some(token),
			})
			.await;
		assert!(
			matches!(result, Err(AuthError::ApiUnavailable(_))),
			"expected ApiUnavailable when client cert missing, got {result:?}"
		);

		Ok(())
	}

	fn parse(url: &str, domains: &[&str]) -> AuthParams {
		// Mirror the canonicalization Auth::new does so unit tests of from_url
		// take suffixes in the same form callers would.
		let domains: Vec<String> = domains
			.iter()
			.map(|s| format!(".{}", s.trim_start_matches('.').to_ascii_lowercase()))
			.collect();
		AuthParams::from_url(&url::Url::parse(url).unwrap(), &domains)
	}

	#[test]
	fn test_match_domain_slug_prepended() {
		let p = parse("https://customer.cdn.moq.dev/foo", &["cdn.moq.dev"]);
		assert_eq!(p.path, "/customer/foo");
	}

	#[test]
	fn test_match_domain_exact_suffix_no_slug() {
		let p = parse("https://cdn.moq.dev/foo", &["cdn.moq.dev"]);
		assert_eq!(p.path, "/foo");
	}

	#[test]
	fn test_match_domain_non_matching_host() {
		let p = parse("https://something.else.com/foo", &["cdn.moq.dev"]);
		assert_eq!(p.path, "/foo");
	}

	#[test]
	fn test_match_domain_empty_path_with_slug() {
		// url::Url canonicalizes an empty path to "/", so the output is
		// "/customer/" rather than "/customer" — the trailing slash is harmless
		// since Path strips it.
		let p = parse("https://customer.cdn.moq.dev/", &["cdn.moq.dev"]);
		assert_eq!(p.path, "/customer/");
	}

	#[test]
	fn test_match_domain_multi_label_to_path() {
		// Multi-label slugs reverse so the DNS label closest to the suffix
		// (broadest scope) becomes the outermost path segment. With suffix
		// `cdn.moq.dev`, `team.customer.cdn.moq.dev/foo` routes to
		// `/customer/team/foo` — the customer is the broader scope.
		let p = parse("https://team.customer.cdn.moq.dev/foo", &["cdn.moq.dev"]);
		assert_eq!(p.path, "/customer/team/foo");
	}

	#[test]
	fn test_match_domain_multiple_non_overlapping_suffixes() {
		let p = parse(
			"https://customer.staging.moq.dev/foo",
			&["cdn.moq.dev", "staging.moq.dev"],
		);
		assert_eq!(p.path, "/customer/foo");
	}

	#[test]
	fn test_match_domain_case_insensitive() {
		let p = parse("https://CUSTOMER.CDN.moq.dev/Foo", &["cdn.moq.dev"]);
		// The URL crate lowercases the host but preserves the path case.
		assert_eq!(p.path, "/customer/Foo");
	}

	#[test]
	fn test_match_domain_no_domains_configured() {
		let p = parse("https://customer.cdn.moq.dev/foo", &[]);
		assert_eq!(p.path, "/foo");
	}

	#[test]
	fn test_match_domain_preserves_jwt() {
		let p = parse("https://customer.cdn.moq.dev/foo?jwt=abc", &["cdn.moq.dev"]);
		assert_eq!(p.path, "/customer/foo");
		assert_eq!(p.jwt.as_deref(), Some("abc"));
	}

	#[tokio::test]
	async fn test_match_domain_overlapping_suffixes_longest_first() -> anyhow::Result<()> {
		// `Auth::new` sorts configured domains longest-first so that a nested
		// suffix like "usw.cdn.moq.dev" wins over its parent "cdn.moq.dev".
		// Without this, `customer.usw.cdn.moq.dev` would route under
		// "cdn.moq.dev" as `/usw/customer/foo` depending on the configured
		// order, instead of `/customer/foo` under "usw.cdn.moq.dev".
		for order in [
			vec!["cdn.moq.dev".to_string(), "usw.cdn.moq.dev".to_string()],
			vec!["usw.cdn.moq.dev".to_string(), "cdn.moq.dev".to_string()],
		] {
			let auth = Auth::new(AuthConfig {
				public: detailed_public(&["customer"], &[]),
				domains: order,
				..Default::default()
			})
			.await?;
			let params = auth.params_from_url(&url::Url::parse("https://customer.usw.cdn.moq.dev/foo")?);
			assert_eq!(params.path, "/customer/foo");
		}
		Ok(())
	}

	#[tokio::test]
	async fn test_subdomain_slug_flows_through_public_prefix() -> anyhow::Result<()> {
		// End-to-end: a subdomain slug, combined with a public prefix scoped to
		// the customer, authorizes a connection that would otherwise be rejected.
		let auth = Auth::new(AuthConfig {
			public: detailed_public(&["customer/anon"], &[]),
			domains: vec!["cdn.moq.dev".to_string()],
			..Default::default()
		})
		.await?;

		let params = auth.params_from_url(&url::Url::parse("https://customer.cdn.moq.dev/anon/room")?);
		assert_eq!(params.path, "/customer/anon/room");

		let token = auth.verify(&params).await?;
		assert_eq!(token.root, Path::new("customer/anon/room").to_owned());
		assert_eq!(token.subscribe, vec!["".as_path()]);

		// A different customer under the same suffix is rejected by the prefix check.
		let params = auth.params_from_url(&url::Url::parse("https://other.cdn.moq.dev/anon/room")?);
		assert_eq!(params.path, "/other/anon/room");
		assert!(auth.verify(&params).await.is_err());

		Ok(())
	}

	#[test]
	fn unrestricted_scopes_to_root() {
		// An mTLS publisher dialing "/demo" must announce under the `demo` root,
		// not the cluster root, so path-scoped subscribers (e.g. `demo/*`) see it.
		let token = AuthToken::unrestricted(Path::new("/demo").to_owned());
		assert_eq!(token.root, "demo".as_path());
		assert_eq!(token.subscribe, vec!["".as_path()]);
		assert_eq!(token.publish, vec!["".as_path()]);
		// The billing tier is set by the caller, not baked into the token.
		assert_eq!(token.tier, Tier::default());
	}

	#[test]
	fn unrestricted_empty_root_is_unscoped() {
		// Cluster peers dial "/", which normalizes to an empty root, leaving the
		// grant unscoped across the whole cluster.
		let token = AuthToken::unrestricted(Path::new("/").to_owned());
		assert_eq!(token.root, "".as_path());
		assert_eq!(token.tier, Tier::default());
	}

	// ---------------------------------------------------------------------
	// Unified --auth-api
	// ---------------------------------------------------------------------

	/// Build an Auth wired to a wiremock server's `/auth` unified endpoint.
	async fn auth_with_api(server: &MockServer) -> Auth {
		Auth::new(AuthConfig {
			auth_api: Some(format!("{}/auth", server.uri())),
			..Default::default()
		})
		.await
		.unwrap()
	}

	#[tokio::test]
	async fn auth_api_jwt_scopes_to_alias() -> anyhow::Result<()> {
		// JWT connection: the unified call returns the verifying key plus the
		// full resolved alias; the token scopes to that alias root.
		let server = MockServer::start().await;
		let key = create_test_key_with_kid("test-key");

		Mock::given(method("GET"))
			.and(path_matcher("/auth"))
			.and(query_param("root", "demo/room"))
			.respond_with(
				ResponseTemplate::new(200)
					.set_body_string(format!(r#"{{"alias":"x7k2qp/room","key":{}}}"#, jwk_body(&key))),
			)
			.mount(&server)
			.await;

		let auth = auth_with_api(&server).await;

		let claims = moq_token::Claims {
			root: "x7k2qp/room".to_string(),
			subscribe: vec!["".to_string()],
			..Default::default()
		};
		let token = key.encode(&claims)?;

		let verified = auth
			.verify(&AuthParams {
				path: "/demo/room".into(),
				jwt: Some(token),
			})
			.await?;
		assert_eq!(verified.root, "x7k2qp/room".as_path());
		assert_eq!(verified.subscribe, vec!["".as_path()]);
		Ok(())
	}

	#[tokio::test]
	async fn auth_api_full_root_passthrough() -> anyhow::Result<()> {
		// The server returns the FULL resolved root (deep path preserved); the
		// relay uses it verbatim — no client-side first-segment rewriting.
		let server = MockServer::start().await;
		let key = create_test_key_with_kid("test-key");

		Mock::given(method("GET"))
			.and(path_matcher("/auth"))
			.and(query_param("root", "demo/room/cam"))
			.respond_with(
				ResponseTemplate::new(200)
					.set_body_string(format!(r#"{{"alias":"x7k2qp/room/cam","key":{}}}"#, jwk_body(&key))),
			)
			.mount(&server)
			.await;

		let auth = auth_with_api(&server).await;
		let claims = moq_token::Claims {
			root: "x7k2qp".to_string(),
			subscribe: vec!["".to_string()],
			..Default::default()
		};
		let token = key.encode(&claims)?;

		let verified = auth
			.verify(&AuthParams {
				path: "/demo/room/cam".into(),
				jwt: Some(token),
			})
			.await?;
		assert_eq!(verified.root, "x7k2qp/room/cam".as_path());
		Ok(())
	}

	#[tokio::test]
	async fn auth_api_anonymous_uses_public() -> anyhow::Result<()> {
		// No JWT: claims come from the `public` field, anchored at the alias root.
		let server = MockServer::start().await;
		Mock::given(method("GET"))
			.and(path_matcher("/auth"))
			.and(query_param("root", "demo"))
			.respond_with(
				ResponseTemplate::new(200).set_body_string(r#"{"alias":"x7k2qp","public":{"subscribe":["cam"]}}"#),
			)
			.mount(&server)
			.await;

		let auth = auth_with_api(&server).await;
		let verified = auth.verify(&AuthParams::new("/demo")).await?;
		assert_eq!(verified.root, "x7k2qp".as_path());
		assert_eq!(verified.subscribe, vec!["cam".as_path()]);
		assert_eq!(verified.publish, vec![]);
		assert_eq!(verified.tier, Tier::default());
		Ok(())
	}

	#[tokio::test]
	async fn auth_api_tier_label_buckets_connection() -> anyhow::Result<()> {
		// A non-mTLS connection can be assigned any billing tier label by the API
		// (e.g. a first-party dashboard token), defaulting to the unprefixed tier.
		let server = MockServer::start().await;
		Mock::given(method("GET"))
			.and(path_matcher("/auth"))
			.and(query_param("root", "demo"))
			.respond_with(
				ResponseTemplate::new(200)
					.set_body_string(r#"{"alias":"x7k2qp","public":{"subscribe":[""]},"tier":"internal"}"#),
			)
			.mount(&server)
			.await;

		let auth = auth_with_api(&server).await;
		let verified = auth.verify(&AuthParams::new("/demo")).await?;
		assert_eq!(verified.tier, Tier::new("internal"));
		Ok(())
	}

	#[tokio::test]
	async fn auth_api_legacy_internal_bool_maps_to_tier() -> anyhow::Result<()> {
		// Backward compatibility: an auth API written against the original
		// contract returns `internal: bool`, not `tier`. `true` -> `internal`.
		let server = MockServer::start().await;
		Mock::given(method("GET"))
			.and(path_matcher("/auth"))
			.and(query_param("root", "demo"))
			.respond_with(
				ResponseTemplate::new(200).set_body_string(r#"{"public":{"subscribe":[""]},"internal":true}"#),
			)
			.mount(&server)
			.await;

		let auth = auth_with_api(&server).await;
		let verified = auth.verify(&AuthParams::new("/demo")).await?;
		assert_eq!(verified.tier, Tier::new("internal"));
		Ok(())
	}

	#[test]
	fn auth_api_response_tier_precedence() {
		// New `tier` label wins over the legacy `internal` bool when both are present.
		let both: AuthApiResponse = serde_json::from_str(r#"{"tier":"region/sjc","internal":true}"#).unwrap();
		assert_eq!(both.tier(), Some(Tier::new("region/sjc")));

		// Legacy `internal: false` demotes to the default (unprefixed) tier.
		let legacy_false: AuthApiResponse = serde_json::from_str(r#"{"internal":false}"#).unwrap();
		assert_eq!(legacy_false.tier(), Some(Tier::default()));

		// Neither field present: the relay applies its per-connection default.
		let neither: AuthApiResponse = serde_json::from_str(r#"{}"#).unwrap();
		assert_eq!(neither.tier(), None);
	}

	#[tokio::test]
	async fn auth_api_unknown_project_echoes_path() -> anyhow::Result<()> {
		// Absent `alias` -> the relay falls back to the request path as the root.
		let server = MockServer::start().await;
		let key = create_test_key_with_kid("test-key");
		Mock::given(method("GET"))
			.and(path_matcher("/auth"))
			.and(query_param("root", "unknown"))
			.respond_with(ResponseTemplate::new(200).set_body_string(format!(r#"{{"key":{}}}"#, jwk_body(&key))))
			.mount(&server)
			.await;

		let auth = auth_with_api(&server).await;
		let claims = moq_token::Claims {
			root: "unknown".to_string(),
			subscribe: vec!["".to_string()],
			..Default::default()
		};
		let token = key.encode(&claims)?;
		let verified = auth
			.verify(&AuthParams {
				path: "/unknown".into(),
				jwt: Some(token),
			})
			.await?;
		assert_eq!(verified.root, "unknown".as_path());
		Ok(())
	}

	#[tokio::test]
	async fn auth_api_missing_key_rejects_jwt() -> anyhow::Result<()> {
		// A JWT connection whose kid the API can't resolve (no `key`) is rejected.
		let server = MockServer::start().await;
		let key = create_test_key_with_kid("test-key");
		Mock::given(method("GET"))
			.and(path_matcher("/auth"))
			.and(query_param("root", "demo"))
			.respond_with(ResponseTemplate::new(200).set_body_string(r#"{"alias":"x7k2qp"}"#))
			.mount(&server)
			.await;

		let auth = auth_with_api(&server).await;
		let token = key.encode(&moq_token::Claims {
			root: "x7k2qp".to_string(),
			subscribe: vec!["".to_string()],
			..Default::default()
		})?;
		let result = auth
			.verify(&AuthParams {
				path: "/demo".into(),
				jwt: Some(token),
			})
			.await;
		assert!(matches!(result, Err(AuthError::KeyNotFound)));
		Ok(())
	}

	#[tokio::test]
	async fn auth_api_server_error_fails_closed() -> anyhow::Result<()> {
		// Unlike the old alias step, the unified call fails CLOSED: the key comes
		// from here, so a 5xx must reject rather than silently allow.
		let server = MockServer::start().await;
		Mock::given(method("GET"))
			.respond_with(ResponseTemplate::new(500))
			.mount(&server)
			.await;

		let auth = auth_with_api(&server).await;
		let result = auth.verify(&AuthParams::new("/demo")).await;
		assert!(result.is_err());
		Ok(())
	}

	#[tokio::test]
	async fn auth_api_mtls_resolves_alias_and_tier() -> anyhow::Result<()> {
		// mTLS peers get the canonical root + tier; absent `tier` defaults to
		// `internal` (trusted peer).
		let server = MockServer::start().await;
		Mock::given(method("GET"))
			.and(path_matcher("/auth"))
			.and(query_param("root", "demo/room"))
			.respond_with(ResponseTemplate::new(200).set_body_string(r#"{"alias":"x7k2qp/room"}"#))
			.mount(&server)
			.await;

		let auth = auth_with_api(&server).await;
		assert_eq!(
			auth.resolve_mtls("/demo/room").await?,
			("x7k2qp/room".to_string(), Tier::new("internal"))
		);
		Ok(())
	}

	#[tokio::test]
	async fn auth_api_mtls_tier_override_default() -> anyhow::Result<()> {
		// The API can move a cert-verified connection to the default (unprefixed)
		// tier by returning an empty label.
		let server = MockServer::start().await;
		Mock::given(method("GET"))
			.and(path_matcher("/auth"))
			.and(query_param("root", "demo"))
			.respond_with(ResponseTemplate::new(200).set_body_string(r#"{"alias":"x7k2qp","tier":""}"#))
			.mount(&server)
			.await;

		let auth = auth_with_api(&server).await;
		assert_eq!(
			auth.resolve_mtls("/demo").await?,
			("x7k2qp".to_string(), Tier::default())
		);
		Ok(())
	}

	#[tokio::test]
	async fn auth_api_mtls_resolves_root_via_api() -> anyhow::Result<()> {
		// Root connections go through the API too, so it owns the alias + tier for
		// every mTLS peer. Here the API aliases the root and buckets it to `region`.
		let server = MockServer::start().await;
		Mock::given(method("GET"))
			.and(path_matcher("/auth"))
			.and(query_param("root", ""))
			.respond_with(ResponseTemplate::new(200).set_body_string(r#"{"alias":"x7k2qp","tier":"region"}"#))
			.mount(&server)
			.await;

		let auth = auth_with_api(&server).await;
		assert_eq!(
			auth.resolve_mtls("/").await?,
			("x7k2qp".to_string(), Tier::new("region"))
		);
		Ok(())
	}

	#[tokio::test]
	async fn auth_api_mtls_no_api_fails_open() -> anyhow::Result<()> {
		// With no auth API configured the cert is the only credential: use the path
		// unchanged at the internal tier. This is the sole fail-open case. (A public
		// path just makes the config valid; mTLS resolution ignores it.)
		let auth = Auth::new(AuthConfig {
			public: simple_public("anon"),
			..Default::default()
		})
		.await?;
		assert_eq!(
			auth.resolve_mtls("/demo").await?,
			("/demo".to_string(), Tier::new("internal"))
		);
		assert_eq!(auth.resolve_mtls("/").await?, ("/".to_string(), Tier::new("internal")));
		Ok(())
	}

	#[tokio::test]
	async fn auth_api_mtls_fails_closed_on_api_error() -> anyhow::Result<()> {
		// A non-root mTLS path needs an alias. If the API can't answer, reject the
		// connection instead of accepting it with the path unresolved (which would
		// route the broadcast to the literal vanity path and strand the publisher).
		let server = MockServer::start().await;
		Mock::given(method("GET"))
			.and(path_matcher("/auth"))
			.and(query_param("root", "demo"))
			.respond_with(ResponseTemplate::new(404))
			.mount(&server)
			.await;

		let auth = auth_with_api(&server).await;
		let err = auth.resolve_mtls("/demo").await.unwrap_err();
		assert!(matches!(err, AuthError::ApiUnavailable(_)));
		assert_eq!(http::StatusCode::from(err), http::StatusCode::BAD_GATEWAY);
		Ok(())
	}

	#[tokio::test]
	async fn auth_api_mtls_fails_closed_on_invalid_json() -> anyhow::Result<()> {
		// A 2xx with an unparseable body is still an upstream failure: classify it
		// as 502 (not a credential 401) so the mTLS peer reconnects and self-heals.
		let server = MockServer::start().await;
		Mock::given(method("GET"))
			.and(path_matcher("/auth"))
			.and(query_param("root", "demo"))
			.respond_with(ResponseTemplate::new(200).set_body_string("not json"))
			.mount(&server)
			.await;

		let auth = auth_with_api(&server).await;
		let err = auth.resolve_mtls("/demo").await.unwrap_err();
		assert!(matches!(err, AuthError::ApiInvalidResponse(_)));
		assert_eq!(http::StatusCode::from(err), http::StatusCode::BAD_GATEWAY);
		Ok(())
	}

	#[tokio::test]
	async fn auth_api_mutually_exclusive_with_key_dir() {
		// --auth-api can't be combined with the standalone key/public sources.
		let result = Auth::new(AuthConfig {
			auth_api: Some("https://api.example.com/cluster/auth".into()),
			key_dir: Some("https://api.example.com/cluster/keys".into()),
			..Default::default()
		})
		.await;
		assert!(result.is_err());
	}
}
