use anyhow::Context;
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
			options: HttpCacheOptions::default(),
		}))
		.build())
}
