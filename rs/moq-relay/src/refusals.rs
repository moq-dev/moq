//! Relay refusal collection, shared with published stats when enabled.

use crate::auth;
pub(crate) use moq_net::stats::Refusal;
use moq_net::stats::{Registry, Tier};

impl From<&auth::Error> for Refusal {
	// Exhaustive on purpose: a new `auth::Error` variant must pick its reason here.
	fn from(err: &auth::Error) -> Self {
		match err {
			auth::Error::Refused => Refusal::Refused,
			auth::Error::Unavailable(_) => Refusal::Unavailable,
			auth::Error::Forbidden(_) => Refusal::Forbidden,
			auth::Error::Request(_) => Refusal::Request,
		}
	}
}

/// Refusal-only collection when publishing is disabled, otherwise the shared registry.
#[derive(Clone)]
pub(crate) struct Refusals(Registry);

impl Default for Refusals {
	fn default() -> Self {
		Self(Registry::new(Default::default()))
	}
}

impl Refusals {
	pub(crate) fn new(registry: &Registry) -> Self {
		if registry.is_enabled() {
			Self(registry.clone())
		} else {
			Self::default()
		}
	}
	pub(crate) fn record(&self, reason: Refusal) {
		self.0.tier(Tier::default()).refuse("", reason);
	}
	pub(crate) fn snapshot(&self) -> moq_net::stats::Refusals {
		let mut counts = moq_net::stats::Refusals::default();
		for (_, presence) in self.0.snapshot().sessions() {
			counts.add(presence.sessions_refused);
		}
		counts
	}
	#[cfg(test)]
	pub(crate) fn count(&self, reason: Refusal) -> u64 {
		self.snapshot().count(reason)
	}
}
