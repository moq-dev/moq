//! The dynamic peer-list source and its own server trust.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Dynamic peer-list source, independent from mesh TLS trust.
#[serde_with::serde_as]
#[derive(usage_rs::Args, Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
#[non_exhaustive]
pub struct Config {
	/// Fetch peers from HTTPS, loopback HTTP, or a local file, refusing redirects,
	/// reloading at runtime without a restart. The source returns a JSON array
	/// of peers: bare URL strings and/or objects with `url`, `cost`, `token`,
	/// and `upstream`, exactly as `--cluster-connect` accepts, e.g.
	/// `["https://a.pop.example/?cost=1", {"url": "https://b.pop.example/", "cost": 2}]`.
	/// An http(s) URL is re-checked on a fixed cadence, with caching, conditional revalidation
	/// (`ETag` / `Last-Modified`), and stale-if-error handled by the shared HTTP
	/// cache client, so the response's `Cache-Control` controls how often a real
	/// fetch hits the endpoint; a local path is watched via OS filesystem
	/// notifications (with a periodic re-check fallback). This relay's own
	/// `--cluster-node` value, when set, is sent as a `?node=` query param so the
	/// server can return this node's peers. The relay keeps the last good list if
	/// a fetch fails. Composes with `--cluster-connect`.
	#[usage(long = "cluster-api-url", env = "MOQ_CLUSTER_API_URL", setting = "cluster.api.url")]
	pub url: Option<String>,

	/// PEM roots to use instead of system roots, verifying the API URL's hostname.
	/// The client identity comes from the dial template; mesh trust never applies.
	#[usage(
		long = "cluster-api-root",
		env = "MOQ_CLUSTER_API_ROOT",
		setting = "cluster.api.root"
	)]
	#[usage(value_hint = usage_rs::ValueHint::FilePath, extensions("pem", "crt", "cer"))]
	#[serde(skip_serializing_if = "Vec::is_empty")]
	#[serde_as(as = "serde_with::OneOrMany<_>")]
	pub root: Vec<PathBuf>,
}
