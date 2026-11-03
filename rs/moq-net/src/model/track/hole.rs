use std::time::Duration;

use crate::StreamError;

/// Why a group sequence will not be delivered.
///
/// lite-07 carries the two kinds in their own messages, SUBSCRIBE_SKIP and SUBSCRIBE_DROP;
/// earlier versions name both in SUBSCRIBE_DROP.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Hole {
	/// Never produced, such as a skipped sequence number: final, and it ends no reader.
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
