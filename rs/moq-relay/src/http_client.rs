use anyhow::Context;
use axum::http;
use http_cache_reqwest::{Cache, CacheMode, HttpCache, HttpCacheOptions, MokaCache, MokaManager};
use reqwest_middleware::ClientWithMiddleware;
use std::time::Duration;

/// How long any single request may take. Revalidation needs this to size the
/// budget it gives a re-check, so it lives here rather than inline.
pub(crate) const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// HTTP cache visibility for this client's responses.
#[derive(Clone, Copy, Default)]
pub(crate) enum CacheScope {
	/// Honor shared freshness directives for relay-wide data.
	#[default]
	Shared,
	/// Cache credential-specific grants with ordinary `max-age`.
	Private,
}

/// Build a reqwest client with RFC-compliant HTTP caching (honors `Cache-Control`,
/// `ETag`, `Last-Modified`) over the given TLS config. The client presents the
/// supplied client certificate, so an mTLS-gated endpoint can identify the relay.
///
/// Shared by auth (JWK / public-API fetches) and cluster (peer-list polling).
pub(crate) fn build(tls: &rustls::ClientConfig, scope: CacheScope) -> anyhow::Result<ClientWithMiddleware> {
	let client = reqwest::Client::builder()
		.timeout(REQUEST_TIMEOUT)
		.use_preconfigured_tls(tls.clone())
		.build()
		.context("failed to build HTTP client")?;

	Ok(reqwest_middleware::ClientBuilder::new(client)
		.with(Cache(HttpCache {
			mode: CacheMode::Default,
			// The library default is 42 entries, which is a sample value rather than
			// a relay-sized one. Auth entries are keyed per (kid-or-credential,
			// root, transport) and are now read on a cadence by every live session,
			// so a busy relay thrashes 42 and re-dials the auth API for every miss.
			manager: MokaManager::new(MokaCache::new(10_000)),
			options: HttpCacheOptions {
				cache_key: Some(std::sync::Arc::new(cache_key)),
				// Credential-specific grants need private caching for plain max-age.
				cache_options: Some(http_cache_reqwest::CacheOptions {
					shared: matches!(scope, CacheScope::Shared),
					..Default::default()
				}),
				..HttpCacheOptions::default()
			},
		}))
		.build())
}

/// The default `method:uri` cache key, plus the `Authorization` header when the
/// request carries one.
///
/// A credential-carrying auth-API lookup (see `Auth::verify`) returns a grant
/// decided from that credential, so two viewers on the same URL can legitimately
/// get different answers. `Vary: Authorization` is the endpoint's job to send and
/// an endpoint that forgets it would have one viewer served another's grant, so
/// the split is enforced here instead of trusted. Requests without the header key
/// exactly as before.
fn cache_key(parts: &http::request::Parts) -> String {
	let key = format!("{}:{}", parts.method, parts.uri);
	match parts.headers.get(http::header::AUTHORIZATION) {
		// Digested rather than interpolated: the key is a moka map key that can
		// reach logs and metrics, and the raw value is a bearer secret. SHA-256
		// rather than a `Hash` impl because the split is a security boundary and
		// the credentials are attacker-chosen: two of them landing on one key
		// would serve one viewer another's grant.
		Some(auth) => format!("{key}:{}", digest(auth.as_bytes())),
		None => key,
	}
}

/// Hex SHA-256 of a credential, so it can key a map or a log line without the
/// secret itself. Shared with `Auth`'s flight keys so both split on the same line.
pub(crate) fn digest(credential: impl AsRef<[u8]>) -> String {
	use sha2::Digest;
	hex::encode(sha2::Sha256::digest(credential.as_ref()))
}
