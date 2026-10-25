//! Automatic certificate configuration, enabled by the default `acme` feature.

use serde::{Deserialize, Serialize};
use std::{path::PathBuf, time::Duration};
use url::Url;

#[cfg(feature = "acme")]
mod client;
#[cfg(feature = "acme")]
pub(crate) use client::Acme;

/// Automatic certificates from an ACME directory, enabled by `acme.domains`.
#[serde_with::serde_as]
#[derive(usage_rs::Args, Clone, Debug, Default, Deserialize, Serialize)]
#[usage(unknown_flags = "error", args_override_self = false)]
#[serde(deny_unknown_fields, default)]
#[non_exhaustive]
pub struct Config {
	/// Domains the certificate covers; setting any enables ACME. Wildcards are refused.
	///
	/// The directory validates each one over TLS-ALPN-01 on port 443, so every
	/// domain must resolve to this relay with `web.https.listen` bound or forwarded there.
	#[usage(
		long = "acme-domains",
		name = "acme-domains",
		delimiter = ',',
		env = "MOQ_ACME_DOMAINS",
		setting = "acme.domains"
	)]
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	#[serde_as(as = "serde_with::OneOrMany<_>")]
	pub domains: Vec<String>,

	/// Contact URLs registered with the ACME account, e.g. `mailto:ops@example.com`.
	#[usage(
		long = "acme-contact",
		name = "acme-contact",
		delimiter = ',',
		env = "MOQ_ACME_CONTACT",
		setting = "acme.contact"
	)]
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	#[serde_as(as = "serde_with::OneOrMany<_>")]
	pub contact: Vec<Url>,

	/// Cache directory required when domains are configured; keeps the account key and certificate private.
	#[usage(
		long = "acme-cache",
		name = "acme-cache",
		env = "MOQ_ACME_CACHE",
		setting = "acme.cache"
	)]
	#[usage(value_hint = usage_rs::ValueHint::DirPath)]
	pub cache: Option<PathBuf>,

	/// The ACME directory URL, Let's Encrypt production when unset.
	#[usage(
		long = "acme-directory",
		name = "acme-directory",
		env = "MOQ_ACME_DIRECTORY",
		setting = "acme.directory"
	)]
	pub directory: Option<Url>,

	/// How long the first issuance may take before startup fails, e.g. "5m". Defaults to 5 minutes.
	#[usage(long = "acme-timeout", env = "MOQ_ACME_TIMEOUT", setting = "acme.timeout")]
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub timeout: Option<moq_tokio::cli::Duration>,
	#[usage(long = "acme-dir", env = "MOQ_ACME_DIR", hide = true)]
	#[serde(skip)]
	legacy_dir: Option<PathBuf>,
	#[usage(skip)]
	#[serde(rename = "dir", skip_serializing)]
	removed_dir: Option<serde::de::IgnoredAny>,
}

impl Config {
	fn timeout(&self) -> Option<Duration> {
		self.timeout.map(Into::into)
	}

	pub(crate) fn deprecated(&self) -> moq_tokio::cli::Deprecated {
		let mut found = moq_tokio::cli::Deprecated::default();
		if self.legacy_dir.is_some() {
			found.flag("--acme-dir", Some("MOQ_ACME_DIR"), "--acme-cache");
		}
		if self.removed_dir.is_some() {
			found.toml("[acme] dir", "[acme] cache", None);
		}
		found
	}

	#[cfg(not(feature = "acme"))]
	pub(crate) fn refuse_unsupported(&self) -> anyhow::Result<()> {
		anyhow::ensure!(
			self.domains.is_empty()
				&& self.contact.is_empty()
				&& self.cache.is_none()
				&& self.directory.is_none()
				&& self.timeout().is_none(),
			"acme settings require the acme feature"
		);
		Ok(())
	}
}
