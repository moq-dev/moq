use anyhow::Context;
use axum::http;
use http_cache_reqwest::{Cache, CacheMode, HttpCache, HttpCacheOptions, MokaCache, MokaManager};
use reqwest_middleware::ClientWithMiddleware;
use std::time::Duration;

/// How long any single request may take. Revalidation needs this to size the
/// budget it gives a re-check, so it lives here rather than inline.
pub(crate) const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// Build a reqwest client with RFC-compliant HTTP caching (honors `Cache-Control`,
/// `ETag`, `Last-Modified`) over the given TLS config. The client presents the
/// supplied client certificate, so an mTLS-gated endpoint can identify the relay.
///
/// Shared by auth (JWK / public-API fetches) and cluster (peer-list polling).
pub(crate) fn build(tls: &rustls::ClientConfig) -> anyhow::Result<ClientWithMiddleware> {
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
				// A private cache, for every client built here. RFC 9111 §3.5 stops a
				// SHARED cache storing any response to a request carrying
				// `Authorization`, because it cannot tell one user's response from
				// another's; `cache_key` draws exactly that line, and without this a
				// proxy-mode endpoint sending a plain `max-age` would never be cached
				// at all. The cost is that a private cache ignores `s-maxage`, which
				// nothing here documents or reads (see `CacheHints`), and stores
				// `private` responses, which only this process ever sees anyway.
				cache_options: Some(http_cache_reqwest::CacheOptions {
					shared: false,
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
