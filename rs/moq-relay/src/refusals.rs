//! Relay refusal collection, shared with published stats when enabled.

use crate::auth;
pub(crate) use moq_net::stats::RefusalKind;
use moq_net::stats::{Registry, Tier};

impl From<&auth::Error> for RefusalKind {
	// Exhaustive on purpose: a new `auth::Error` variant must pick its reason here.
	fn from(err: &auth::Error) -> Self {
		match err {
			auth::Error::Refused(refusal) => match refusal.reason {
				moq_auth::refusal::Reason::Forbidden => RefusalKind::Forbidden,
				moq_auth::refusal::Reason::Expired => RefusalKind::Expired,
				_ => RefusalKind::Refused,
			},
			auth::Error::Unavailable(_) => RefusalKind::Unavailable,
			auth::Error::Forbidden(_) => RefusalKind::Forbidden,
			auth::Error::Request(_) => RefusalKind::Request,
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
	pub(crate) fn record(&self, reason: RefusalKind) {
		self.0.tier(Tier::default()).refuse("", reason);
	}
	pub(crate) fn record_auth(&self, err: &auth::Error) {
		if let auth::Error::Refused(refusal) = err {
			self.0
				.tier(refusal.tier.clone().map(Tier::new).unwrap_or_default())
				.refuse(refusal.root.as_deref().unwrap_or(""), err.into());
		} else {
			self.record(err.into());
		}
	}
	pub(crate) fn snapshot(&self) -> moq_net::stats::Refusals {
		let mut counts = moq_net::stats::Refusals::default();
		for (_, presence) in self.0.snapshot().sessions() {
			counts.add(presence.sessions_refused);
		}
		counts
	}
	#[cfg(test)]
	pub(crate) fn count(&self, reason: RefusalKind) -> u64 {
		self.snapshot().count(reason)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn typed_refusals_keep_only_decider_attribution() {
		let registry = Registry::new(Default::default());
		let refusals = Refusals::new(&registry);
		for reason in [
			moq_auth::refusal::Reason::Refused,
			moq_auth::refusal::Reason::Forbidden,
			moq_auth::refusal::Reason::Expired,
		] {
			for root in [None, Some("tenant")] {
				for tier in [None, Some("edge")] {
					let mut refusal = moq_auth::Refusal::default();
					refusal.reason = reason;
					refusal.root = root.map(str::to_owned);
					refusal.tier = tier.map(str::to_owned);
					let error = auth::Error::Refused(refusal);
					refusals.record_auth(&error);
				}
			}
		}
		let mut report = moq_net::stats::Report::default();
		registry.report(&mut report, moq_net::time::Instant::ZERO);
		assert_eq!(report.sessions.len(), 4);
		for entry in report.sessions {
			assert!(["", "tenant"].contains(&entry.label.as_str()));
			assert!(["", "edge"].contains(&entry.tier.as_str()));
			let presence = entry.presence;
			assert_eq!(presence.sessions_started, 0);
			assert_eq!(presence.sessions_refused.refused, 1);
			assert_eq!(presence.sessions_refused.forbidden, 1);
			assert_eq!(presence.sessions_refused.expired, 1);
		}
		assert_eq!(refusals.count(RefusalKind::Expired), 4);
	}
}
