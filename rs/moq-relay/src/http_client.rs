use anyhow::Context;
use http_cache_reqwest::{Cache, CacheMode, HttpCache, HttpCacheOptions, MokaCache, MokaManager};
use reqwest_middleware::ClientWithMiddleware;
use std::time::Duration;

/// How long any single request may take.
pub(crate) const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// Build a reqwest client with RFC-compliant HTTP caching (honors `Cache-Control`,
/// `ETag`, `Last-Modified`) over the given TLS config. The client presents the
/// supplied client certificate, so an mTLS-gated endpoint can identify the relay.
///
/// Used by the cluster's peer-list polling (`--cluster-connect-api`).
pub(crate) fn build(tls: &rustls::ClientConfig) -> anyhow::Result<ClientWithMiddleware> {
	let client = reqwest::Client::builder()
		.timeout(REQUEST_TIMEOUT)
		.use_preconfigured_tls(tls.clone())
		.build()
		.context("failed to build HTTP client")?;

	Ok(reqwest_middleware::ClientBuilder::new(client)
		.with(Cache(HttpCache {
			mode: CacheMode::Default,
			manager: MokaManager::new(MokaCache::new(100)),
			options: HttpCacheOptions::default(),
		}))
		.build())
}
