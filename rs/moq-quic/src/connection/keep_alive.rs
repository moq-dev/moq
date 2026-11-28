//! A keep-alive that reports a quiet peer before it closes on it
//!
//! A [`KeepAlive`] probes the peer once half of `soft` passes without hearing from it, marks it
//! stale once all of `soft` passes, and closes the connection once `hard` does. Any packet or
//! record the peer sends counts as hearing from it, so a busy connection never probes.

use thiserror::Error;

use crate::{Duration, Instant};

/// How long a peer may go unheard: stale after `soft`, closed after `hard`
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct KeepAlive {
	/// How long the peer may go unheard before it counts as stale. A probe goes out at half of it.
	pub soft: Duration,
	/// How long the peer may go unheard before the connection closes as timed out
	pub hard: Duration,
}

impl KeepAlive {
	/// Stale after `soft` unheard, closed after `hard`
	pub fn new(soft: Duration, hard: Duration) -> Self {
		Self { soft, hard }
	}
}

/// Why a connection refused a [`KeepAlive`]
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum KeepAliveError {
	/// `soft` is too short to halve into a probe interval, or longer than `hard`
	#[error("keep-alive needs a soft limit of at least 2 ns and no longer than its hard limit")]
	Order,
	/// `hard` is longer than the negotiated idle timeout, which would close the connection first
	#[error("keep-alive hard limit {hard:?} is longer than the idle timeout {idle:?}")]
	IdleTimeout {
		/// The hard limit asked for
		hard: Duration,
		/// The negotiated idle timeout
		idle: Duration,
	},
	/// The wire format carries no ping, so a quiet peer cannot be probed
	#[error("the wire format cannot probe the peer")]
	Unsupported,
}

/// A keep-alive in force, and when the peer was last heard
#[derive(Debug)]
pub(crate) struct Liveness {
	config: KeepAlive,
	heard: Instant,
	stale: bool,
}

impl Liveness {
	/// Start `config` on a connection whose idle timeout is `idle`, last hearing from the peer at
	/// `heard`
	pub(crate) fn new(config: KeepAlive, idle: Option<Duration>, heard: Instant) -> Result<Self, KeepAliveError> {
		// Half of `soft` is the probe interval, which must not round to zero and rearm at once.
		if (config.soft / 2).is_zero() || config.soft > config.hard {
			return Err(KeepAliveError::Order);
		}
		if let Some(idle) = idle
			&& config.hard > idle
		{
			return Err(KeepAliveError::IdleTimeout {
				hard: config.hard,
				idle,
			});
		}
		Ok(Self {
			config,
			heard,
			stale: false,
		})
	}

	/// The peer was heard at `now`; returns whether that ended a stale stretch
	pub(crate) fn heard(&mut self, now: Instant) -> bool {
		self.heard = self.heard.max(now);
		std::mem::replace(&mut self.stale, false)
	}

	/// How long to go unheard before probing
	pub(crate) fn probe_after(&self) -> Duration {
		self.config.soft / 2
	}

	/// How many probes a path that stays dead draws before `hard` closes it
	pub(crate) fn probes_before_close(&self) -> u32 {
		let probes = self.config.hard.as_nanos() / self.probe_after().as_nanos();
		u32::try_from(probes).unwrap_or(u32::MAX)
	}

	/// When to probe a peer not heard from since
	pub(crate) fn probe_at(&self) -> Instant {
		self.heard.saturating_add(self.probe_after())
	}

	/// When the peer turns stale, or `None` while it already is
	pub(crate) fn stale_at(&self) -> Option<Instant> {
		(!self.stale).then(|| self.heard.saturating_add(self.config.soft))
	}

	/// When the connection closes unless the peer is heard first
	pub(crate) fn silent_at(&self) -> Instant {
		self.heard.saturating_add(self.config.hard)
	}

	/// Mark the peer stale once `soft` passed unheard by `now`; returns whether it just turned
	pub(crate) fn expire(&mut self, now: Instant) -> bool {
		if self.stale_at().is_some_and(|at| now >= at) {
			self.stale = true;
			return true;
		}
		false
	}

	/// Whether `soft` passed without hearing from the peer
	pub(crate) fn is_stale(&self) -> bool {
		self.stale
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn at(ms: u64) -> Instant {
		Instant::ZERO.saturating_add(Duration::from_millis(ms))
	}

	#[test]
	fn refuses_an_unordered_or_too_long_keep_alive() {
		let ms = Duration::from_millis;
		let idle = Some(ms(10_000));
		// Half of a 1 ns soft limit rounds to a zero probe interval.
		for soft in [Duration::ZERO, Duration::from_nanos(1)] {
			assert_eq!(
				Liveness::new(KeepAlive::new(soft, ms(1)), idle, at(0)).unwrap_err(),
				KeepAliveError::Order
			);
		}
		Liveness::new(KeepAlive::new(Duration::from_nanos(2), ms(1)), idle, at(0)).unwrap();
		assert_eq!(
			Liveness::new(KeepAlive::new(ms(3), ms(2)), idle, at(0)).unwrap_err(),
			KeepAliveError::Order
		);
		assert_eq!(
			Liveness::new(KeepAlive::new(ms(2_000), ms(10_001)), idle, at(0)).unwrap_err(),
			KeepAliveError::IdleTimeout {
				hard: ms(10_001),
				idle: ms(10_000)
			}
		);
		// A hard limit equal to the idle timeout, or no idle timeout at all, is fine.
		Liveness::new(KeepAlive::new(ms(2_000), ms(10_000)), idle, at(0)).unwrap();
		Liveness::new(KeepAlive::new(ms(2_000), ms(60_000)), None, at(0)).unwrap();
	}

	#[test]
	fn turns_stale_after_soft_and_live_on_the_next_hearing() {
		let ms = Duration::from_millis;
		let mut liveness = Liveness::new(KeepAlive::new(ms(2_000), ms(10_000)), None, at(100)).unwrap();
		assert_eq!(liveness.probe_at(), at(1_100));
		assert_eq!(liveness.stale_at(), Some(at(2_100)));
		assert_eq!(liveness.silent_at(), at(10_100));

		assert!(!liveness.expire(at(2_099)));
		assert!(liveness.expire(at(2_100)));
		assert!(liveness.is_stale());
		assert_eq!(liveness.stale_at(), None);
		// Already stale: no second edge.
		assert!(!liveness.expire(at(3_000)));

		assert!(liveness.heard(at(4_000)));
		assert!(!liveness.is_stale());
		assert_eq!(liveness.stale_at(), Some(at(6_000)));
		assert_eq!(liveness.silent_at(), at(14_000));
		// Hearing while live is no edge, and an out-of-order instant never moves it back.
		assert!(!liveness.heard(at(3_000)));
		assert_eq!(liveness.silent_at(), at(14_000));
	}
}
