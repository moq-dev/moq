use anyhow::Context;
use axum::http;
use futures::FutureExt;
#[cfg(test)]
use moq_net::AsPath;
use moq_net::{Path, PathOwned, PathPrefixes, stats::Tier};
use moq_token::{Key, KeyId};
use moq_tokio::Transport;
use rand::RngExt;
use reqwest_middleware::ClientWithMiddleware;
use serde::{Deserialize, Serialize};
use serde_with::{OneOrMany, formats::PreferMany, serde_as};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::time::Instant;
use url::Url;

/// Parameters extracted from an incoming connection for authentication: the
/// request-derived path + JWT, plus metadata about the connection itself that the
/// auth API can bucket on (e.g. the transport). Connection metadata is set by the
/// relay after parsing the request (the URL/SETUP parsers don't know it).
#[derive(Default, Debug, Clone)]
pub struct AuthParams {
	/// The URL path identifying the broadcast root.
	pub path: String,
	/// A JWT token, if provided via the `jwt` query parameter.
	pub jwt: Option<String>,
	/// The connection's transport, forwarded to the auth API as `transport=` so it
	/// can bucket by connection type (e.g. bill traffic on the internal Unix-socket
	/// listener -- the out-of-process RTMP/SRT/WebRTC gateways, `"unix"` -- into a
	/// distinct tier). Absent (`None`) sends no `transport` parameter.
	pub transport: Option<Transport>,
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

		Self {
			path,
			jwt,
			..Default::default()
		}
	}

	/// Extract authentication parameters from an already-separated path and query.
	///
	/// URL-less transports (a qmux Unix socket, raw QUIC) carry the request path
	/// in the moq-lite-05 SETUP rather than a real request URI, so there is no
	/// host and no subdomain->path routing to apply; the caller (a gateway) has
	/// already prepended any vanity prefix. Only the `jwt` query parameter is
	/// URL-decoded. The public request API represents a missing or root path as
	/// empty, so authentication canonicalizes it back to `/` like a URL does.
	pub(crate) fn from_path_query(path: &str, query: Option<&str>) -> Self {
		let jwt = query.and_then(|query| {
			url::form_urlencoded::parse(query.as_bytes())
				.filter(|(k, v)| k == "jwt" && !v.is_empty())
				.map(|(_, v)| v.into_owned())
				.last()
		});

		Self {
			path: if path.is_empty() { "/" } else { path }.to_string(),
			jwt,
			..Default::default()
		}
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

	#[error("the auth API has no grant for this connection")]
	NotFound,

	#[error("the auth API could not be revalidated; served a stale cached response")]
	ApiStale,

	#[error("missing key ID in token")]
	MissingKeyId,

	#[error("auth API request failed: {0}")]
	ApiUnavailable(String),

	#[error("auth API response was invalid: {0}")]
	ApiInvalidResponse(String),

	#[error("invalid URL: {0}")]
	InvalidUrl(String),

	#[error(transparent)]
	InvalidKeyId(#[from] moq_token::KeyIdError),
}

impl AuthError {
	/// True when the auth API answered and the answer was "no".
	///
	/// The distinction is what lets [`Auth::revalidate`] tell a withdrawn grant
	/// from an unreachable API: a refusal closes the session immediately, while
	/// everything else is evidence of nothing and only closes once the staleness
	/// window passes. An unrecognized error is NOT a refusal, so a new failure
	/// mode keeps sessions serving rather than mass-disconnecting them.
	fn is_refusal(&self) -> bool {
		matches!(
			self,
			Self::UnexpectedToken
				| Self::ExpectedToken
				| Self::DecodeFailed
				| Self::IncorrectRoot
				| Self::KeyNotFound
				| Self::MissingKeyId
				| Self::NotFound
				| Self::InvalidKeyId(_)
		)
	}
}

/// Renders an error and its `source()` chain into a single message.
///
/// Dependency errors are stored as messages so their crates stay out of this crate's public
/// API. Several of them keep the actionable half in `source()` and nothing but a category in
/// `Display`, so a plain `to_string()` would drop the only detail worth reporting.
pub(crate) fn message(err: impl std::error::Error) -> String {
	use std::fmt::Write;

	let mut out = err.to_string();
	let mut source = err.source();
	while let Some(err) = source {
		let _ = write!(out, ": {err}");
		source = err.source();
	}
	out
}

// Dependency errors are flattened to their message so their crates stay out of this crate's
// public API. `reqwest` in particular reports only a category ("error sending request for url
// ...") and leaves the DNS, TLS, or connection cause in `source()`, so an auth outage would
// otherwise be undiagnosable. Both HTTP error types land on the same variant, so `?` works on `.send()`
// (which returns `reqwest_middleware::Error`) and on `.error_for_status()` / `.text()`
// (which return `reqwest::Error`) alike.
impl From<reqwest::Error> for AuthError {
	fn from(err: reqwest::Error) -> Self {
		Self::ApiUnavailable(message(err))
	}
}

impl From<reqwest_middleware::Error> for AuthError {
	fn from(err: reqwest_middleware::Error) -> Self {
		Self::ApiUnavailable(message(err))
	}
}

impl From<serde_json::Error> for AuthError {
	fn from(err: serde_json::Error) -> Self {
		Self::ApiInvalidResponse(message(err))
	}
}

impl From<url::ParseError> for AuthError {
	fn from(err: url::ParseError) -> Self {
		Self::InvalidUrl(message(err))
	}
}

impl From<&AuthError> for http::StatusCode {
	fn from(err: &AuthError) -> Self {
		match err {
			// Upstream auth API unreachable or misconfigured — this is a server-side
			// problem, not a credential problem.
			// A 404 is reported the same way. It is a deterministic answer, which is
			// why revalidation treats it as a refusal, but at ADMISSION it is far
			// more often a misconfigured or half-deployed endpoint than a real
			// verdict -- and 502 is what lets an mTLS cluster peer reconnect and
			// self-heal once the endpoint is fixed.
			// `ApiStale` belongs here too: the endpoint was unreachable and the cache
			// answered for it. Reporting that as 401 would tell a client its
			// credential was rejected when nothing was ever checked.
			AuthError::ApiUnavailable(_)
			| AuthError::ApiInvalidResponse(_)
			| AuthError::NotFound
			| AuthError::ApiStale => http::StatusCode::BAD_GATEWAY,
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

/// Deprecated `--auth-tls-*` overrides, kept for backwards compatibility. The
/// auth client otherwise reuses the cluster client's `--connect-tls-*` config.
/// Hidden from `--help`; setting any field logs a deprecation warning.
#[doc(hidden)]
#[serde_as]
#[derive(Clone, Default, Debug, usage::Args, Serialize, Deserialize)]
#[usage(unknown_flags = "error", args_override_self = false)]
#[serde(default, deny_unknown_fields)]
#[non_exhaustive]
pub struct AuthTls {
	#[serde(skip_serializing_if = "Vec::is_empty")]
	#[usage(
		name = "auth-tls-root",
		long = "auth-tls-root",
		env = "MOQ_AUTH_TLS_ROOT",
		hide = true
	)]
	#[serde_as(as = "OneOrMany<_>")]
	pub root: Vec<PathBuf>,

	#[serde(skip_serializing_if = "Option::is_none")]
	#[usage(
		name = "auth-tls-cert",
		long = "auth-tls-cert",
		env = "MOQ_AUTH_TLS_CERT",
		hide = true
	)]
	pub cert: Option<PathBuf>,

	#[serde(skip_serializing_if = "Option::is_none")]
	#[usage(name = "auth-tls-key", long = "auth-tls-key", env = "MOQ_AUTH_TLS_KEY", hide = true)]
	pub key: Option<PathBuf>,

	#[serde(skip_serializing_if = "Option::is_none")]
	#[usage(
		name = "auth-tls-disable-verify",
		long = "auth-tls-disable-verify",
		env = "MOQ_AUTH_TLS_DISABLE_VERIFY",
		hide = true,
		default_missing = "true",
		num_args = 0..=1,
		require_equals = true,
	)]
	pub disable_verify: Option<bool>,
}

impl AuthTls {
	/// True when any deprecated `--auth-tls-*` override is configured, in which
	/// case it takes precedence over the shared `--connect-tls-*` identity.
	fn is_set(&self) -> bool {
		!self.root.is_empty() || self.cert.is_some() || self.key.is_some() || self.disable_verify.is_some()
	}

	/// Convert into a [`moq_tokio::tls::Connect`] so we can reuse its
	/// rustls-building logic. The fields map one-to-one.
	fn to_client_tls(&self) -> anyhow::Result<moq_tokio::tls::Connect> {
		match (&self.cert, &self.key) {
			(Some(_), None) => anyhow::bail!("--auth-tls-cert requires --auth-tls-key"),
			(None, Some(_)) => anyhow::bail!("--auth-tls-key requires --auth-tls-cert"),
			_ => {}
		}

		let mut tls = moq_tokio::tls::Connect::default();
		tls.root = self.root.clone();
		tls.cert = self.cert.clone();
		tls.key = self.key.clone();
		tls.insecure = self.disable_verify;
		Ok(tls)
	}
}

/// Configuration for JWT-based authentication.
#[serde_as]
#[derive(usage::Args, Clone, Debug, Serialize, Deserialize, Default)]
#[usage(unknown_flags = "error", args_override_self = false)]
#[serde(default)]
#[non_exhaustive]
pub struct AuthConfig {
	/// A single JWK key file for authentication.
	/// No `kid` header is required in JWTs.
	#[usage(long = "auth-key", env = "MOQ_AUTH_KEY")]
	pub key: Option<String>,

	/// A directory path or base URL containing JWK files named by key ID.
	///
	/// File path: reads `{dir}/{kid}.jwk` from disk.
	/// URL: fetches `{url}/{kid}.jwk` with HTTP caching.
	///
	/// DEPRECATED (URL form): prefer the unified `--auth-api`, which resolves the
	/// key in the same call as public access and the alias. The file-directory
	/// form remains supported for standalone relays.
	#[usage(long = "auth-key-dir", env = "MOQ_AUTH_KEY_DIR")]
	pub key_dir: Option<String>,

	/// Deprecated `--auth-tls-*` overrides; see [`AuthTls`].
	#[usage(flatten)]
	#[serde(default)]
	pub tls: AuthTls,

	/// Cluster client TLS injected by [`AuthConfig::init`] so outbound auth HTTP
	/// (JWK + auth/public-API fetches) reuses the `--connect-tls-*` identity.
	/// Not a CLI or TOML field; the deprecated `--auth-tls-*` flags override it.
	#[usage(skip)]
	#[serde(skip)]
	client_tls: Option<moq_tokio::tls::Connect>,

	/// Public (unauthenticated) access configuration.
	///
	/// CLI: `--auth-public <prefix>` sets both subscribe and publish for the prefix.
	/// TOML: Accepts a string, array, or table `{ subscribe = ..., publish = ... }`.
	/// Any value starting with `http://` or `https://` is treated as a URL endpoint.
	#[usage(long = "auth-public", env = "MOQ_AUTH_PUBLIC")]
	#[serde(default, deserialize_with = "PublicConfig::deserialize_option")]
	pub public: Option<PublicConfig>,

	/// Public (unauthenticated) subscribe access configuration.
	///
	/// CLI-only shorthand: `--auth-public-subscribe <prefix>` sets subscribe-only access.
	/// For TOML, use `[auth.public]` with separate `subscribe`/`publish` fields instead.
	#[usage(long = "auth-public-subscribe", env = "MOQ_AUTH_PUBLIC_SUBSCRIBE")]
	#[serde(skip)]
	pub public_subscribe: Option<PublicConfig>,

	/// Public (unauthenticated) publish access configuration.
	///
	/// CLI-only shorthand: `--auth-public-publish <prefix>` sets publish-only access.
	/// For TOML, use `[auth.public]` with separate `subscribe`/`publish` fields instead.
	#[usage(long = "auth-public-publish", env = "MOQ_AUTH_PUBLIC_PUBLISH")]
	#[serde(skip)]
	pub public_publish: Option<PublicConfig>,

