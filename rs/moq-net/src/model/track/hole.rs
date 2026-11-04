use std::time::Duration;

use crate::StreamError;

/// Why a group sequence will not be delivered.
///
/// lite-07 carries the two kinds in their own messages, SUBSCRIBE_SKIP and SUBSCRIBE_DROP;
/// earlier versions name both in SUBSCRIBE_DROP.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Hole {
	/// Never produced, as the publisher declared (see `track::Producer::skip_groups`): final,
	/// and it ends no reader. Never inferred from a gap, since groups may be created out of order.
	Skip,
	/// Produced but not delivered, ending any reader of it with `error`.
	Drop {
		error: StreamError,
		/// The budget a staleness verdict was judged under, `None` for a drop that does not
		/// depend on it. Only a widening past it reverses the verdict.
		stale: Option<Duration>,
	},
}

impl Hole {
	/// A drop with `error` that no budget reverses.
	pub fn drop(error: StreamError) -> Self {
		Self::Drop { error, stale: None }
	}

	/// A group judged stale under `budget`.
	pub fn stale(budget: Duration) -> Self {
		Self::Drop {
			error: StreamError::Old,
			stale: Some(budget),
		}
	}

	/// A run a reader's misses gave up: stale under `stale` when that is the verdict,
	/// otherwise simply not found. Never a skip, which only the publisher declares.
	pub fn missed(stale: Option<Duration>) -> Self {
		match stale {
			Some(budget) => Self::stale(budget),
			None => Self::drop(StreamError::NotFound),
		}
	}

	/// The error a reader waiting on the sequence ends with: a sequence never produced is
	/// simply not found.
	pub fn error(&self) -> StreamError {
		match self {
			Self::Skip => StreamError::NotFound,
			Self::Drop { error, .. } => error.clone(),
		}
	}

	/// The budget a staleness verdict was judged under, if this is one.
	pub fn stale_budget(&self) -> Option<Duration> {
		match self {
			Self::Drop { stale, .. } => *stale,
			Self::Skip => None,
		}
	}
}
