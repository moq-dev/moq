//! Debounces a capture source's geometry.
//!
//! The encoder is built for the geometry the stream opened with, so a source
//! that changes size has to end the stream and let the caller reopen. Ending on
//! the first frame that differs turns one drag of a window edge into a reopen
//! per mouse-move, each one dropping the encoder, publishing a discontinuity,
//! and republishing the catalog rendition for a size that is about to change
//! again. So hold instead: once the geometry moves, stop capturing but keep
//! polling, and only end once it has stayed put.

use std::time::{Duration, Instant};

/// How long a new geometry has to hold still before the stream ends.
///
/// This trades reopen latency against churn. It is well above the gap between
/// the size changes a drag produces (pointer input arrives every few
/// milliseconds, and even a slow compositor coalesces far below this), so a
/// whole drag settles once. It is still short enough that a one-shot resize
/// (snap, maximize, a restored window) reopens promptly, which matters because
/// the picture is frozen on the last pre-resize frame until it does.
pub(super) const HOLD: Duration = Duration::from_millis(250);

/// What to do with the frame that is due now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Settled {
	/// The geometry is still the one the stream opened with: capture.
	Open,
	/// The geometry moved and has not settled: skip this frame, poll again.
	Waiting,
	/// The new geometry held for [`HOLD`]: end the stream so the caller reopens.
	Changed,
}

/// Tracks a source's geometry against the one its stream opened with.
pub(super) struct Settle<T> {
	opened: T,
	/// The differing geometry being waited on and when it was first seen. A
	/// further change replaces it, restarting the wait.
	pending: Option<(T, Instant)>,
}

impl<T: Clone + PartialEq> Settle<T> {
	/// Watch for changes against the geometry the stream opened with.
	pub fn new(opened: T) -> Self {
		Self { opened, pending: None }
	}

	/// Feed the geometry observed at `now`.
	pub fn observe(&mut self, current: &T, now: Instant) -> Settled {
		if *current == self.opened {
			// A drag that ends where it started leaves nothing to reopen for.
			self.pending = None;
			return Settled::Open;
		}

		match &self.pending {
			Some((pending, since)) if pending == current => {
				if now.saturating_duration_since(*since) >= HOLD {
					Settled::Changed
				} else {
					Settled::Waiting
				}
			}
			// A new size restarts the wait, so a drag settles only once it stops.
			_ => {
				self.pending = Some((current.clone(), now));
				Settled::Waiting
			}
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn the_open_geometry_captures() {
		let start = Instant::now();
		let mut settle = Settle::new((1920u32, 1080u32));
		assert_eq!(settle.observe(&(1920, 1080), start), Settled::Open);
		assert_eq!(settle.observe(&(1920, 1080), start + HOLD * 4), Settled::Open);
	}

	#[test]
	fn a_change_ends_the_stream_once_it_holds() {
		let start = Instant::now();
		let mut settle = Settle::new((1920u32, 1080u32));
		assert_eq!(settle.observe(&(1280, 720), start), Settled::Waiting);
		assert_eq!(settle.observe(&(1280, 720), start + HOLD / 2), Settled::Waiting);
		assert_eq!(settle.observe(&(1280, 720), start + HOLD), Settled::Changed);
	}

	#[test]
	fn a_drag_restarts_the_wait_on_every_new_size() {
		let start = Instant::now();
		let mut settle = Settle::new((1920u32, 1080u32));
		// One size change every 10ms for twice the settle window: a drag never
		// settles while the pointer is moving.
		let mut now = start;
		for step in 1..=(2 * HOLD.as_millis() / 10) as u32 {
			now = start + Duration::from_millis(u64::from(step) * 10);
			assert_eq!(settle.observe(&(1920 - step * 2, 1080), now), Settled::Waiting);
		}
		// The pointer stops: the last size settles one window later.
		let last = (1920 - (2 * HOLD.as_millis() / 10) as u32 * 2, 1080);
		assert_eq!(settle.observe(&last, now + HOLD / 2), Settled::Waiting);
		assert_eq!(settle.observe(&last, now + HOLD), Settled::Changed);
	}

	#[test]
	fn a_drag_back_to_the_open_geometry_cancels() {
		let start = Instant::now();
		let mut settle = Settle::new((1920u32, 1080u32));
		assert_eq!(settle.observe(&(1280, 720), start), Settled::Waiting);
		assert_eq!(settle.observe(&(1920, 1080), start + HOLD / 2), Settled::Open);
		// The earlier change is forgotten, so the wait starts over.
		assert_eq!(settle.observe(&(1280, 720), start + HOLD), Settled::Waiting);
		assert_eq!(settle.observe(&(1280, 720), start + HOLD * 2), Settled::Changed);
	}
}