	/// CLI-only shorthand: `--auth-public-api <url>` sets a URL endpoint that returns
	/// `{ subscribe: [...], publish: [...] }` per namespace. The connection namespace is
	/// appended to the URL. For TOML, use `[auth.public]` with an `api` field instead.
	///
	/// DEPRECATED: prefer the unified `--auth-api`, which returns public access in
	/// the same call as the key and alias.
	#[usage(long = "auth-public-api", env = "MOQ_AUTH_PUBLIC_API")]
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
	#[usage(long = "auth-domain", env = "MOQ_AUTH_DOMAIN")]
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
	/// Per connection the relay issues
	/// `GET <base>?root=<path>&kid=<kid>&mtls=true&transport=<transport>`
	/// over the same cached, mTLS-gated HTTP client used by the other auth fetches.
	/// `root` is the connection path (slashes preserved); `kid` is sent only when
	/// the connection carries a JWT (value from its header); `mtls=true` is sent
	/// only when the peer presented a verified client cert; `transport` is the
	/// connection's transport (`quic`/`websocket`/`tcp`/`unix`/`iroh`), so the API
	/// can bucket by connection type (e.g. tier Unix-socket gateway traffic
	/// separately). All are query params (never path segments), so the base URL is
	/// used verbatim. The response is a JSON object whose fields are ALL optional:
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
	/// - `tier`: the billing tier label (e.g. `region/sjc`). The relay forwards
	///   `mtls=true` and lets the API decide. Absent or empty selects the default
	///   unprefixed tier for every connection.
	///
	/// FAILS CLOSED: any network error, non-2xx status, or parse error rejects
	/// the connection. Unlike the standalone flags, the verifying key itself
	/// comes from this call, so there is no safe fallback; the response cache
	/// (`Cache-Control` from the endpoint) softens transient failures.
	///
	/// Example: `https://api.moq.dev/cluster/auth` (called as
	/// `?root=demo/room&kid=abc&mtls=true`).
	#[usage(long = "auth-api", env = "MOQ_AUTH_API")]
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub auth_api: Option<String>,

	/// Billing tier label for mTLS peers when the auth API doesn't return one
	/// (or no `--auth-api` is configured). Defaults to the unprefixed tier.
	#[usage(long = "auth-mtls-tier", env = "MOQ_AUTH_MTLS_TIER")]
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

/// The configured `--auth-api`, and the revalidation state that belongs to it.
///
/// One struct rather than two `Option`s that were always in lockstep:
/// revalidation is what `--auth-api` MEANS, so an endpoint that can refuse a new
/// connection can also stop a running one, and there is no flag to get that
/// wrong. Keeping them together is also what removes the "revalidation requires
/// an auth API" expect and the impossible-state plumbing around it.
#[derive(Clone)]
struct AuthApi {
	base: url::Url,
	client: ClientWithMiddleware,
	revalidator: Arc<Revalidator>,
}

/// Only the endpoint is worth printing: the HTTP client and the in-flight map
/// are noise in a log line, and the map is behind a mutex besides.
impl std::fmt::Debug for AuthApi {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("AuthApi").field("base", &self.base.as_str()).finish()
	}
}

impl AuthApi {
	/// True when the reply is a cached response the middleware served only
	/// because it could not revalidate it against the origin.
	fn revalidation_failed(headers: &http::HeaderMap) -> bool {
		headers
			.get_all(http::header::WARNING)
			.iter()
			.filter_map(|value| value.to_str().ok())
			.any(|value| value.trim_start().starts_with("111"))
	}

	/// One unified auth-API call. Fails CLOSED (any network / non-2xx / parse
	/// error is an `Err`): with `--auth-api` the verifying key comes from here,
	/// so there is no safe fallback. Also returns the response's `Cache-Control`
	/// timings, which drive revalidation.
	async fn fetch(&self, request: &AuthApiRequest) -> Result<(AuthApiResponse, CacheHints), AuthError> {
		let response = self.client.get(request.url(&self.base)).send().await?;

		// `Warning: 111` means the cache served a STALE entry because it could not
		// reach the origin (RFC 2616 14.46). Treating that as a success would let a
		// sustained outage hand back the same cached grant every cadence, resetting
		// the staleness deadline forever, so a revoked session would keep serving
		// for the whole outage and the window would never fail closed.
		if Self::revalidation_failed(response.headers()) {
			return Err(AuthError::ApiStale);
		}
		// A 404 is the endpoint answering, not failing: it means no grant here.
		// Named so revalidation can close the session immediately instead of
		// waiting out the staleness window, while admission keeps reporting it as
		// an upstream problem (see the StatusCode mapping).
		if response.status() == http::StatusCode::NOT_FOUND {
			return Err(AuthError::NotFound);
		}
		let response = response.error_for_status()?;
		let hints = CacheHints::from_headers(response.headers());
		let body = response.text().await?;
		Ok((serde_json::from_str(&body)?, hints))
	}
}

/// One lookup against the unified `--auth-api` endpoint.
#[derive(Debug)]
struct AuthApiRequest {
	/// The connection path.
	path: String,
	/// The JWT `kid` to resolve a verifying key for.
	kid: Option<String>,
	/// Set only after the relay has verified the peer's client certificate.
	mtls: bool,
	transport: Option<&'static str>,
}

impl AuthApiRequest {
	/// The request URL. Everything the endpoint keys on is a query param on the
	/// base URL - never a path segment - so client-controlled values are
	/// percent-encoded by `query_pairs_mut` and can't retarget the path/query.
	fn url(&self, base: &url::Url) -> url::Url {
		let mut url = base.clone();
		{
			let mut q = url.query_pairs_mut();
			q.append_pair("root", self.path.trim_matches('/'));
			if let Some(kid) = &self.kid {
				q.append_pair("kid", kid);
			}
			if self.mtls {
				q.append_pair("mtls", "true");
			}
			if let Some(transport) = self.transport {
				q.append_pair("transport", transport);
			}
		}
		url
	}

	/// What two sessions must share before they can share one re-check.
	///
	/// Derived from the request that actually gets sent, never rebuilt alongside
	/// it: a key assembled from parts can describe a different request than the
	/// one issued, which silently costs a re-check per viewer instead of per
	/// broadcast.
	fn identity(&self, base: &url::Url) -> FlightKey {
		FlightKey {
			url: self.url(base).into(),
		}
	}
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
	/// Billing tier label for this connection (e.g. `region/sjc`).
	/// The relay sends `mtls=true` when the peer presented a verified client
	/// cert and lets the API decide. Absent or empty selects the default
	/// unprefixed tier.
	#[serde(default)]
	tier: Option<String>,
}

impl AuthApiResponse {
	/// Billing tier this response selects. `None` leaves the choice to the
	/// relay's per-connection default.
	fn tier(&self) -> Option<Tier> {
		self.tier.clone().map(Tier::new)
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
	///
	/// `client_tls` is the cluster client TLS (`--connect-tls-*`); the auth client
	/// reuses it for outbound HTTP unless the deprecated `--auth-tls-*` flags are
	/// set.
	pub async fn init(mut self, client_tls: &moq_tokio::tls::Connect) -> anyhow::Result<Auth> {
		self.client_tls = Some(client_tls.clone());
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
	/// through configuration or the auth API's `tier` field; defaults to the
	/// unprefixed tier.
	pub tier: Tier,
	/// When the credential backing this session expires, if it has an expiry.
	///
	/// For JWT auth this is the token's `exp` claim; for mTLS it's the peer
	/// certificate's `notAfter`. The relay closes the session once this passes
	/// instead of trusting a credential that was only checked at connect time.
	pub expires: Option<std::time::SystemTime>,
	/// The grant the auth API must keep vouching for; see [`Auth::revalidate`].
	/// Set for every auth-API session, anonymous ones included; never for mTLS.
	pub(crate) revalidate: Option<Revalidate>,
}

impl AuthToken {
	/// Wait until the backing credential expires, or forever when it has no expiry.
	pub(crate) async fn expired(&self) {
		match self.expires {
			Some(expires) => {
				let remaining = expires.duration_since(std::time::SystemTime::now()).unwrap_or_default();
				tokio::time::sleep(remaining).await
			}
			None => std::future::pending().await,
		}
	}

	/// Construct a token for a peer that was authenticated at the TLS layer
	/// via mTLS. These peers are granted full publish and subscribe access
	/// within `root`. The billing tier is left at the default; the caller (mTLS
	/// handshake or cluster dial) sets it from config. The cert's trust chain
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
			revalidate: None,
		}
	}
}

/// A live session's auth-API grant, re-checked by [`Auth::revalidate`].
///
/// The re-check REPLAYS the admission request rather than asking a narrower
/// question, and the session survives only while the replay still produces a
/// grant covering `scope`. That one predicate is what makes revocation correct
/// for every credential shape at once:
///
/// - a disabled key, or a gated project, stops returning a key or a public
///   grant, so the replay refuses;
/// - a key REPLACED under the same `kid` fails to verify the retained JWT, which
///   a "does some key exist for this kid" check would have missed entirely;
/// - an anonymous session has no `kid` and no `exp`, so the replay is its ONLY
///   bound - and it is the case that matters most, since a tokenless viewer of a
///   gated project would otherwise draw billable traffic forever;
/// - a grant resolved by the endpoint itself has no key to look for at all.
#[derive(Debug, Clone)]
pub(crate) struct Revalidate {
	/// The endpoint that admitted the session, and therefore the only authority
	/// that can revoke it. Carried rather than resolved from whichever [`Auth`]
	/// happens to run the re-check, so a token can never be judged against a
	/// differently-configured endpoint, or against none at all.
	api: AuthApi,
	/// The admission request, replayed verbatim on each re-check.
	params: Arc<AuthParams>,
	/// The scope admission granted. A replay that no longer covers it is a
	/// revocation, so a narrowed grant closes the session and the client
	/// reconnects into the narrower one.
	scope: Scope,
	/// The schedule admission resolved. Its existence IS the opt-in: no `max-age`
	/// on the admission reply means no `Revalidate` at all.
	schedule: Schedule,
}

/// The part of an [`AuthToken`] a re-check has to keep vouching for.
#[derive(Debug, Clone)]
struct Scope {
	root: PathOwned,
	subscribe: PathPrefixes,
	publish: PathPrefixes,
}

impl Scope {
	fn new(token: &AuthToken) -> Self {
		Self {
			root: token.root.clone(),
			subscribe: token.subscribe.clone(),
			publish: token.publish.clone(),
		}
	}

	/// True when `token` still grants everything this scope holds.
	///
	/// Deliberately "covers" and not "equals": an endpoint WIDENING a grant (the
	/// customer adds a public prefix) must not drop live sessions, while any loss
	/// of authority must.
	fn covered_by(&self, token: &AuthToken) -> bool {
		let covers = |granted: &PathPrefixes, held: &PathPrefixes| {
			held.iter()
				.all(|held| granted.iter().any(|granted| held.has_prefix(granted)))
		};
		self.root == token.root && covers(&token.subscribe, &self.subscribe) && covers(&token.publish, &self.publish)
	}
}

/// Why [`Auth::expired`] decided a session's credential is no longer valid.
///
/// `#[non_exhaustive]` because this is a new public enum and a future bound (a
/// lifecycle hook, a quota signal) should not be a breaking release.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Expired {
	/// The JWT `exp` (or client cert `notAfter`) passed.
	Credential,
	/// The auth API stopped vouching for the grant: it refused the replayed
	/// admission request, or answered with a grant that no longer covers what the
	/// session holds.
	Revoked,
	/// The auth API kept failing for the whole staleness window.
	Stale,
}

impl std::fmt::Display for Expired {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match self {
			Self::Credential => write!(f, "expired"),
			Self::Revoked => write!(f, "revoked"),
			Self::Stale => write!(f, "stale"),
		}
	}
}

/// The SHARED half of a re-check: one auth-API reply, reused by every session
/// that would have issued the identical request.
///
/// Deliberately not a verdict. Sessions sharing a [`FlightKey`] do not
/// necessarily hold the same grant - two anonymous sessions on one path can have
/// been admitted either side of a narrowed `public` block, and two JWTs under
/// one `kid` can carry different claims - so each waiter authorizes this reply
/// against its OWN credential and scope. Deciding once inside the flight would
/// hand the creator's answer to everybody, letting a session keep authority the
/// reply no longer grants.
#[derive(Clone)]
enum Fetched {
	/// The endpoint answered. Shared, so waiters read it rather than own it.
	Ok {
		resp: Arc<AuthApiResponse>,
		hints: CacheHints,
	},
	/// The endpoint refused, which is true for every waiter regardless of scope.
	Refused,
	/// The endpoint could not answer.
	Unavailable,
}

/// One session's conclusion, drawn from a [`Fetched`] against its own scope.
#[derive(Debug, Clone, Copy)]
enum Recheck {
	/// Still vouched for; check again after the new max-age.
	Valid { hints: CacheHints },
	/// The reply no longer grants what this session holds.
	Revoked,
	/// The API could not answer.
	Unavailable,
}

/// A registered in-flight re-check, shared by every session waiting on the same request. The map holds only a weak handle, so the request is
/// dropped with its last waiter instead of running on for nobody.
struct FlightSlot {
	id: u64,
	flight: futures::future::WeakShared<futures::future::BoxFuture<'static, Fetched>>,
}

/// One auth-API re-check request; sessions that would issue the identical
/// request share a flight. Built by [`AuthApiRequest::identity`].
///
/// The credential is not part of it: the response depends only on (`kid`, root,
/// transport), so an audience sharing one `kid` shares one re-check however many
/// distinct tokens they hold, and auth cost tracks broadcasts rather than
/// viewers.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct FlightKey {
	url: String,
}

