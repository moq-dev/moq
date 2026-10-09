//! Sessions admission turned away, counted by reason.
//!
//! A refused session never gets a stats context, so without these counts the
//! only trace of one is a log line. `/metrics` renders them.

use std::sync::{
	Arc,
	atomic::{AtomicU64, Ordering},
};

use crate::auth;

/// Why admission turned a session away.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Refusal {
	/// The decider said no: the auth server, the public rules, or an embedder.
	Refused,
	/// The decider could not answer: the auth server was unreachable or answered
	/// with neither a grant nor a refusal, a decider sent a grant the relay cannot
	/// use, or an embedder did not answer.
	Unavailable,
	/// An embedder refused the request as one it cannot decide. The relay's own
	/// deciders never answer this way.
	Request,
	/// The grant allows nothing the session asked for, or an embedder refused the
	/// session as forbidden.
	Forbidden,
	/// A LAN peer's membership proof was missing or wrong, or LAN discovery is off.
	Lan,
}

impl Refusal {
	/// Every reason, so `/metrics` can list each one from zero.
	pub(crate) const ALL: &'static [Refusal] = &[
		Refusal::Refused,
		Refusal::Unavailable,
		Refusal::Request,
		Refusal::Forbidden,
		Refusal::Lan,
	];

	/// The `reason` label value.
	pub(crate) const fn as_str(self) -> &'static str {
		match self {
			Refusal::Refused => "refused",
			Refusal::Unavailable => "unavailable",
			Refusal::Request => "request",
			Refusal::Forbidden => "forbidden",
			Refusal::Lan => "lan",
		}
	}
}

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

/// Refused-session counts by [`Refusal`]. Clones share the counts.
#[derive(Clone, Default)]
pub(crate) struct Refusals(Arc<Counts>);

#[derive(Default)]
struct Counts {
	refused: AtomicU64,
	unavailable: AtomicU64,
	request: AtomicU64,
	forbidden: AtomicU64,
	lan: AtomicU64,
}

impl Refusals {
	/// Count one refused session.
	pub(crate) fn record(&self, refusal: Refusal) {
		self.counter(refusal).fetch_add(1, Ordering::Relaxed);
	}

	/// Sessions refused for `refusal` so far.
	pub(crate) fn count(&self, refusal: Refusal) -> u64 {
		self.counter(refusal).load(Ordering::Relaxed)
	}

	fn counter(&self, refusal: Refusal) -> &AtomicU64 {
		match refusal {
			Refusal::Refused => &self.0.refused,
			Refusal::Unavailable => &self.0.unavailable,
			Refusal::Request => &self.0.request,
			Refusal::Forbidden => &self.0.forbidden,
			Refusal::Lan => &self.0.lan,
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn clones_share_counts() {
		let refusals = Refusals::default();
		let clone = refusals.clone();
		clone.record(Refusal::Refused);
		clone.record(Refusal::Refused);
		refusals.record(Refusal::Forbidden);
		assert_eq!(refusals.count(Refusal::Refused), 2);
		assert_eq!(clone.count(Refusal::Forbidden), 1);
		assert_eq!(refusals.count(Refusal::Unavailable), 0);
	}
}
