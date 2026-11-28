//! What a label's moq-lite-07 sessions did on the route layer.

use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};

/// A kind enum, the readout counting each kind, and its atomic backing.
macro_rules! kinds {
	(
		$(#[$kind_doc:meta])* $kind:ident,
		$(#[$counts_doc:meta])* $counts:ident,
		$atomics:ident,
		{ $($(#[$doc:meta])* $variant:ident => $field:ident),+ $(,)? }
	) => {
		$(#[$kind_doc])*
		#[derive(Debug, Clone, Copy, PartialEq, Eq)]
		#[non_exhaustive]
		pub enum $kind {
			$($(#[$doc])* $variant,)+
		}

		impl $kind {
			/// Every currently defined kind, in display order.
			pub const ALL: &'static [Self] = &[$(Self::$variant),+];

			/// The stable stats and metrics name.
			pub const fn as_str(self) -> &'static str {
				match self {
					$(Self::$variant => stringify!($field),)+
				}
			}
		}

		$(#[$counts_doc])*
		#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
		#[serde(default)]
		#[non_exhaustive]
		pub struct $counts {
			$($(#[$doc])* pub $field: u64,)+
		}

		impl $counts {
			/// The cumulative count for this kind.
			pub fn count(&self, kind: $kind) -> u64 {
				match kind {
					$($kind::$variant => self.$field,)+
				}
			}

			/// The sum over every kind.
			pub fn total(&self) -> u64 {
				0u64 $(.saturating_add(self.$field))+
			}

			/// Add every kind from another readout.
			pub fn add(&mut self, other: Self) {
				$(self.$field = self.$field.saturating_add(other.$field);)+
			}

			/// Whether any cumulative count went backwards.
			pub fn regressed(&self, previous: &Self) -> bool {
				false $(|| self.$field < previous.$field)+
			}
		}

		#[derive(Debug, Default)]
		pub(super) struct $atomics {
			$($field: AtomicU64,)+
		}

		impl $atomics {
			pub(super) fn add(&self, kind: $kind, count: u64) {
				let counter = match kind {
					$($kind::$variant => &self.$field,)+
				};
				counter.fetch_add(count, Ordering::Relaxed);
			}

			pub(super) fn snapshot(&self) -> $counts {
				$counts {
					$($field: self.$field.load(Ordering::Relaxed),)+
				}
			}
		}
	};
}

kinds!(
	/// A message that withdraws routes.
	Withdrawal,
	/// Route withdrawals by kind.
	Withdrawals,
	WithdrawalCounters,
	{
		/// DEAD: a session between two relays closed, ending every route through it.
		Dead => dead,
		/// DRAIN: a relay sent GOAWAY, so every route through it ranks as draining.
		Drain => drain,
	}
);

kinds!(
	/// Why this node refused a route or claim a peer forwarded, keeping the session.
	RouteRefusal,
	/// Refused routes and claims by reason.
	RouteRefusals,
	RouteRefusalCounters,
	{
		/// It names a path this node cannot hold.
		Path => path,
		/// It lies outside the session's root or grant, or what the origin serves.
		Scope => scope,
		/// Its way already crosses this relay, or more relays than the hop limit allows.
		Looped => looped,
	}
);

kinds!(
	/// Why a session closed for its route layer.
	RouteClose,
	/// Sessions closed for their route layer, by kind.
	RouteCloses,
	RouteCloseCounters,
	{
		/// The peer stopped reading its route stream.
		Stalled => stalled,
		/// The peer read its route stream too slowly for what changed.
		Overrun => overrun,
		/// The peer sent a route, claim, request, or prefix the rule never builds, or broke
		/// its route stream.
		Malformed => malformed,
		/// The peer sent what its role never sends, such as a routing peer asking for
		/// part of the tree.
		Role => role,
		/// The peer's stream fell out of step: a claim outside what was asked, or under a
		/// route it never offered.
		Unasked => unasked,
		/// Any other reason, such as the peer announcing past the session's cap.
		Other => other,
	}
);

/// What a label's moq-lite-07 sessions did on the route layer, cumulative.
///
/// Each count lands on the label of the session whose peer caused it, never per
/// broadcast or per node, so its size stays flat as a mesh grows.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct Routes {
	/// Times the routes to a relay changed what they give the ways to its claims, on what
	/// the peer sent or how its session ended. A move to another path of the same rank
	/// changes nothing, and a burst of messages changing one relay's routes before the
	/// node acts on them counts once.
	pub changed: u64,
	/// Broadcasts rewritten into the origin behind those changes: every broadcast the
	/// relay still claims as the node acts on the change, each once per pass. A route
	/// that ended takes its claims with it, which are rewritten as claims ending instead.
	pub rewritten: u64,
	/// Withdrawals sent to the peer, by kind.
	pub sent: Withdrawals,
	/// Withdrawals received from the peer, by kind.
	pub received: Withdrawals,
	/// Routes and claims the peer forwarded that this node refused, keeping the session.
	/// Their total is [`Presence::routes_refused`](super::Presence::routes_refused).
	pub refused: RouteRefusals,
	/// Sessions closed for their route layer, by kind.
	pub closed: RouteCloses,
}

impl Routes {
	/// Whether nothing has been counted.
	pub(crate) fn is_zero(&self) -> bool {
		*self == Self::default()
	}

	/// Add every counter from another readout.
	pub fn add(&mut self, other: Self) {
		self.changed = self.changed.saturating_add(other.changed);
		self.rewritten = self.rewritten.saturating_add(other.rewritten);
		self.sent.add(other.sent);
		self.received.add(other.received);
		self.refused.add(other.refused);
		self.closed.add(other.closed);
	}

	/// Whether any cumulative count went backwards.
	pub fn regressed(&self, previous: &Self) -> bool {
		self.changed < previous.changed
			|| self.rewritten < previous.rewritten
			|| self.sent.regressed(&previous.sent)
			|| self.received.regressed(&previous.received)
			|| self.refused.regressed(&previous.refused)
			|| self.closed.regressed(&previous.closed)
	}
}

#[derive(Debug, Default)]
pub(super) struct RouteCounters {
	changed: AtomicU64,
	rewritten: AtomicU64,
	pub(super) sent: WithdrawalCounters,
	pub(super) received: WithdrawalCounters,
	pub(super) refused: RouteRefusalCounters,
	pub(super) closed: RouteCloseCounters,
}

impl RouteCounters {
	pub(super) fn changed(&self, rewritten: u64) {
		self.changed.fetch_add(1, Ordering::Relaxed);
		self.rewritten.fetch_add(rewritten, Ordering::Relaxed);
	}

	pub(super) fn snapshot(&self) -> Routes {
		Routes {
			changed: self.changed.load(Ordering::Relaxed),
			rewritten: self.rewritten.load(Ordering::Relaxed),
			sent: self.sent.snapshot(),
			received: self.received.snapshot(),
			refused: self.refused.snapshot(),
			closed: self.closed.snapshot(),
		}
	}
}
