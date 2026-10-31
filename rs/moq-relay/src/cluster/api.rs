//! The dynamic peer-list source and its own server trust.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Dynamic peer-list source, independent from mesh TLS trust.
#[derive(usage_rs::Args, Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
#[non_exhaustive]
pub struct Config {
	/// Fetch peers from HTTPS, loopback HTTP, or a local file, refusing redirects,
	/// reloading at runtime without a restart. The source returns a JSON array
	/// of peers: bare URL strings and/or objects with `url`, `cost`, `egress`,
	/// `token`, and `upstream`, exactly as `--cluster-connect` accepts, e.g.
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
	/// TLS trust used only for the peer-list API.
	#[usage(flatten)]
	pub tls: Tls,
}

/// Server roots for a peer-list API; its client identity comes from the dial template.
#[serde_with::serde_as]
#[derive(usage_rs::Args, Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
#[non_exhaustive]
pub struct Tls {
	/// PEM roots to use instead of system roots, verifying the API URL's hostname.
	#[usage(
		long = "cluster-api-tls-root",
		env = "MOQ_CLUSTER_API_TLS_ROOT",
		setting = "cluster.api.tls.root"
	)]
	#[usage(value_hint = usage_rs::ValueHint::FilePath, extensions("pem", "crt", "cer"))]
	#[serde_as(as = "serde_with::OneOrMany<_>")]
	pub root: Vec<PathBuf>,
}
