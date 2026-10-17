//! Fixed session-outcome counters shared by embedders and relay metrics.

use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};

/// The reason a session was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Refusal {
	/// The admission decider refused the session.
	Refused,
	/// The admission decider could not answer.
	Unavailable,
	/// The admission request cannot be decided.
	Request,
	/// The grant covers none of the requested operations.
	Forbidden,
	/// The LAN membership proof was missing or invalid.
	Lan,
}

impl Refusal {
	/// Every currently defined kind, in display order.
	pub const ALL: &'static [Self] = &[
		Self::Refused,
		Self::Unavailable,
		Self::Request,
		Self::Forbidden,
		Self::Lan,
	];
	/// The stable stats and metrics name.
	pub const fn as_str(self) -> &'static str {
		match self {
			Self::Refused => "refused",
			Self::Unavailable => "unavailable",
			Self::Request => "request",
			Self::Forbidden => "forbidden",
			Self::Lan => "lan",
		}
	}
}

/// Sessions refused before admission, with one cumulative count per kind.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct Refusals {
	/// The admission decider refused the session.
	pub refused: u64,
	/// The admission decider could not answer.
	pub unavailable: u64,
	/// The admission request cannot be decided.
	pub request: u64,
	/// The grant covers none of the requested operations.
	pub forbidden: u64,
	/// The LAN membership proof was missing or invalid.
	pub lan: u64,
}

impl Refusals {
	/// The cumulative count for this kind.
	pub fn count(&self, kind: Refusal) -> u64 {
		match kind {
			Refusal::Refused => self.refused,
			Refusal::Unavailable => self.unavailable,
			Refusal::Request => self.request,
			Refusal::Forbidden => self.forbidden,
			Refusal::Lan => self.lan,
		}
	}
	/// Add every kind from another readout.
	pub fn add(&mut self, other: Self) {
		self.refused += other.refused;
		self.unavailable += other.unavailable;
		self.request += other.request;
		self.forbidden += other.forbidden;
		self.lan += other.lan;
	}
	/// Whether any cumulative count went backwards.
	pub fn regressed(&self, previous: &Self) -> bool {
		self.refused < previous.refused
			|| self.unavailable < previous.unavailable
			|| self.request < previous.request
			|| self.forbidden < previous.forbidden
			|| self.lan < previous.lan
	}
}

#[derive(Debug, Default)]
pub(super) struct RefusalCounters {
	refused: AtomicU64,
	unavailable: AtomicU64,
	request: AtomicU64,
	forbidden: AtomicU64,
	lan: AtomicU64,
}

impl RefusalCounters {
	pub(super) fn counter(&self, kind: Refusal) -> &AtomicU64 {
		match kind {
			Refusal::Refused => &self.refused,
			Refusal::Unavailable => &self.unavailable,
			Refusal::Request => &self.request,
			Refusal::Forbidden => &self.forbidden,
			Refusal::Lan => &self.lan,
		}
	}
	pub(super) fn snapshot(&self) -> Refusals {
		Refusals {
			refused: self.refused.load(Ordering::Relaxed),
			unavailable: self.unavailable.load(Ordering::Relaxed),
			request: self.request.load(Ordering::Relaxed),
			forbidden: self.forbidden.load(Ordering::Relaxed),
			lan: self.lan.load(Ordering::Relaxed),
		}
	}
}

/// The reason a session ended abnormally.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Failure {
	/// The transport reached its idle deadline.
	IdleTimeout,
	/// The transport failed.
	Transport,
	/// The application or protocol failed.
	Application,
	/// The session credential expired.
	Expired,
}

impl Failure {
	/// Every currently defined kind, in display order.
	pub const ALL: &'static [Self] = &[Self::IdleTimeout, Self::Transport, Self::Application, Self::Expired];
	/// The stable stats and metrics name.
	pub const fn as_str(self) -> &'static str {
		match self {
			Self::IdleTimeout => "idle_timeout",
			Self::Transport => "transport",
			Self::Application => "application",
			Self::Expired => "expired",
		}
	}
}

/// Admitted sessions ending abnormally, with one cumulative count per kind.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct Failures {
	/// The transport reached its idle deadline.
	pub idle_timeout: u64,
	/// The transport failed.
	pub transport: u64,
	/// The application or protocol failed.
	pub application: u64,
	/// The session credential expired.
	pub expired: u64,
}

impl Failures {
	/// The cumulative count for this kind.
	pub fn count(&self, kind: Failure) -> u64 {
		match kind {
			Failure::IdleTimeout => self.idle_timeout,
			Failure::Transport => self.transport,
			Failure::Application => self.application,
			Failure::Expired => self.expired,
		}
	}
	/// Add every kind from another readout.
	pub fn add(&mut self, other: Self) {
		self.idle_timeout += other.idle_timeout;
		self.transport += other.transport;
		self.application += other.application;
		self.expired += other.expired;
	}
	/// Whether any cumulative count went backwards.
	pub fn regressed(&self, previous: &Self) -> bool {
		self.idle_timeout < previous.idle_timeout
			|| self.transport < previous.transport
			|| self.application < previous.application
			|| self.expired < previous.expired
	}
}

#[derive(Debug, Default)]
pub(super) struct FailureCounters {
	idle_timeout: AtomicU64,
	transport: AtomicU64,
	application: AtomicU64,
	expired: AtomicU64,
}

impl FailureCounters {
	pub(super) fn counter(&self, kind: Failure) -> &AtomicU64 {
		match kind {
			Failure::IdleTimeout => &self.idle_timeout,
			Failure::Transport => &self.transport,
			Failure::Application => &self.application,
			Failure::Expired => &self.expired,
		}
	}
	pub(super) fn snapshot(&self) -> Failures {
		Failures {
			idle_timeout: self.idle_timeout.load(Ordering::Acquire),
			transport: self.transport.load(Ordering::Acquire),
			application: self.application.load(Ordering::Acquire),
			expired: self.expired.load(Ordering::Acquire),
		}
	}
}
