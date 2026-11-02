//! The shared early exit for a group read that found nothing.
//!
//! A read that parks registers on everything that can change its answer: the track, the
//! subscription, the fills it waits on, and the wall deadlines it judged against. Each of
//! those notifies on every change, so until one fires, the answer stays the same. [`Idle`]
//! answers a read repeated before that wake with the same `Pending`, rather than walking
//! the whole read path again to reach it.
//!
//! A change the reader makes to its own cursor, such as moving its range, notifies nothing,
//! so it disarms the gate itself.

/// A cursor's parked group read, and the waiter it parked on.
pub(super) type Idle = crate::util::Idle<Walk>;

/// Which read parked: a [`Cursor`](super::Cursor) parks the arrival and sequence walks
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