/// Shared state for live-session revalidation.
#[derive(Default)]
struct Revalidator {
	/// In-flight re-checks; an entry removes itself when its flight ends,
	/// completed or abandoned.
	flights: Mutex<HashMap<FlightKey, FlightSlot>>,
	/// Source of [`FlightSlot::id`].
	next_flight: std::sync::atomic::AtomicU64,
}

/// Removes a flight's map entry when the flight is dropped, whether it ran to
/// completion or lost its last waiter mid-request.
struct FlightGuard {
	revalidator: Arc<Revalidator>,
	key: FlightKey,
	id: u64,
}

impl Revalidator {
	/// Initial retry backoff after a failed re-check, doubled up to the cadence.
	const BACKOFF: Duration = Duration::from_secs(1);
}

impl Drop for FlightGuard {
	fn drop(&mut self) {
		let Ok(mut flights) = self.revalidator.flights.lock() else {
			return;
		};
		if flights.get(&self.key).is_some_and(|slot| slot.id == self.id) {
			flights.remove(&self.key);
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
	auth_api: Option<AuthApi>,
	/// Billing tier recorded for an mTLS peer when the auth API doesn't return a
	/// tier (or none is configured). See [`AuthConfig::mtls_tier`].
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

		// Outbound auth HTTP (JWK + auth/public-API fetches) reuses the cluster
		// client's --client-tls-* identity. The deprecated --auth-tls-* flags
		// still override it when set.
		let tls_config = if config.tls.is_set() {
			tracing::warn!(
				"the --auth-tls-* flags are deprecated and will be removed; the auth client now \
				 reuses the cluster client TLS (--client-tls-root, --client-tls-cert, --client-tls-key). \
				 Drop --auth-tls-* and configure those instead."
			);
			config.tls.to_client_tls()?
		} else {
			config.client_tls.clone().unwrap_or_default()
		};
		let tls = tls_config.build()?;

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
		let auth_api = match config.auth_api {
			Some(url_str) => Some(AuthApi {
				base: Url::parse(&url_str).context("invalid --auth-api URL")?,
				client: Self::build_client(&tls)?,
				revalidator: Arc::default(),
			}),
			None => None,
		};

		Ok(Self {
			resolver,
			public,
			domains: Arc::from(domains.into_boxed_slice()),
			auth_api,
			mtls_tier: crate::configured_tier(config.mtls_tier),
		})
	}

	/// Override the mTLS fallback billing tier. For the
	/// mTLS-only stub built via [`Auth::default`], where there is no
	/// [`AuthConfig`] to carry `--auth-mtls-tier`. An empty label selects the
	/// default (unprefixed) tier.
	pub fn with_mtls_tier(mut self, tier: Option<String>) -> Self {
		self.mtls_tier = crate::configured_tier(tier);
		self
	}

	/// Build [`AuthParams`] from an incoming connection URL, applying any
	/// configured subdomain-based slug routing.
	pub(crate) fn params_from_url(&self, url: &url::Url) -> AuthParams {
		AuthParams::from_url(url, &self.domains)
	}

	/// Build a full-access token for a peer already authenticated by mTLS.
	///
	/// The HTTPS/QUIC layer verifies the client certificate before calling this.
	/// This method applies the relay's canonical alias resolution and billing
	/// tier decision, so embedded HTTP handlers get the same authorization scope
	/// as the built-in relay routes.
	pub async fn verify_mtls(&self, path: &str, transport: Option<Transport>) -> Result<AuthToken, AuthError> {
		let (root, tier) = self.resolve_mtls(path, transport).await?;
		let mut token = AuthToken::unrestricted(Path::new(&root).to_owned());
		token.tier = tier;
		Ok(token)
	}

	/// Resolve the canonical root and billing tier for an mTLS peer via the
	/// unified `--auth-api`. mTLS peers are already trusted (the cert is the
	/// credential), so this only fetches the alias + tier.
	///
	/// Fails OPEN only when there is no auth API configured: the cert is the
	/// credential and there is nothing to resolve, so the path and configured
	/// tier are used unchanged. Otherwise the API is the source of truth for every
	/// connection, including the root (`/`), so it can alias and tier root peers
	/// too. An API error therefore FAILS CLOSED (returns `Err`) rather than
	/// accepting the connection with the path unresolved. Accepting it would route
	/// the broadcast to the literal vanity path (e.g. `demo`) instead of its
	/// canonical root (e.g. `x7k2qp`), producing a zombie session: the publisher
	/// believes it is connected and never reconnects, but nothing is ever served.
	/// Failing closed lets the client retry and self-heal once the API recovers.
	async fn resolve_mtls(&self, path: &str, transport: Option<Transport>) -> Result<(String, Tier), AuthError> {
		let Some(api) = &self.auth_api else {
			return Ok((path.to_string(), self.mtls_tier.clone()));
		};

		let request = AuthApiRequest {
			path: path.to_string(),
			kid: None,
			mtls: true,
			transport: transport.map(Transport::as_str),
		};
		let (resp, _) = api.fetch(&request).await?;
		// Fall back to the configured mTLS tier when the API omits one.
		let tier = resp.tier().unwrap_or_else(|| self.mtls_tier.clone());
		Ok((resp.alias.unwrap_or_else(|| path.to_string()), tier))
	}

	/// Verify a connection via the unified `--auth-api`: one call returns the
	/// alias (root), the billing tier, and EITHER something to verify the
	/// credential against (a `key`) or the answer itself (a `grant`).
	async fn verify_via_api(&self, api: &AuthApi, params: &AuthParams) -> Result<(AuthToken, CacheHints), AuthError> {
		let request = self.api_request(params)?;
		let (resp, hints) = api.fetch(&request).await?;
		Ok((self.authorize(params, &resp)?, hints))
	}

	/// Turn one auth-API reply into this connection's token.
	///
	/// Split out from the fetch because the two have different scopes: the reply
	/// depends only on (`kid`, root, transport) and is shared, while the
	/// authorization depends on the credential and is emphatically NOT. See
	/// [`Fetched`].
	fn authorize(&self, params: &AuthParams, resp: &AuthApiResponse) -> Result<AuthToken, AuthError> {
		let claims = match params.jwt.as_deref() {
			Some(token) => {
				let key = resp.key.as_ref().ok_or(AuthError::KeyNotFound)?;
				// claims.root is the token's own root (a vanity name OR a pid); it is
				// checked against the ORIGINAL connection path below, not the alias, so
				// a vanity token matches a vanity URL and a pid token matches a pid URL.
				key.verify(token).map_err(|_| AuthError::DecodeFailed)?
			}
			None => {
				let public = resp.public.as_ref();
				let subscribe = public.map(|p| p.subscribe.clone()).unwrap_or_default();
				let publish = public.map(|p| p.publish.clone()).unwrap_or_default();
				if subscribe.is_empty() && publish.is_empty() {
					return Err(AuthError::ExpectedToken);
				}
				// Anonymous access: anchor the public claims at the connection path so
				// the overlap check below is a no-op; routing lands on the alias.
				moq_token::Claims::default()
					.with_root(params.path.clone())
					.with_subscribe(subscribe)
					.with_publish(publish)
			}
		};

		Self::finalize_api(params, resp.alias.clone(), resp.tier(), claims)
	}

	/// The auth-API request for a connection.
	///
	/// Admission and every re-check build the request here, so the flight key can
	/// be taken from the request itself rather than reconstructed beside it. The
	/// credential is never sent: the response depends only on (kid, root,
	/// transport), which is what lets a whole audience sharing a signing key
	/// resolve to one cached request per relay.
	fn api_request(&self, params: &AuthParams) -> Result<AuthApiRequest, AuthError> {
		Ok(AuthApiRequest {
			path: params.path.clone(),
			kid: match params.jwt.as_deref() {
				Some(token) => {
					jsonwebtoken::decode_header(token)
						.map_err(|_| AuthError::DecodeFailed)?
						.kid
				}
				None => None,
			},
			mtls: false,
			transport: params.transport.map(Transport::as_str),
		})
	}

	/// The flight key for a live session's re-check.
	fn flight_key(&self, grant: &Revalidate) -> Option<FlightKey> {
		Some(self.api_request(&grant.params).ok()?.identity(&grant.api.base))
	}

	/// Anchor verified claims on the API's alias, shared by both modes.
	///
	/// The API resolves the connection path's leading segment (a vanity name or
	/// pid) to the project's canonical pid. Broadcasts anchor there on the
	/// backbone so they survive vanity renames. An absent alias (unknown project)
	/// routes to the request path unchanged. Connections default to the unprefixed
	/// tier; the API may bucket specific ones under a named tier.
	fn finalize_api(
		params: &AuthParams,
		alias: Option<String>,
		tier: Option<Tier>,
		claims: moq_token::Claims,
	) -> Result<AuthToken, AuthError> {
		let alias = alias.unwrap_or_else(|| params.path.clone());
		// Check the token root against the ORIGINAL connection path (vanity or
		// pid); anchor the resulting scope on the alias (canonical pid).
		let mut token = Self::finalize(&params.path, &alias, claims)?;
		token.tier = tier.unwrap_or_default();
		Ok(token)
	}

	/// Admit a connection through the auth API and arm its re-check.
	///
	/// Every auth-API session is revalidated, anonymous ones included. A public
	/// grant is built from claims with no `exp` at all, so without this the relay
	/// would hold a gated project's tokenless viewers until the peer hung up.
	/// mTLS peers are the one exemption: they authenticate through
	/// [`Auth::verify_mtls`], which never reaches here, so the relay mesh is never
	/// torn down by a customer-facing decision.
	async fn admit_via_api(&self, api: &AuthApi, params: &AuthParams) -> Result<AuthToken, AuthError> {
		let (mut token, hints) = self.verify_via_api(api, params).await?;
		// No schedule means the endpoint named no `max-age`, so it has not asked to
		// be re-consulted; the credential's own `exp` stays the only bound.
		token.revalidate = hints.schedule().map(|schedule| Revalidate {
			api: api.clone(),
			params: Arc::new(params.clone()),
			scope: Scope::new(&token),
			schedule,
		});
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
		// The unified API resolves key/grant + public + alias in one call.
		if let Some(api) = &self.auth_api {
			return self.admit_via_api(api, params).await;
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
			key.verify(token).map_err(|_| AuthError::DecodeFailed)?
		} else if !self.public.is_empty() {
			// No JWT. Use public access (static prefixes + optional API).
			let root = Path::new(&params.path);

			// Use static config if any static prefix overlaps the request path in either
			// direction (request is under a public prefix, or request is a parent of one).
			let overlaps = |p: &Path| root.has_prefix(p) || p.has_prefix(&root);
			if self.public.subscribe.iter().any(&overlaps) || self.public.publish.iter().any(overlaps) {
				moq_token::Claims::default()
					.with_root("")
					.with_subscribe(self.public.subscribe.iter().map(|p| p.to_string()))
					.with_publish(self.public.publish.iter().map(|p| p.to_string()))
			} else if let Some((base, client)) = &self.public.api {
				// No static overlap. Response paths are relative to the namespace.
				let namespace = root.to_string();
				let url = base.join(&namespace)?;
				let response = Self::fetch_public_response(client, &url).await?;
				moq_token::Claims::default()
					.with_root(namespace)
					.with_subscribe(response.subscribe)
					.with_publish(response.publish)
			} else {
				return Err(AuthError::ExpectedToken);
			}
		} else {
			return Err(AuthError::ExpectedToken);
		};

		Self::finalize(&params.path, &params.path, claims)
	}

	/// Reduce verified `claims` into an [`AuthToken`].
	///
	/// [`Claims::authorize`](moq_token::Claims::authorize) does the overlap check and
	/// rebases the permission prefixes against `check_root` (the ORIGINAL connection
	/// path the client dialed, e.g. a vanity name); a token whose root sits outside
	/// that path is rejected. The resulting `AuthToken.root` is anchored at
	/// `route_root` (the `--auth-api` alias, i.e. the canonical pid), so broadcasts
	/// live under the stable pid on the backbone and survive vanity-name changes.
	/// `route_root` is `check_root` with only its leading segment swapped to the pid
	/// (same depth), so the rebased relative prefixes anchor unchanged. The standalone
	/// path passes the same value for both (no alias). Shared by the standalone and
	/// `--auth-api` paths.
	fn finalize(check_root: &str, route_root: &str, claims: moq_token::Claims) -> Result<AuthToken, AuthError> {
		let root = Path::new(check_root);
		let route_root = Path::new(route_root);
		let depth = |path: &Path<'_>| {
			if path.is_empty() {
				0
			} else {
				path.as_str().split('/').count()
			}
		};

		if depth(&root) != depth(&route_root) {
			return Err(AuthError::IncorrectRoot);
		}

		// A token that grants nothing here is indistinguishable from one aimed at
		// another root, so both reduce to IncorrectRoot.
		let permissions = claims.authorize(check_root).map_err(|_| AuthError::IncorrectRoot)?;

		// authorize() returns paths already normalized and relative to check_root,
		// which route_root matches in depth.
		let rebase = |paths: Vec<String>| -> PathPrefixes { paths.iter().map(|p| Path::new(p).to_owned()).collect() };

		Ok(AuthToken {
			root: route_root.to_owned(),
			subscribe: rebase(permissions.subscribe),
			publish: rebase(permissions.publish),
			tier: Tier::default(),
			expires: claims.expires,
			revalidate: None,
		})
	}

