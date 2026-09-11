use moq_net::bandwidth::{Allocator, Rate, Reservation};
use moq_net::track;

/// A passthrough track's standing claim on an [`Allocator`].
///
/// The ceiling is the peak-hold catalog bitrate: taken on the first estimate,
/// raised when a later window exceeds it, never walked back. Nobody chose this
/// track's bitrate, so a grant is nothing it can act on.
pub(crate) struct Claim {
	allocator: Allocator,
	reservation: Option<Reservation>,
	ceiling: Rate,
}

impl Claim {
	pub fn new(allocator: Allocator) -> Self {
		Self {
			allocator,
			reservation: None,
			ceiling: Rate::ZERO,
		}
	}

	/// Reserve `bitrate` on the first estimate; raise the ceiling only when a later
	/// window exceeds it. A missing bitrate (the window hasn't closed) is a no-op.
	pub fn update(&mut self, track: &track::Demand, bitrate: Option<u64>) {
		let Some(bitrate) = bitrate else {
			return;
		};
		let max = Rate::from_bps(bitrate);
		if let Some(reservation) = &self.reservation {
			if max > self.ceiling {
				reservation.update(max);
				self.ceiling = max;
			}
			return;
		}
		self.reservation = Some(self.allocator.reserve(track, max));
		self.ceiling = max;
	}

	#[cfg(test)]
	pub fn ceiling(&self) -> Option<Rate> {
		self.reservation.as_ref().map(|_| self.ceiling)
	}
}
