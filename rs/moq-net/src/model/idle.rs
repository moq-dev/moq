//! The shared early exit for a read that found nothing.
//!
//! A read that parks registers on everything that can change its answer: for a group read,
//! the track, the subscription, the fills it waits on, the route, and the wall deadlines it
//! judged against; for a frame read, the group and its expiry. Each of those notifies on
//! every change, so until one fires, the answer stays the same. [`Idle`] answers a read
//! repeated before that wake with the same `Pending`, rather than walking the whole read
//! path again to reach it.
//!
//! A change the reader makes to its own state, such as moving its range, notifies nothing,
//! so it disarms the gate itself.
//!
//! Gates nest: a read through a gate that polls another gated reader passes it the outer
//! gate's waiter, whose waker stays the same across reads, so the inner gate keeps its arm
//! and its wake reaches the outer gate on the way to the task.

pub(super) use crate::util::Idle;

/// Which group read parked: a track reader parks the arrival and sequence walks
/// separately, since one finding nothing says nothing about the other.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Walk {
	Arrival,
	Sequence,
}

impl Walk {
	/// The walk a read in sequence order for `ordered`, or arrival order otherwise, takes.
	pub(super) fn new(ordered: bool) -> Self {
		match ordered {
			true => Self::Sequence,
			false => Self::Arrival,
		}
	}
}
