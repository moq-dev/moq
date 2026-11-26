use std::time::Duration;

use crate::StreamError;

/// Why a group key will not be delivered.
///
/// lite-07 proves a skip below a group with that group's GROUP header (`Previous Delta`), and
/// names every other hole in SUBSCRIBE_DROP, as earlier versions name both.
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
		/// The exclusive group end the upstream subscription judged a staleness verdict within,
		/// `None` when it followed the live edge or its end is not known to be the one it judged
		/// under. A reader capped at the same end reads the groups the verdict rests on.
		cap: Option<u64>,
	},
}

impl Hole {
	/// A drop with `error` that no budget reverses.
	pub fn drop(error: StreamError) -> Self {
		Self::Drop {
			error,
			stale: None,
			cap: None,
		}
	}

	/// A group judged stale under `budget`.
	pub fn stale(budget: Duration) -> Self {
		Self::Drop {
			error: StreamError::Old,
			stale: Some(budget),
			cap: None,
		}
	}

	/// This verdict, judged within the exclusive group end `cap`.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub fn capped(mut self, cap: Option<u64>) -> Self {
		if let Self::Drop { cap: judged, .. } = &mut self {
			*judged = cap;
		}
		self
	}

	/// A run a reader's misses gave up: stale under `stale` when that is the verdict,
	/// otherwise simply not found. Never a skip, which only the publisher declares.
	pub fn missed(stale: Option<Duration>) -> Self {
		match stale {
			Some(budget) => Self::stale(budget),
			None => Self::drop(StreamError::NotFound),
		}
	}

	/// The error a reader waiting on the key ends with: a key never produced is
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

	/// The exclusive group end a staleness verdict was judged within, if it is one with a cap.
	pub fn stale_cap(&self) -> Option<u64> {
		match self {
			Self::Drop {
				stale: Some(_), cap, ..
			} => *cap,
			_ => None,
		}
	}
}