	/// Wait until `token` stops being valid, then say why.
	///
	/// Resolves when the credential's own bound passes (a JWT `exp`, or a client
	/// cert's `notAfter`), or when the auth API stops vouching for the grant -
	/// because a key was disabled or replaced, a project was gated, or the API
	/// went unreachable for long enough to fail closed. Pends forever for a token
	/// with neither bound, so it is always safe to `select!` on.
	///
	/// Every session that authenticated through `--auth-api` carries the second
	/// bound, anonymous ones included; mTLS peers carry neither. This is the one
	/// seam a session loop needs: selecting on the credential's expiry alone
	/// would keep serving through a revoked grant.
	///
	/// The endpoint consulted is the one that ADMITTED the session, carried on the
	/// token itself, so a process holding several differently-configured `Auth`
	/// instances still judges each token against the authority that issued it.
	pub async fn expired(&self, token: &AuthToken) -> Expired {
		let revoked = async {
			match &token.revalidate {
				Some(grant) => self.revalidate(grant).await,
				None => std::future::pending().await,
			}
		};

		tokio::select! {
			_ = token.expired() => Expired::Credential,
			reason = revoked => reason,
		}
	}

	/// Wait until the auth API stops vouching for this session's grant.
	///
	/// Re-checks ride the same cached HTTP client as admission, which is the other
	/// half of the cost story: coalescing merges re-checks in flight at the same
	/// moment, and the cache merges the ones that are not. Sessions start at
	/// different times, so without it a staggered audience would each dial on its
	/// own schedule. The price is that a re-check may be answered from an entry up
	/// to one `max-age` old, so the revocation window is up to TWICE the
	/// endpoint's `max-age`. Size it accordingly.
	///
	/// Re-checks ride the same cached HTTP client as admission, which is the other
	/// half of the cost story: coalescing merges re-checks in flight at the same
	/// moment, and the cache merges the ones that are not. Sessions start at
	/// different times, so without it a staggered audience would each dial on its
	/// own schedule. The price is that a re-check may be answered from an entry up
	/// to one `max-age` old, so the revocation window is up to TWICE the
	/// endpoint's `max-age`. Size it accordingly.
	///
	/// Re-checks on the endpoint's `Cache-Control: max-age` cadence and resolves
	/// once the API refuses the replayed request or answers with a smaller grant
	/// ([`Expired::Revoked`]), or keeps failing for the whole staleness window,
	/// 3x the last max-age ([`Expired::Stale`]).
	///
	/// The two failure directions are deliberately asymmetric. A refusal is the
	/// API answering, so it closes the session at once. Everything else is
	/// evidence of nothing, so the session keeps SERVING through jittered
	/// retries: a brief auth outage must not mass-disconnect a fleet's worth of
	/// viewers. A sustained one still fails closed.
	async fn revalidate(&self, grant: &Revalidate) -> Expired {
		let mut schedule = grant.schedule;
		let mut next = Instant::now() + schedule.cadence;
		let mut deadline = next + schedule.staleness;
		let mut backoff = Revalidator::BACKOFF;

		loop {
			tokio::time::sleep_until(next).await;

			// Bound the attempt by the deadline so a peer that accepts a request and
			// then stalls cannot carry a revoked session past its window - but never
			// give it less than one request timeout. A zero or very short window
			// would otherwise cancel the very re-check that was about to RENEW the
			// grant, closing every session without the endpoint ever being asked.
			let budget = deadline.max(Instant::now() + crate::http_client::REQUEST_TIMEOUT);
			let outcome = match tokio::time::timeout_at(budget, self.recheck(grant)).await {
				Ok(outcome) => outcome,
				Err(_) => return Expired::Stale,
			};
			match outcome {
				Recheck::Valid { hints } => {
					// A reply that stops naming `max-age` keeps the schedule the session
					// already opted into, rather than silently becoming unrevocable.
					schedule = hints.schedule().unwrap_or(schedule);
					next = Instant::now() + schedule.cadence;
					deadline = next + schedule.staleness;
					backoff = Revalidator::BACKOFF;
				}
				Recheck::Revoked => return Expired::Revoked,
				Recheck::Unavailable => {
					let now = Instant::now();
					if now >= deadline {
						return Expired::Stale;
					}
					// Retries are capped at the deadline; an attempt scheduled there is
					// cut off by `timeout_at` above rather than running past it.
					let delay = backoff.mul_f64(0.5 + rand::rng().random::<f64>() / 2.0);
					next = (now + delay).min(deadline);
					backoff = (backoff * 2).min(schedule.cadence);
				}
			}
		}
	}

	/// One re-check, joining the in-flight request for the same grant if there is one.
	async fn recheck(&self, grant: &Revalidate) -> Recheck {
		match self.fetch_shared(grant).await {
			// The reply is shared; the verdict is this session's alone.
			Fetched::Ok { resp, hints } => match self.authorize(&grant.params, &resp) {
				Ok(token) if grant.scope.covered_by(&token) => Recheck::Valid { hints },
				Ok(_) => Recheck::Revoked,
				Err(err) if err.is_refusal() => Recheck::Revoked,
				Err(_) => Recheck::Unavailable,
			},
			Fetched::Refused => Recheck::Revoked,
			Fetched::Unavailable => Recheck::Unavailable,
		}
	}

	/// The shared auth-API fetch, joining an in-flight one for the same request.
	async fn fetch_shared(&self, grant: &Revalidate) -> Fetched {
		let Some(key) = self.flight_key(grant) else {
			return Fetched::Unavailable;
		};
		let revalidator = &grant.api.revalidator;
		let flight = {
			let mut flights = revalidator.flights.lock().unwrap();
			if let Some(flight) = flights.get(&key).and_then(|slot| slot.flight.upgrade()) {
				flight
			} else {
				let id = revalidator
					.next_flight
					.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
				let guard = FlightGuard {
					revalidator: revalidator.clone(),
					key: key.clone(),
					id,
				};
				let owned = grant.clone();
				let auth = self.clone();
				let flight = async move {
					let _guard = guard;
					auth.recheck_fetch(&owned).await
				}
				.boxed()
				.shared();
				let weak = flight.downgrade().expect("a fresh shared future can be downgraded");
				flights.insert(key, FlightSlot { id, flight: weak });
				flight
			}
		};
		flight.await
	}

	/// Replay the admission request. The reply is shared; what it MEANS is not.
	///
	/// Asking the SAME question admission asked is the whole point. A narrower
	/// check ("does a key still exist for this kid?") cannot see a key REPLACED
	/// under that kid, and cannot see an anonymous grant withdrawn.
	async fn recheck_fetch(&self, grant: &Revalidate) -> Fetched {
		let request = match self.api_request(&grant.params) {
			Ok(request) => request,
			// The credential parsed at admission, so this cannot be transient.
			Err(_) => return Fetched::Refused,
		};
		match grant.api.fetch(&request).await {
			Ok((resp, hints)) => Fetched::Ok {
				resp: Arc::new(resp),
				hints,
			},
			Err(err) if err.is_refusal() => Fetched::Refused,
			Err(_) => Fetched::Unavailable,
		}
	}

	fn build_client(tls: &rustls::ClientConfig) -> anyhow::Result<ClientWithMiddleware> {
		crate::http_client::build(tls)
	}
}

/// What the auth API said about how long its answer is good for.
///
/// This IS the revalidation policy. There is no relay configuration for any of
/// it: the endpoint decides per response, so it can hand a project nowhere near
/// its limits a long window and one close to them a short one, with no fleet
/// roll. The relay only imposes guardrails so a pathological value cannot make
/// it poll hot or serve forever.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct CacheHints {
	/// `max-age`: how long this answer is fresh. Also the re-check cadence, and
	/// therefore what bounds how long a revoked grant keeps serving.
	max_age: Option<Duration>,
	/// `stale-if-error`: how long to keep serving when revalidation ERRORS. The
	/// precise license for what this loop does, so it wins over the broader
	/// `stale-while-revalidate` when both are present.
	stale_if_error: Option<Duration>,
	/// `stale-while-revalidate`: the broader "serve stale while refreshing"
	/// license, used as the outage window when `stale-if-error` is absent.
	stale_while_revalidate: Option<Duration>,
}

impl CacheHints {
	/// Floor on the re-check cadence.
	const MIN_CADENCE: Duration = Duration::from_secs(1);

	/// Upper bound on any timing the endpoint asks for.
	///
	/// NOT a policy ceiling: the endpoint owns the window, so a long `max-age` is
	/// a long revocation window by its explicit choice, and the relay does not
	/// second-guess it. This exists so `Instant` arithmetic cannot overflow on a
	/// `max-age` near `u64::MAX`, and is set far beyond any duration a real
	/// endpoint would send.
	const MAX_TIMING: Duration = Duration::from_secs(10 * 365 * 24 * 60 * 60);

	/// Outage tolerance when the endpoint names neither stale directive.
	///
	/// Deliberately generous, and deliberately not a multiple of the cadence: a
	/// short cadence is a request for a tight REVOCATION window, not permission to
	/// sever every live session over a brief auth outage. At a 60s cadence a
	/// proportional default would drop the fleet after three minutes, which a
	/// routine Worker deploy or a transient incident can exceed. An hour of the
	/// auth API being unreachable is an outage worth surviving; past that,
	/// failing closed is the right call. An endpoint that wants less says so with
	/// `stale-if-error`.
	const DEFAULT_STALE: Duration = Duration::from_secs(60 * 60);

	fn from_headers(headers: &http::HeaderMap) -> Self {
		Self {
			max_age: Self::directive(headers, "max-age"),
			stale_if_error: Self::directive(headers, "stale-if-error"),
			stale_while_revalidate: Self::directive(headers, "stale-while-revalidate"),
		}
	}

	/// The schedule these hints ask for, or `None` when the endpoint named no
	/// `max-age` and so did not opt in to revalidation at all.
	///
	/// Revalidation is opt-in BY THE ENDPOINT rather than by relay config, and
	/// `max-age` is the opt-in: an endpoint that will not say how long its answer
	/// is good for has not given the relay a cadence to invent. `no-store`,
	/// `no-cache`, and `max-age=0` all carry no usable interval and mean the same
	/// thing here. The session then ends only at its credential's own `exp`,
	/// exactly as it did before revalidation existed.
	fn schedule(&self) -> Option<Schedule> {
		let cadence = self
			.max_age
			.filter(|max_age| !max_age.is_zero())?
			.clamp(Self::MIN_CADENCE, Self::MAX_TIMING);

		// `stale-if-error` is the precise license for "revalidation is failing";
		// `stale-while-revalidate` is the broader one. Either grants the window,
		// the specific one wins, and absent both the cadence implies it.
		let staleness = self
			.stale_if_error
			.or(self.stale_while_revalidate)
			.unwrap_or(Self::DEFAULT_STALE)
			.min(Self::MAX_TIMING);

		Some(Schedule { cadence, staleness })
	}

	/// The seconds value of one `Cache-Control` directive.
	fn directive(headers: &http::HeaderMap, name: &str) -> Option<Duration> {
		// Repeated Cache-Control field lines are valid and combine, so read them
		// all rather than only the first.
		headers
			.get_all(http::header::CACHE_CONTROL)
			.iter()
			.filter_map(|value| value.to_str().ok())
			.flat_map(|value| value.split(','))
			.find_map(|directive| {
				let (found, secs) = directive.trim().split_once('=')?;
				if !found.eq_ignore_ascii_case(name) {
					return None;
				}
				let secs: u64 = secs.trim().trim_matches('"').parse().ok()?;
				Some(Duration::from_secs(secs))
			})
	}
}

/// When a live session re-checks, and how long it may keep serving once those
/// re-checks start failing.
///
/// `staleness` runs from where freshness ENDS, not from the last success, which
/// is what the stale directives mean. Measuring from the last success would make
/// an ordinary `max-age=300, stale-while-revalidate=60` expire its deadline four
/// minutes before the first re-check even ran, so one transient 500 would
/// disconnect every affected session at once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Schedule {
	cadence: Duration,
	staleness: Duration,
}

