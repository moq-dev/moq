use anyhow::Context;
use http_cache_reqwest::{Cache, CacheMode, HttpCache, HttpCacheOptions, MokaManager};
use reqwest_middleware::ClientWithMiddleware;

/// Build a reqwest client with RFC-compliant HTTP caching (honors `Cache-Control`,
/// `ETag`, `Last-Modified`) over the given TLS config. The client presents the
/// supplied client certificate, so an mTLS-gated endpoint can identify the relay.
///
/// Shared by auth (JWK / public-API fetches) and cluster (peer-list polling).
pub(crate) fn build(tls: &rustls::ClientConfig) -> anyhow::Result<ClientWithMiddleware> {
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