#[cfg(test)]
mod tests {
	/// A dependency that reports only a category in `Display` and keeps the real cause in
	/// `source()` (reqwest, whose DNS/TLS detail lives there) must not lose it on conversion.
	#[test]
	fn message_flattens_the_source_chain() {
		#[derive(Debug, thiserror::Error)]
		#[error("inner")]
		struct Inner;

		#[derive(Debug, thiserror::Error)]
		#[error("outer")]
		struct Outer(#[source] Inner);

		assert_eq!(super::message(Outer(Inner)), "outer: inner");
	}

	use super::*;
	use moq_token::{Algorithm, Key, KeyId};
	use tempfile::TempDir;

	#[test]
	fn auth_params_from_path() {
		// Path + JWT (the gateway media uplink shape).
		let p = AuthParams::from_path_query("/customer/foo/bar", Some("jwt=xd"));
		assert_eq!(p.path, "/customer/foo/bar");
		assert_eq!(p.jwt.as_deref(), Some("xd"));

		// Path only (tokenless public playback).
		let p = AuthParams::from_path_query("/customer/foo/bar", None);
		assert_eq!(p.path, "/customer/foo/bar");
		assert_eq!(p.jwt, None);

		// Missing and root paths share the public empty representation, then use the
		// canonical URL root for authentication.
		let p = AuthParams::from_path_query("", None);
		assert_eq!(p.path, "/");
		assert_eq!(p.jwt, None);

		// An empty jwt value counts as absent.
		let p = AuthParams::from_path_query("/foo", Some("jwt="));
		assert_eq!(p.jwt, None);

		// The jwt may sit among other query params and be URL-encoded.
		let p = AuthParams::from_path_query("/foo", Some("a=1&jwt=ab%20cd"));
		assert_eq!(p.path, "/foo");
		assert_eq!(p.jwt.as_deref(), Some("ab cd"));

		// Match URL query parsing when a client supplies duplicate credentials.
		let p = AuthParams::from_path_query("/foo", Some("jwt=first&jwt=second"));
		assert_eq!(p.jwt.as_deref(), Some("second"));
		let p = parse("https://example.com/foo?jwt=first&jwt=second", &[]);
		assert_eq!(p.jwt.as_deref(), Some("second"));
	}

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

		let claims = moq_token::Claims::default()
			.with_root("room/123")
			.with_subscribe([""])
			.with_publish(["alice"]);
		let token = key.sign(&claims)?;

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
		let claims = moq_token::Claims::default()
			.with_root("room/123")
			.with_subscribe([""])
			.with_publish(["alice"])
			.with_expires(expires);
		let token = key.sign(&claims)?;

		let token = auth
			.verify(&AuthParams {
				path: "/room/123".into(),
				jwt: Some(token),
				..Default::default()
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

		let claims = moq_token::Claims::default()
			.with_root("room/123")
			.with_subscribe([""])
			.with_publish([""]);
		let token = key.sign(&claims)?;

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

		let claims = moq_token::Claims::default()
			.with_root("room/123")
			.with_subscribe(["bob"])
			.with_publish(["alice"]);
		let token = key.sign(&claims)?;

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

		let claims = moq_token::Claims::default().with_root("room/123").with_subscribe([""]);
		let token = key.sign(&claims)?;

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

		let claims = moq_token::Claims::default().with_root("room/123").with_publish(["bob"]);
		let token = key.sign(&claims)?;

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

		let claims = moq_token::Claims::default()
			.with_root("room/123")
			.with_subscribe([""])
			.with_publish([""]);
		let token = key.sign(&claims)?;

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
	async fn test_claims_reduction_with_publisher_restrictions() -> anyhow::Result<()> {
		let key = create_test_key_with_kid("test-key");
		let dir = setup_key_dir(&[("test-key", &key)]);

		let auth = Auth::new(AuthConfig {
			key_dir: Some(dir.path().to_string_lossy().to_string()),
			..Default::default()
		})
		.await?;

		let claims = moq_token::Claims::default()
			.with_root("room/123")
			.with_subscribe([""])
			.with_publish(["alice"]);
		let token = key.sign(&claims)?;

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

		let claims = moq_token::Claims::default()
			.with_root("room/123")
			.with_subscribe(["bob"])
			.with_publish([""]);
		let token = key.sign(&claims)?;

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

		let claims = moq_token::Claims::default()
			.with_root("room/123")
			.with_subscribe(["bob"])
			.with_publish(["alice"]);
		let token = key.sign(&claims)?;

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

		let claims = moq_token::Claims::default()
			.with_root("room/123")
			.with_subscribe(["users/bob/screen"])
			.with_publish(["users/alice/camera"]);
		let token = key.sign(&claims)?;

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

		let claims = moq_token::Claims::default()
			.with_root("room/123")
			.with_subscribe(["alice"]);
		let token = key.sign(&claims)?;

		let verified = auth
			.verify(&AuthParams {
				path: "/room/123/alice".into(),
				jwt: Some(token),
				..Default::default()
			})
			.await?;

		assert_eq!(verified.subscribe, vec!["".as_path()]);
		assert_eq!(verified.publish, vec![]);

		let claims = moq_token::Claims::default()
			.with_root("room/123")
			.with_publish(["alice"]);
		let token = key.sign(&claims)?;

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
		let claims = moq_token::Claims::default().with_root("test").with_subscribe([""]);
		let token = key.sign(&claims)?;

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
		let claims = moq_token::Claims::default().with_root("room/1").with_subscribe([""]);
		let token1 = key1.sign(&claims)?;

		let verified = auth
			.verify(&AuthParams {
				path: "/room/1".into(),
				jwt: Some(token1),
				..Default::default()
			})
			.await?;
		assert_eq!(verified.root, "room/1".as_path());

		// Sign with key-2
		let claims = moq_token::Claims::default().with_root("room/2").with_subscribe([""]);
		let token2 = key2.sign(&claims)?;

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

		let claims = moq_token::Claims::default().with_root("test").with_subscribe([""]);
		let token = key.sign(&claims)?;

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
		let claims = moq_token::Claims::default()
			.with_root("secret")
			.with_subscribe([""])
			.with_publish(["alice"]);
		let jwt = key.sign(&claims)?;

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
		let claims = moq_token::Claims::default()
			.with_root("demo")
			.with_subscribe([""])
			.with_publish(["alice"]);
		let token = key.sign(&claims)?;

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
		let claims = moq_token::Claims::default()
			.with_root("room/123")
			.with_subscribe([""])
			.with_publish(["alice"]);
		let token = key.sign(&claims)?;

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
		let claims = moq_token::Claims::default()
			.with_root("demo")
			.with_subscribe([""])
			.with_publish([""]);
		let token = key.sign(&claims)?;

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
		let claims = moq_token::Claims::default().with_root("").with_subscribe(["demo"]);
		let token = key.sign(&claims)?;

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
	fn test_usage_public_from_str() {
		let config: PublicConfig = "anon".parse().unwrap();
		let d = config.into_detailed();
		assert_eq!(d.subscribe, vec!["anon"]);
		assert_eq!(d.publish, vec!["anon"]);
	}

	#[test]
	fn test_usage_public_url_from_str() {
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

		let claims = moq_token::Claims::default().with_root("room/1").with_subscribe([""]);
		let token = key.sign(&claims)?;

		let verified = auth
			.verify(&AuthParams {
				path: "/room/1".into(),
				jwt: Some(token),
				..Default::default()
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

		let claims = moq_token::Claims::default().with_root("room/1").with_subscribe([""]);
		let token = key.sign(&claims)?;
		let result = auth
			.verify(&AuthParams {
				path: "/room/1".into(),
				jwt: Some(token),
				..Default::default()
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

		let claims = moq_token::Claims::default().with_root("room/1").with_subscribe([""]);
		let token = key.sign(&claims)?;
		let result = auth
			.verify(&AuthParams {
				path: "/room/1".into(),
				jwt: Some(token),
				..Default::default()
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
		let claims = moq_token::Claims::default().with_root("room/1").with_subscribe([""]);
		let token = key.sign(&claims)?;
		let result = auth
			.verify(&AuthParams {
				path: "/room/1".into(),
				jwt: Some(token),
				..Default::default()
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

		let claims = moq_token::Claims::default().with_root("room/1").with_subscribe([""]);
		let token = key.sign(&claims)?;
		let result = auth
			.verify(&AuthParams {
				path: "/room/1".into(),
				jwt: Some(token),
				..Default::default()
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

		let claims = moq_token::Claims::default().with_root("room/1").with_subscribe([""]);
		let token = key.sign(&claims)?;

		for _ in 0..2 {
			auth.verify(&AuthParams {
				path: "/room/1".into(),
				jwt: Some(token.clone()),
				..Default::default()
			})
			.await?;
		}
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

		let claims = moq_token::Claims::default().with_root("room/1").with_subscribe([""]);
		let token = fx.key.sign(&claims)?;
		let verified = auth_with_identity
			.verify(&AuthParams {
				path: "/room/1".into(),
				jwt: Some(token.clone()),
				..Default::default()
			})
			.await?;
		assert_eq!(verified.root, "room/1".as_path());

		// New path: the identity is supplied via the shared --client-tls-* config
		// (injected through AuthConfig::init) instead of the deprecated
		// --auth-tls-* flags. The server accepts it the same way.
		let mut client_tls = moq_tokio::tls::Connect::default();
		client_tls.root = vec![fx.ca_pem_path.clone()];
		client_tls.cert = Some(fx.client_cert_path.clone());
		client_tls.key = Some(fx.client_key_path.clone());
		let auth_via_client_tls = AuthConfig {
			key_dir: Some(format!("{}/keys/", fx.base_url)),
			..Default::default()
		}
		.init(&client_tls)
		.await?;
		let verified = auth_via_client_tls
			.verify(&AuthParams {
				path: "/room/1".into(),
				jwt: Some(token.clone()),
				..Default::default()
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
				..Default::default()
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
		// JWT connection: the token root is the vanity path the client dialed
		// ("demo/room"); the API resolves that to the canonical alias
		// ("x7k2qp/room"), and the verified token anchors on the alias so the
		// backbone uses the stable pid.
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

		let claims = moq_token::Claims::default().with_root("demo/room").with_subscribe([""]);
		let token = key.sign(&claims)?;

		let verified = auth
			.verify(&AuthParams {
				path: "/demo/room".into(),
				jwt: Some(token),
				..Default::default()
			})
			.await?;
		assert_eq!(verified.root, "x7k2qp/room".as_path());
		assert_eq!(verified.subscribe, vec!["".as_path()]);
		Ok(())
	}

	#[tokio::test]
	async fn auth_api_full_root_passthrough() -> anyhow::Result<()> {
		// A vanity parent token ("demo") connecting to a deep path
		// ("demo/room/cam"): the token root overlaps the connection path, and the
		// verified root is anchored on the FULL resolved alias ("x7k2qp/room/cam").
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
		let claims = moq_token::Claims::default().with_root("demo").with_subscribe([""]);
		let token = key.sign(&claims)?;

		let verified = auth
			.verify(&AuthParams {
				path: "/demo/room/cam".into(),
				jwt: Some(token),
				..Default::default()
			})
			.await?;
		assert_eq!(verified.root, "x7k2qp/room/cam".as_path());
		Ok(())
	}

	#[tokio::test]
	async fn auth_api_jwt_vanity_root_scopes_to_pid() -> anyhow::Result<()> {
		// Regression for the dashboard flow: a token minted with the vanity
		// project name as its root ("kixelated") connecting to the vanity
		// subdomain path. The API aliases the name to the pid ("uwwdyw61"); the
		// token verifies against the vanity path and the scope anchors on the pid,
		// so publishing "hello-world" lands at "uwwdyw61/hello-world".
		let server = MockServer::start().await;
		let key = create_test_key_with_kid("test-key");

		Mock::given(method("GET"))
			.and(path_matcher("/auth"))
			.and(query_param("root", "kixelated"))
			.respond_with(
				ResponseTemplate::new(200)
					.set_body_string(format!(r#"{{"alias":"uwwdyw61","key":{}}}"#, jwk_body(&key))),
			)
			.mount(&server)
			.await;

		let auth = auth_with_api(&server).await;

		let claims = moq_token::Claims::default()
			.with_root("kixelated")
			.with_publish(["hello-world"])
			.with_subscribe(["hello-world"]);
		let token = key.sign(&claims)?;

		let verified = auth
			.verify(&AuthParams {
				path: "/kixelated".into(),
				jwt: Some(token),
				..Default::default()
			})
			.await?;
		assert_eq!(verified.root, "uwwdyw61".as_path());
		assert_eq!(verified.publish, vec!["hello-world".as_path()]);
		assert_eq!(verified.subscribe, vec!["hello-world".as_path()]);
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
	async fn auth_api_alias_depth_mismatch_fails_closed() -> anyhow::Result<()> {
		let server = MockServer::start().await;
		Mock::given(method("GET"))
			.and(path_matcher("/auth"))
			.and(query_param("root", "demo/room"))
			.respond_with(
				ResponseTemplate::new(200)
					.set_body_string(r#"{"alias":"x7k2qp/room/extra","public":{"subscribe":[""]}}"#),
			)
			.mount(&server)
			.await;

		let auth = auth_with_api(&server).await;
		let result = auth.verify(&AuthParams::new("/demo/room")).await;
		assert!(matches!(result, Err(AuthError::IncorrectRoot)));
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
					.set_body_string(r#"{"alias":"x7k2qp","public":{"subscribe":[""]},"tier":"region/sjc"}"#),
			)
			.mount(&server)
			.await;

		let auth = auth_with_api(&server).await;
		let verified = auth.verify(&AuthParams::new("/demo")).await?;
		assert_eq!(verified.tier, Tier::new("region/sjc"));
		Ok(())
	}

	#[tokio::test]
	async fn auth_api_forwards_transport_for_tiering() -> anyhow::Result<()> {
		// The relay forwards the connection transport as `transport=`, so the API can
		// bucket by connection type -- e.g. tier traffic on the internal Unix-socket
		// listener (the RTMP/SRT/WebRTC gateways) into its own billing tier. The mock
		// REQUIRES the param, so a missing `transport` fails the match (404 -> closed).
		let server = MockServer::start().await;
		Mock::given(method("GET"))
			.and(path_matcher("/auth"))
			.and(query_param("root", "customer/live"))
			.and(query_param("transport", "unix"))
			.respond_with(
				ResponseTemplate::new(200).set_body_string(r#"{"public":{"subscribe":[""]},"tier":"gateway"}"#),
			)
			.mount(&server)
			.await;

		let auth = auth_with_api(&server).await;
		let params = AuthParams {
			path: "/customer/live".into(),
			transport: Some(Transport::Unix),
			..Default::default()
		};
		let verified = auth.verify(&params).await?;
		assert_eq!(verified.tier, Tier::new("gateway"));
		Ok(())
	}

	#[test]
	fn auth_api_response_tier() {
		let named: AuthApiResponse = serde_json::from_str(r#"{"tier":"region/sjc"}"#).unwrap();
		assert_eq!(named.tier(), Some(Tier::new("region/sjc")));

		let default: AuthApiResponse = serde_json::from_str(r#"{"tier":""}"#).unwrap();
		assert_eq!(default.tier(), Some(Tier::default()));

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
		let claims = moq_token::Claims::default().with_root("unknown").with_subscribe([""]);
		let token = key.sign(&claims)?;
		let verified = auth
			.verify(&AuthParams {
				path: "/unknown".into(),
				jwt: Some(token),
				..Default::default()
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
		let token = key.sign(&moq_token::Claims::default().with_root("x7k2qp").with_subscribe([""]))?;
		let result = auth
			.verify(&AuthParams {
				path: "/demo".into(),
				jwt: Some(token),
				..Default::default()
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
		// mTLS peers get the canonical root + tier; absent `tier` uses the
		// configured default.
		let server = MockServer::start().await;
		Mock::given(method("GET"))
			.and(path_matcher("/auth"))
			.and(query_param("root", "demo/room"))
			.respond_with(ResponseTemplate::new(200).set_body_string(r#"{"alias":"x7k2qp/room"}"#))
			.mount(&server)
			.await;

		let auth = auth_with_api(&server).await;
		assert_eq!(
			auth.resolve_mtls("/demo/room", None).await?,
			("x7k2qp/room".to_string(), Tier::default())
		);
		Ok(())
	}

	#[tokio::test]
	async fn auth_api_mtls_tier_override_default() -> anyhow::Result<()> {
		// The API can move a cert-verified connection to a named tier.
		let server = MockServer::start().await;
		Mock::given(method("GET"))
			.and(path_matcher("/auth"))
			.and(query_param("root", "demo"))
			.respond_with(ResponseTemplate::new(200).set_body_string(r#"{"alias":"x7k2qp","tier":"region"}"#))
			.mount(&server)
			.await;

		let auth = auth_with_api(&server).await;
		assert_eq!(
			auth.resolve_mtls("/demo", None).await?,
			("x7k2qp".to_string(), Tier::new("region"))
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
			auth.resolve_mtls("/", None).await?,
			("x7k2qp".to_string(), Tier::new("region"))
		);
		Ok(())
	}

	#[tokio::test]
	async fn auth_api_mtls_no_api_fails_open() -> anyhow::Result<()> {
		// With no auth API configured the cert is the only credential: use the path
		// and configured tier unchanged. This is the sole fail-open case. (A public
		// path just makes the config valid; mTLS resolution ignores it.)
		let auth = Auth::new(AuthConfig {
			public: simple_public("anon"),
			..Default::default()
		})
		.await?;
		assert_eq!(
			auth.resolve_mtls("/demo", None).await?,
			("/demo".to_string(), Tier::default())
		);
		assert_eq!(auth.resolve_mtls("/", None).await?, ("/".to_string(), Tier::default()));
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
		let err = auth.resolve_mtls("/demo", None).await.unwrap_err();
		assert!(matches!(err, AuthError::NotFound));
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
		let err = auth.resolve_mtls("/demo", None).await.unwrap_err();
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

	/// The endpoint `auth` was built with. Admission carries this onto every grant
	/// it mints, so a hand-built one has to name it too.
	fn test_api(auth: &Auth) -> AuthApi {
		auth.auth_api.clone().expect("test grants need an auth API")
	}

	/// A grant as admission would mint it, on a short cadence. The empty scope
	/// makes `covered_by` a root check, which is what the loop tests care about;
	/// the scope comparison itself is covered by its own tests below.
	fn test_grant(auth: &Auth, jwt: Option<String>, after: Duration) -> Revalidate {
		test_grant_schedule(
			auth,
			jwt,
			Schedule {
				cadence: after,
				// Deliberately generous, and deliberately not `3 * cadence`: these
				// cadences are far below `MIN_CADENCE` so the tests run fast, and a
				// proportional window would then cut a mock's response short at the
				// staleness deadline. Tests that are ABOUT the window set it.
				staleness: Duration::from_secs(5),
			},
		)
	}

	/// The same, with the resolved schedule spelled out.
	fn test_grant_schedule(auth: &Auth, jwt: Option<String>, schedule: Schedule) -> Revalidate {
		Revalidate {
			api: test_api(auth),
			params: Arc::new(AuthParams {
				path: "demo".into(),
				jwt,
				..Default::default()
			}),
			scope: Scope {
				root: Path::new("demo").to_owned(),
				subscribe: PathPrefixes::default(),
				publish: PathPrefixes::default(),
			},
			schedule,
		}
	}

	/// Mount a `/auth` responder returning `body` with the given `Cache-Control`.
	async fn mount_auth(server: &MockServer, cache_control: &str, body: String) {
		Mock::given(method("GET"))
			.and(path_matcher("/auth"))
			.respond_with(
				ResponseTemplate::new(200)
					.insert_header("Cache-Control", cache_control)
					.set_body_string(body),
			)
			.mount(server)
			.await;
	}

	fn hints(value: &str) -> CacheHints {
		let mut headers = http::HeaderMap::new();
		headers.insert(http::header::CACHE_CONTROL, value.parse().unwrap());
		CacheHints::from_headers(&headers)
	}

	#[test]
	fn cache_control_directive_parsing() {
		assert_eq!(hints("max-age=300").max_age, Some(Duration::from_secs(300)));
		assert_eq!(
			hints("public, max-age=60, must-revalidate").max_age,
			Some(Duration::from_secs(60))
		);
		assert_eq!(hints("Max-Age=\"60\"").max_age, Some(Duration::from_secs(60)));
		assert_eq!(hints("no-store").max_age, None);
		assert_eq!(hints("max-age=oops").max_age, None);
		assert_eq!(CacheHints::from_headers(&http::HeaderMap::new()), CacheHints::default());

		// The shared prefix must not let one stale directive match the other.
		let both = hints("max-age=60, stale-while-revalidate=300, stale-if-error=900");
		assert_eq!(both.stale_while_revalidate, Some(Duration::from_secs(300)));
		assert_eq!(both.stale_if_error, Some(Duration::from_secs(900)));
	}

	/// Revalidation is opt-in by the ENDPOINT, and `max-age` is the opt-in. A
	/// reply that names no usable interval is not asking to be re-consulted, and
	/// the relay does not invent a cadence for it.
	#[test]
	fn no_max_age_means_no_schedule() {
		assert_eq!(CacheHints::default().schedule(), None);
		assert_eq!(hints("no-store").schedule(), None);
		assert_eq!(hints("no-cache").schedule(), None);
		assert_eq!(hints("max-age=0").schedule(), None);
		// A stale directive alone does not opt in either: there is no cadence.
		assert_eq!(hints("stale-if-error=900").schedule(), None);
	}

	#[test]
	fn schedule_clamps_the_cadence() {
		assert_eq!(
			hints("max-age=300").schedule().unwrap().cadence,
			Duration::from_secs(300)
		);
		assert_eq!(hints("max-age=1").schedule().unwrap().cadence, CacheHints::MIN_CADENCE);
		assert_eq!(
			hints(&format!("max-age={}", u64::MAX)).schedule().unwrap().cadence,
			CacheHints::MAX_TIMING
		);
	}

	/// `stale-if-error` is the precise license for "revalidation is failing", so
	/// it wins over the broader `stale-while-revalidate`; either grants the
	/// window, and absent both the cadence implies it.
	#[test]
	fn schedule_prefers_stale_if_error() {
		assert_eq!(
			hints("max-age=10").schedule().unwrap().staleness,
			CacheHints::DEFAULT_STALE
		);
		assert_eq!(
			hints("max-age=10, stale-while-revalidate=300")
				.schedule()
				.unwrap()
				.staleness,
			Duration::from_secs(300)
		);
		assert_eq!(
			hints("max-age=10, stale-if-error=900").schedule().unwrap().staleness,
			Duration::from_secs(900)
		);
		assert_eq!(
			hints("max-age=10, stale-while-revalidate=300, stale-if-error=60")
				.schedule()
				.unwrap()
				.staleness,
			Duration::from_secs(60)
		);
		// Capped, both as policy and to keep the deadline off `Instant` overflow.
		assert_eq!(
			hints(&format!("max-age=10, stale-if-error={}", u64::MAX))
				.schedule()
				.unwrap()
				.staleness,
			CacheHints::MAX_TIMING
		);
	}

	/// EVERY auth-API session is revalidated, anonymous ones included. An
	/// anonymous grant has no `exp`, so without this a gated project's tokenless
	/// viewers would keep drawing traffic until the peer hung up.
	#[tokio::test]
	async fn revalidate_arms_every_api_session() -> anyhow::Result<()> {
		let server = MockServer::start().await;
		let key = create_test_key_with_kid("test-key");
		mount_auth(
			&server,
			"max-age=300",
			format!(r#"{{"key":{},"public":{{"subscribe":[""]}}}}"#, jwk_body(&key)),
		)
		.await;

		let auth = auth_with_api(&server).await;
		let jwt = key.sign(&moq_token::Claims::default().with_root("demo").with_subscribe([""]))?;

		let verified = auth
			.verify(&AuthParams {
				path: "/demo".into(),
				jwt: Some(jwt),
				..Default::default()
			})
			.await?;
		let grant = verified.revalidate.expect("jwt session should carry a grant");
		assert_eq!(grant.schedule.cadence, Duration::from_secs(300));

		let anon = auth.verify(&AuthParams::new("/demo")).await?;
		assert!(anon.revalidate.is_some(), "anonymous sessions must revalidate too");

		// mTLS is the one exemption: a customer decision must never tear down the
		// relay mesh.
		let peer = auth.verify_mtls("/demo", None).await?;
		assert!(peer.revalidate.is_none(), "mTLS peers must never revalidate");
		Ok(())
	}

	/// An endpoint that names no `max-age` has not opted in, so the session gets
	/// no re-check at all and its credential's `exp` stays the only bound. This
	/// is what makes the upgrade inert for an operator who was not caching.
	#[tokio::test]
	async fn no_max_age_arms_no_revalidation() -> anyhow::Result<()> {
		let server = MockServer::start().await;
		Mock::given(method("GET"))
			.and(path_matcher("/auth"))
			.respond_with(
				ResponseTemplate::new(200)
					.insert_header("Cache-Control", "no-store")
					.set_body_string(r#"{"public":{"subscribe":[""]}}"#),
			)
			.mount(&server)
			.await;

		let auth = auth_with_api(&server).await;
		let token = auth.verify(&AuthParams::new("/demo")).await?;
		assert!(token.revalidate.is_none(), "no max-age means no opt-in");

		// And so `expired` never resolves for a credential with no `exp` either.
		let bounded = tokio::time::timeout(Duration::from_millis(200), auth.expired(&token)).await;
		assert!(bounded.is_err(), "an un-opted-in session has no bound to hit");
		Ok(())
	}

	/// A cached reply the middleware served only because it could NOT reach the
	/// origin (`Warning: 111`) is evidence of nothing. Accepting it would reset
	/// the staleness deadline every cadence, so a sustained outage would keep a
	/// revoked session serving forever and the window would never fail closed.
	#[test]
	fn stale_cached_replies_are_not_successes() {
		let warned = |value: &str| {
			let mut headers = http::HeaderMap::new();
			headers.insert(http::header::WARNING, value.parse().unwrap());
			AuthApi::revalidation_failed(&headers)
		};
		assert!(warned(r#"111 localhost "Revalidation failed""#));
		assert!(!warned(r#"110 localhost "Response is stale""#));
		assert!(!AuthApi::revalidation_failed(&http::HeaderMap::new()));
		// And it must classify as unavailable, never as a revocation - nor, at
		// admission, as the client's credential being rejected.
		assert!(!AuthError::ApiStale.is_refusal());
		assert_eq!(
			http::StatusCode::from(&AuthError::ApiStale),
			http::StatusCode::BAD_GATEWAY
		);
	}

	/// An auth API that goes down must not sever the fleet. With no stale
	/// directive the session rides out a total outage for the default hour, which
	/// is the whole point of the default being flat rather than a small multiple
	/// of a possibly-short cadence.
	#[tokio::test(start_paused = true)]
	async fn a_total_outage_is_survived_for_the_default_hour() -> anyhow::Result<()> {
		let server = MockServer::start().await;
		// Admission succeeds and opts in; every later request fails.
		Mock::given(method("GET"))
			.and(path_matcher("/auth"))
			.respond_with(ResponseTemplate::new(200).insert_header("Cache-Control", "max-age=60"))
			.up_to_n_times(1)
			.mount(&server)
			.await;
		Mock::given(method("GET"))
			.and(path_matcher("/auth"))
			.respond_with(ResponseTemplate::new(503))
			.mount(&server)
			.await;

		let auth = auth_with_api(&server).await;
		let schedule = CacheHints {
			max_age: Some(Duration::from_secs(60)),
			..Default::default()
		}
		.schedule()
		.expect("max-age opts in");
		assert_eq!(schedule.staleness, CacheHints::DEFAULT_STALE);

		let grant = test_grant_schedule(&auth, None, schedule);
		// Half an hour of total outage: still serving.
		let alive = tokio::time::timeout(Duration::from_secs(30 * 60), auth.revalidate(&grant)).await;
		assert!(alive.is_err(), "a 30 minute outage must not disconnect anyone");

		// Past the window it still fails closed rather than serving forever.
		let closed = tokio::time::timeout(Duration::from_secs(2 * 60 * 60), auth.revalidate(&grant))
			.await
			.expect("a sustained outage must eventually close");
		assert_eq!(closed, Expired::Stale);
		Ok(())
	}

	/// The endpoint owns the window, so an enormous `max-age` is honoured rather
	/// than second-guessed - and must not overflow the deadline arithmetic that
	/// `Instant` would panic on.
	#[tokio::test]
	async fn an_enormous_max_age_is_honoured_without_overflowing() -> anyhow::Result<()> {
		let server = MockServer::start().await;
		mount_auth(
			&server,
			&format!("max-age={}, stale-if-error={}", u64::MAX, u64::MAX),
			r#"{"public":{"subscribe":[""]}}"#.to_string(),
		)
		.await;

		let auth = auth_with_api(&server).await;
		let token = auth.verify(&AuthParams::new("demo")).await?;
		let grant = token.revalidate.clone().expect("a max-age opts in");
		assert_eq!(grant.schedule.cadence, CacheHints::MAX_TIMING);
		assert_eq!(grant.schedule.staleness, CacheHints::MAX_TIMING);

		// Arming the loop computes `now + cadence` and `next + staleness`; both must
		// be representable rather than panicking.
		let pending = tokio::time::timeout(Duration::from_millis(200), auth.revalidate(&grant)).await;
		assert!(pending.is_err(), "a grant this long-lived simply keeps serving");
		Ok(())
	}

	/// A zero stale window means "close on the first FAILED re-check", not "close
	/// without ever asking". The attempt still gets a full request budget, so a
	/// healthy endpoint renews the grant.
	#[tokio::test]
	async fn zero_stale_window_still_lets_a_recheck_succeed() -> anyhow::Result<()> {
		let server = MockServer::start().await;
		mount_auth(
			&server,
			"max-age=1, stale-if-error=0",
			r#"{"public":{"subscribe":[""]}}"#.to_string(),
		)
		.await;

		let auth = auth_with_api(&server).await;
		let grant = test_grant_schedule(
			&auth,
			None,
			Schedule {
				cadence: Duration::from_millis(200),
				staleness: Duration::ZERO,
			},
		);

		// The endpoint is healthy, so the session must survive its re-checks rather
		// than being closed by a deadline that left the request no time to run.
		let pending = tokio::time::timeout(Duration::from_millis(900), auth.revalidate(&grant)).await;
		assert!(pending.is_err(), "a healthy endpoint must renew the grant");
		Ok(())
	}

	#[tokio::test]
	async fn revalidate_closes_on_404() -> anyhow::Result<()> {
		// No mock mounted, so every re-check gets wiremock's 404.
		let server = MockServer::start().await;
		let auth = auth_with_api(&server).await;
		let grant = test_grant(&auth, None, Duration::from_millis(500));

		let start = std::time::Instant::now();
		let reason = tokio::time::timeout(Duration::from_secs(5), auth.revalidate(&grant))
			.await
			.expect("revalidate should return once the grant is gone");
		assert_eq!(reason, Expired::Revoked);
		assert!(start.elapsed() >= Duration::from_millis(500));
		Ok(())
	}

	/// A gated project withholds `public`, which is how an anonymous session is
	/// revoked. `key.is_some()` could never have seen this.
	#[tokio::test]
	async fn revalidate_closes_when_public_is_withdrawn() -> anyhow::Result<()> {
		let server = MockServer::start().await;
		mount_auth(&server, "max-age=1", r#"{"alias":"demo"}"#.to_string()).await;

		let auth = auth_with_api(&server).await;
		let grant = test_grant(&auth, None, Duration::from_millis(200));
		let reason = tokio::time::timeout(Duration::from_secs(5), auth.revalidate(&grant))
			.await
			.expect("a withdrawn public grant must close the session");
		assert_eq!(reason, Expired::Revoked);
		Ok(())
	}

	/// A key DELETED and reimported with different material under the same `kid`
	/// must close the sessions the old key admitted. Replaying the request catches
	/// this because the retained JWT no longer verifies; asking "does a key exist
	/// for this kid" would have said yes.
	#[tokio::test]
	async fn revalidate_closes_on_a_rotated_key() -> anyhow::Result<()> {
		let server = MockServer::start().await;
		let replacement = create_test_key_with_kid("test-key");
		mount_auth(&server, "max-age=1", format!(r#"{{"key":{}}}"#, jwk_body(&replacement))).await;

		// A token signed by the ORIGINAL key, still presenting the same kid.
		let original = create_test_key_with_kid("test-key");
		let jwt = original.sign(&moq_token::Claims::default().with_root("demo").with_subscribe([""]))?;

		let auth = auth_with_api(&server).await;
		let grant = test_grant(&auth, Some(jwt), Duration::from_millis(200));
		let reason = tokio::time::timeout(Duration::from_secs(5), auth.revalidate(&grant))
			.await
			.expect("a replaced key must close the sessions its predecessor admitted");
		assert_eq!(reason, Expired::Revoked);
		Ok(())
	}

	#[test]
	fn scope_is_covered_only_while_authority_holds() {
		let scope = |root: &str, subscribe: Vec<&str>| Scope {
			root: Path::new(root).to_owned(),
			subscribe: PathPrefixes::from(subscribe.iter().map(|p| Path::new(p).to_owned()).collect::<Vec<_>>()),
			publish: PathPrefixes::default(),
		};
		let token = |root: &str, subscribe: Vec<&str>| {
			let s = scope(root, subscribe);
			let mut token = AuthToken::unrestricted(s.root.clone());
			token.subscribe = s.subscribe;
			token.publish = PathPrefixes::default();
			token
		};

		// Unchanged, and widened, both keep serving.
		assert!(scope("demo", vec!["room"]).covered_by(&token("demo", vec!["room"])));
		assert!(scope("demo", vec!["room"]).covered_by(&token("demo", vec![""])));
		// Narrowed, or re-rooted, is a loss of authority.
		assert!(!scope("demo", vec![""]).covered_by(&token("demo", vec!["room"])));
		assert!(!scope("demo", vec!["room"]).covered_by(&token("other", vec!["room"])));
		assert!(!scope("demo", vec!["room"]).covered_by(&token("demo", vec!["lobby"])));
	}

	#[tokio::test]
	async fn revalidate_refreshes_cadence_then_closes() -> anyhow::Result<()> {
		// One 200 with max-age=1, then wiremock's 404 once the mock is consumed.
		let server = MockServer::start().await;
		Mock::given(method("GET"))
			.and(path_matcher("/auth"))
			.respond_with(
				ResponseTemplate::new(200)
					.insert_header("Cache-Control", "max-age=1")
					.set_body_string(r#"{"public":{"subscribe":[""]}}"#),
			)
			.up_to_n_times(1)
			.mount(&server)
			.await;

		let auth = auth_with_api(&server).await;
		let grant = test_grant(&auth, None, Duration::from_millis(500));

		let start = std::time::Instant::now();
		let reason = tokio::time::timeout(Duration::from_secs(10), auth.revalidate(&grant))
			.await
			.expect("revalidate should return once the grant is gone");
		assert_eq!(reason, Expired::Revoked);
		assert!(start.elapsed() >= Duration::from_millis(1500));
		Ok(())
	}

	/// An outage is evidence of nothing: keep serving, then fail closed. The
	/// window is three cadences, so a 500ms cadence closes at ~1.5s.
	#[tokio::test]
	async fn revalidate_survives_outage_until_stale() -> anyhow::Result<()> {
		let server = MockServer::start().await;
		Mock::given(method("GET"))
			.respond_with(ResponseTemplate::new(500))
			.mount(&server)
			.await;

		let auth = auth_with_api(&server).await;
		let grant = test_grant(&auth, None, Duration::from_millis(500));

		let start = std::time::Instant::now();
		let reason = tokio::time::timeout(Duration::from_secs(10), auth.revalidate(&grant))
			.await
			.expect("revalidate should fail closed once the staleness window passes");
		assert_eq!(reason, Expired::Stale);
		let elapsed = start.elapsed();
		assert!(elapsed >= Duration::from_millis(1500), "closed too early: {elapsed:?}");
		assert!(elapsed < Duration::from_secs(6), "closed too late: {elapsed:?}");
		Ok(())
	}

	/// The endpoint can stretch the outage window past the default three cadences
	/// with `stale-while-revalidate`, so how long a session rides out an outage is a
	/// response header rather than relay config.
	#[tokio::test]
	async fn revalidate_honors_stale_while_revalidate() -> anyhow::Result<()> {
		let server = MockServer::start().await;
		Mock::given(method("GET"))
			.respond_with(ResponseTemplate::new(500))
			.mount(&server)
			.await;

		let auth = auth_with_api(&server).await;
		// 200ms cadence would default to a 600ms window; the endpoint asked for 2s.
		let grant = test_grant_schedule(
			&auth,
			None,
			Schedule {
				cadence: Duration::from_millis(200),
				staleness: Duration::from_secs(2),
			},
		);

		let start = std::time::Instant::now();
		let reason = tokio::time::timeout(Duration::from_secs(10), auth.revalidate(&grant))
			.await
			.expect("revalidate should fail closed once the staleness window passes");
		assert_eq!(reason, Expired::Stale);
		let elapsed = start.elapsed();
		assert!(
			elapsed >= Duration::from_secs(2),
			"closed before stale-while-revalidate: {elapsed:?}"
		);
		assert!(elapsed < Duration::from_secs(6), "closed too late: {elapsed:?}");
		Ok(())
	}

	/// A garbage body is unavailable, not revoked: an endpoint answering nonsense
	/// has told us nothing about the grant.
	#[tokio::test]
	async fn revalidate_treats_garbage_as_unavailable() -> anyhow::Result<()> {
		let server = MockServer::start().await;
		mount_auth(&server, "max-age=1", "not json".to_string()).await;

		let auth = auth_with_api(&server).await;
		let grant = test_grant(&auth, None, Duration::from_millis(300));
		let reason = tokio::time::timeout(Duration::from_secs(10), auth.revalidate(&grant))
			.await
			.expect("a garbage body must fail closed only after the staleness window");
		assert_eq!(reason, Expired::Stale);
		Ok(())
	}

	#[tokio::test]
	async fn revalidate_coalesces_rechecks_for_one_grant() -> anyhow::Result<()> {
		// The delay keeps the first flight in the air until the second joins it;
		// expect(1) fails on drop if it dialed again.
		let server = MockServer::start().await;
		Mock::given(method("GET"))
			.and(path_matcher("/auth"))
			.respond_with(ResponseTemplate::new(404).set_delay(Duration::from_millis(300)))
			.expect(1)
			.mount(&server)
			.await;

		let auth = auth_with_api(&server).await;
		let grant = test_grant(&auth, None, Duration::from_millis(100));

		let (a, b) = tokio::join!(auth.revalidate(&grant), auth.revalidate(&grant));
		assert_eq!(a, Expired::Revoked);
		assert_eq!(b, Expired::Revoked);
		// Mock::expect(1) is asserted on drop of the server.
		Ok(())
	}

	#[tokio::test]
	async fn revalidate_does_not_coalesce_across_roots() -> anyhow::Result<()> {
		let server = MockServer::start().await;
		Mock::given(method("GET"))
			.and(path_matcher("/auth"))
			.respond_with(ResponseTemplate::new(404).set_delay(Duration::from_millis(300)))
			.expect(2)
			.mount(&server)
			.await;

		let auth = auth_with_api(&server).await;
		let a = test_grant(&auth, None, Duration::from_millis(100));
		let mut b = a.clone();
		b.params = Arc::new(AuthParams {
			path: "other".into(),
			..Default::default()
		});

		let (a, b) = tokio::join!(auth.revalidate(&a), auth.revalidate(&b));
		assert_eq!(a, Expired::Revoked);
		assert_eq!(b, Expired::Revoked);
		Ok(())
	}

	/// Viewers of one broadcast hold DISTINCT JWTs signed by the SAME key, so the
	/// flight key must be the kid, not the credential. Keying it on the credential
	/// makes an N-viewer broadcast cost N re-checks per cadence instead of one.
	#[tokio::test]
	async fn revalidate_coalesces_across_one_kids_audience() -> anyhow::Result<()> {
		let server = MockServer::start().await;
		Mock::given(method("GET"))
			.and(path_matcher("/auth"))
			.respond_with(ResponseTemplate::new(404).set_delay(Duration::from_millis(300)))
			.expect(1)
			.mount(&server)
			.await;

		let auth = auth_with_api(&server).await;
		let key = create_test_key_with_kid("shared");
		let claims = moq_token::Claims::default().with_root("demo").with_subscribe([""]);
		// Two different tokens, same signing key, so the same kid.
		let a = test_grant(&auth, Some(key.sign(&claims)?), Duration::from_millis(100));
		let b = test_grant(
			&auth,
			Some(key.sign(&claims.clone().with_publish([""]))?),
			Duration::from_millis(100),
		);
		assert_ne!(a.params.jwt, b.params.jwt, "the viewers must hold distinct tokens");

		let (a, b) = tokio::join!(auth.revalidate(&a), auth.revalidate(&b));
		assert_eq!(a, Expired::Revoked);
		assert_eq!(b, Expired::Revoked);
		// Mock::expect(1) is asserted on drop of the server.
		Ok(())
	}

	/// Sessions sharing a flight must NOT share its verdict. Two anonymous
	/// sessions on one path have the same flight key, but can have been admitted
	/// either side of a narrowed `public` block - so the one still covered keeps
	/// serving while the one that is not gets revoked, off the same reply.
	#[tokio::test]
	async fn revalidate_judges_each_waiter_against_its_own_scope() -> anyhow::Result<()> {
		let server = MockServer::start().await;
		// The project has narrowed anonymous access to `room`.
		Mock::given(method("GET"))
			.and(path_matcher("/auth"))
			.respond_with(
				ResponseTemplate::new(200)
					// Long enough that the surviving session does not re-check again
					// inside this test, so `expect(1)` proves both verdicts came from
					// ONE request.
					.insert_header("Cache-Control", "max-age=60")
					.set_body_string(r#"{"public":{"subscribe":["room"]}}"#)
					.set_delay(Duration::from_millis(300)),
			)
			.expect(1)
			.mount(&server)
			.await;

		let auth = auth_with_api(&server).await;
		let scoped = |subscribe: &str| Revalidate {
			api: test_api(&auth),
			params: Arc::new(AuthParams {
				path: "demo".into(),
				..Default::default()
			}),
			scope: Scope {
				root: Path::new("demo").to_owned(),
				subscribe: PathPrefixes::from(vec![Path::new(subscribe).to_owned()]),
				publish: PathPrefixes::default(),
			},
			schedule: Schedule {
				cadence: Duration::from_millis(100),
				// Generous, so the 300ms mock is not cut short by the deadline; this
				// test is about WHOSE scope decides, not about the window.
				staleness: Duration::from_secs(5),
			},
		};

		// Admitted when anonymous access still covered the whole root.
		let broad = scoped("");
		// Admitted after it narrowed.
		let narrow = scoped("room");

		let (broad, narrow) = tokio::join!(
			tokio::time::timeout(Duration::from_secs(5), auth.revalidate(&broad)),
			tokio::time::timeout(Duration::from_millis(1000), auth.revalidate(&narrow)),
		);
		assert_eq!(
			broad.expect("the session the reply no longer covers must close"),
			Expired::Revoked
		);
		assert!(narrow.is_err(), "the still-covered session must keep serving");
		// expect(1): both verdicts came from ONE auth-API request.
		Ok(())
	}

	/// `stale-while-revalidate` is measured from where freshness ends, so a window
	/// SHORTER than the cadence still buys its full outage tolerance. Measuring it
	/// from the last success would expire the deadline before the first re-check
	/// even runs, and one transient 500 would disconnect everybody.
	#[tokio::test]
	async fn revalidate_tolerates_an_outage_with_swr_below_max_age() -> anyhow::Result<()> {
		let server = MockServer::start().await;
		Mock::given(method("GET"))
			.respond_with(ResponseTemplate::new(500))
			.mount(&server)
			.await;

		let auth = auth_with_api(&server).await;
		let grant = test_grant_schedule(
			&auth,
			None,
			Schedule {
				cadence: Duration::from_millis(500),
				staleness: Duration::from_millis(800),
			},
		);

		let start = std::time::Instant::now();
		let reason = tokio::time::timeout(Duration::from_secs(10), auth.revalidate(&grant))
			.await
			.expect("revalidate should fail closed once the window passes");
		assert_eq!(reason, Expired::Stale);
		let elapsed = start.elapsed();
		// Freshness (500ms) THEN the window (800ms), not 800ms total.
		assert!(elapsed >= Duration::from_millis(1300), "closed too early: {elapsed:?}");
		assert!(elapsed < Duration::from_secs(5), "closed too late: {elapsed:?}");
		Ok(())
	}

	/// A stalled endpoint cannot hold a session open indefinitely: the re-check is
	/// bounded by the staleness deadline, floored at one request timeout so a
	/// short window still lets an attempt complete. So the session closes by
	/// roughly `deadline + REQUEST_TIMEOUT`, well before the peer would answer.
	#[tokio::test]
	async fn revalidate_closes_while_a_recheck_hangs() -> anyhow::Result<()> {
		let server = MockServer::start().await;
		Mock::given(method("GET"))
			.and(path_matcher("/auth"))
			.respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(120)))
			.mount(&server)
			.await;

		let auth = auth_with_api(&server).await;
		let grant = test_grant_schedule(
			&auth,
			None,
			Schedule {
				cadence: Duration::from_millis(200),
				staleness: Duration::from_millis(300),
			},
		);

		let start = std::time::Instant::now();
		let reason = tokio::time::timeout(Duration::from_secs(30), auth.revalidate(&grant))
			.await
			.expect("a hung re-check must not outlive its budget");
		assert_eq!(reason, Expired::Stale);
		let elapsed = start.elapsed();
		assert!(
			elapsed < Duration::from_secs(20),
			"should close on its own budget, not wait out the peer: {elapsed:?}"
		);
		Ok(())
	}

	#[tokio::test]
	async fn revalidate_drops_an_abandoned_flight() -> anyhow::Result<()> {
		let server = MockServer::start().await;
		Mock::given(method("GET"))
			.and(path_matcher("/auth"))
			.respond_with(ResponseTemplate::new(404).set_delay(Duration::from_secs(5)))
			.mount(&server)
			.await;

		let auth = auth_with_api(&server).await;
		let grant = test_grant(&auth, None, Duration::from_millis(10));
		let abandoned = tokio::time::timeout(Duration::from_millis(200), auth.revalidate(&grant)).await;
		assert!(abandoned.is_err(), "the re-check must still be in flight");

		let flights = auth
			.auth_api
			.as_ref()
			.unwrap()
			.revalidator
			.flights
			.lock()
			.unwrap()
			.len();
		assert_eq!(flights, 0, "an abandoned flight must not stay in the map");
		Ok(())
	}

	#[tokio::test]
	async fn revalidate_keeps_serving_a_vouched_grant() -> anyhow::Result<()> {
		let server = MockServer::start().await;
		mount_auth(
			&server,
			&format!("max-age={}", u64::MAX),
			r#"{"public":{"subscribe":[""]}}"#.to_string(),
		)
		.await;

		let auth = auth_with_api(&server).await;
		let grant = test_grant(&auth, None, Duration::from_millis(100));
		let pending = tokio::time::timeout(Duration::from_millis(500), auth.revalidate(&grant)).await;
		assert!(pending.is_err(), "a vouched-for grant keeps revalidating");
		Ok(())
	}

	/// A grant is judged by the endpoint that ISSUED it, not by whichever `Auth`
	/// happens to run the re-check. Two differently-configured instances in one
	/// process would otherwise let a stranger's endpoint revoke a healthy session.
	#[tokio::test]
	async fn revalidate_asks_the_granting_endpoint() -> anyhow::Result<()> {
		let vouching = MockServer::start().await;
		mount_auth(&vouching, "max-age=1", r#"{"public":{"subscribe":[""]}}"#.to_string()).await;

		let refusing = MockServer::start().await;
		Mock::given(method("GET"))
			.and(path_matcher("/auth"))
			.respond_with(ResponseTemplate::new(404))
			.mount(&refusing)
			.await;

		let issuer = auth_with_api(&vouching).await;
		let stranger = auth_with_api(&refusing).await;
		let grant = test_grant(&issuer, None, Duration::from_millis(100));

		// The stranger's endpoint refuses everything, so a re-check aimed there
		// would close the session on its first cadence.
		let pending = tokio::time::timeout(Duration::from_millis(500), stranger.revalidate(&grant)).await;
		assert!(pending.is_err(), "a vouched grant must survive a stranger's Auth");
		Ok(())
	}

	/// The other direction of the same rule: an `Auth` with no endpoint of its own
	/// must not silently stop revoking. The authority rides on the grant, so the
	/// re-check still runs and still fails closed.
	#[tokio::test]
	async fn revalidate_without_a_local_api_still_closes() -> anyhow::Result<()> {
		let server = MockServer::start().await;
		Mock::given(method("GET"))
			.and(path_matcher("/auth"))
			.respond_with(ResponseTemplate::new(404))
			.mount(&server)
			.await;

		let issuer = auth_with_api(&server).await;
		let grant = test_grant(&issuer, None, Duration::from_millis(100));

		let stub = Auth::default();
		let reason = tokio::time::timeout(Duration::from_secs(5), stub.revalidate(&grant))
			.await
			.expect("a withdrawn grant must close even without a local endpoint");
		assert_eq!(reason, Expired::Revoked);
		Ok(())
	}

	#[tokio::test(start_paused = true)]
	async fn expired_resolves_at_credential_expiry() {
		let auth = Auth::default();
		let mut token = AuthToken::unrestricted(Path::new("").to_owned());
		token.expires = Some(std::time::SystemTime::now() + Duration::from_millis(100));

		let start = tokio::time::Instant::now();
		let reason = tokio::time::timeout(Duration::from_secs(5), auth.expired(&token))
			.await
			.expect("an expiring credential must resolve the bound");
		assert_eq!(reason, Expired::Credential);
		assert!(start.elapsed() >= Duration::from_millis(100), "resolved before expiry");
	}

	#[tokio::test(start_paused = true)]
	async fn expired_pends_without_an_expiry() {
		let auth = Auth::default();
		let token = AuthToken::unrestricted(Path::new("").to_owned());

		let bounded = tokio::time::timeout(Duration::from_millis(200), auth.expired(&token)).await;
		assert!(bounded.is_err(), "a token without exp or grant must never expire");
	}
}
