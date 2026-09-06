//! Validated output renditions in ascending bitrate and height order.

/// One candidate output rendition: a target resolution (by height) and bitrate.
///
/// The width is derived from the source aspect ratio at runtime, and a rung is
/// only offered when it is strictly below the source (see [`Ladder`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Rung {
	/// Output height in pixels. Rounded down to even when it enters a [`Ladder`]
	/// (I420 chroma is 2x2).
	pub height: u32,

	/// The configured maximum in bits per second: the CBR target advertised in the
	/// derivative catalog.
	pub bitrate: u64,
}

impl Rung {
	/// A rung at `height` pixels and `bitrate` bits per second.
	pub fn new(height: u32, bitrate: u64) -> Self {
		Self { height, bitrate }
	}
}

/// Why a set of rungs is not a ladder.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
	/// A rung that encodes nothing: a height that rounds to zero pixels, or no
	/// bitrate to spend.
	#[error("rung {height}p at {bitrate} bps encodes nothing")]
	Empty {
		/// The configured height, before rounding to even.
		height: u32,
		/// The configured maximum, in bits per second.
		bitrate: u64,
	},

	/// Two rungs claim the same maximum bitrate, so neither one is the lower of
	/// the pair.
	#[error("rungs {first}p and {second}p share a maximum of {bitrate} bps")]
	DuplicateBitrate {
		/// The shared maximum, in bits per second.
		bitrate: u64,
		/// The shorter rung's height, in pixels.
		first: u32,
		/// The taller rung's height, in pixels.
		second: u32,
	},

	/// Resolution runs backwards against bitrate: a rung costs more than the one
	/// below it without being taller, so bitrate and picture disagree on which
	/// rendition is lower.
	#[error("rung {height}p at {bitrate} bps does not rise above {below_height}p at {below_bitrate} bps")]
	Unordered {
		/// The more expensive rung's height, in pixels.
		height: u32,
		/// The more expensive rung's maximum, in bits per second.
		bitrate: u64,
		/// The cheaper rung's height, in pixels.
		below_height: u32,
		/// The cheaper rung's maximum, in bits per second.
		below_bitrate: u64,
	},
}

/// The output ladder, in canonical order: strictly ascending maximum bitrate,
/// and strictly ascending height with it.
///
/// Construction validates the order; [`Ladder::rungs`] exposes a read-only slice. The
/// rungs are still filtered against the source at runtime (nothing above it
/// survives), which drops rungs but never reorders them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ladder {
	// Ascending by bitrate, and by height with it. Heights are even.
	rungs: Vec<Rung>,
}

impl Ladder {
	/// Resolve `rungs` into a ladder, in any order.
	///
	/// Heights round down to even, and the result is sorted by maximum bitrate.
	/// An ambiguous ladder is an [`Error`] rather than a guess: two rungs at the
	/// same maximum have no lower one, and a rung that costs more without being
	/// taller means bitrate and picture disagree about which rendition is lower.
	/// An empty ladder is fine; it just offers nothing.
	pub fn new(rungs: impl IntoIterator<Item = Rung>) -> Result<Self, Error> {
		let mut rungs: Vec<Rung> = rungs.into_iter().collect();
		for rung in &mut rungs {
			if rung.height < 2 || rung.bitrate == 0 {
				return Err(Error::Empty {
					height: rung.height,
					bitrate: rung.bitrate,
				});
			}
			// Normalize before checking uniqueness: odd heights can share a track name.
			rung.height &= !1;
		}

		rungs.sort_by_key(|rung| (rung.bitrate, rung.height));

		for pair in rungs.windows(2) {
			let (below, rung) = (pair[0], pair[1]);
			if below.bitrate == rung.bitrate {
				return Err(Error::DuplicateBitrate {
					bitrate: rung.bitrate,
					first: below.height,
					second: rung.height,
				});
			}
			if rung.height <= below.height {
				return Err(Error::Unordered {
					height: rung.height,
					bitrate: rung.bitrate,
					below_height: below.height,
					below_bitrate: below.bitrate,
				});
			}
		}

		Ok(Self { rungs })
	}

	/// The rungs, lowest first: ascending maximum bitrate and ascending height.
	pub fn rungs(&self) -> &[Rung] {
		&self.rungs
	}
}

impl Default for Ladder {
	/// The default ladder: 240p to 1080p, filtered against the source at runtime
	/// so only strictly-lower renditions are offered.
	fn default() -> Self {
		Self::new([
			Rung::new(240, 350_000),
			Rung::new(360, 600_000),
			Rung::new(480, 1_200_000),
			Rung::new(720, 2_500_000),
			Rung::new(1080, 5_000_000),
		])
		.expect("the default ladder is ordered")
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn heights(ladder: &Ladder) -> Vec<u32> {
		ladder.rungs().iter().map(|rung| rung.height).collect()
	}

	#[test]
	fn default_ladder_is_ordered() {
		let ladder = Ladder::default();
		assert_eq!(heights(&ladder), [240, 360, 480, 720, 1080]);
	}

	/// The operator writes the ladder top-down (or in whatever order), and it
	/// still resolves to one canonical bottom-up ranking.
	#[test]
	fn custom_ladder_out_of_order() {
		let ladder = Ladder::new([
			Rung::new(720, 2_500_000),
			Rung::new(240, 350_000),
			Rung::new(480, 1_200_000),
		])
		.unwrap();
		assert_eq!(heights(&ladder), [240, 480, 720]);
		// The neighbour is the next rendition down, which is what the band
		// formula reads.
		assert_eq!(ladder.rungs()[1].bitrate, 1_200_000);
		assert_eq!(ladder.rungs()[0].bitrate, 350_000);
	}

	/// Two rungs at one maximum: neither is the lower, so there is no ladder.
	#[test]
	fn duplicate_ceiling_is_refused() {
		let err = Ladder::new([Rung::new(720, 2_500_000), Rung::new(480, 2_500_000)]).unwrap_err();
		assert_eq!(
			err,
			Error::DuplicateBitrate {
				bitrate: 2_500_000,
				first: 480,
				second: 720,
			}
		);
	}

	/// Odd heights round to even, so a ladder can collide on a height it never
	/// literally wrote. Silently dropping one rung would move its neighbour's
	/// "next lower rendition", so it is refused too.
	#[test]
	fn duplicate_height_is_refused() {
		let err = Ladder::new([Rung::new(721, 2_500_000), Rung::new(720, 1_200_000)]).unwrap_err();
		assert_eq!(
			err,
			Error::Unordered {
				height: 720,
				bitrate: 2_500_000,
				below_height: 720,
				below_bitrate: 1_200_000,
			}
		);
	}

	/// Paying more for a smaller picture: bitrate and resolution disagree about
	/// which rendition is lower, and guessing either one mis-ranks the ladder.
	#[test]
	fn resolution_inversion_is_refused() {
		let err = Ladder::new([Rung::new(1080, 1_000_000), Rung::new(360, 3_000_000)]).unwrap_err();
		assert_eq!(
			err,
			Error::Unordered {
				height: 360,
				bitrate: 3_000_000,
				below_height: 1080,
				below_bitrate: 1_000_000,
			}
		);
	}

	#[test]
	fn rung_without_a_rendition_is_refused() {
		assert_eq!(
			Ladder::new([Rung::new(1, 350_000)]).unwrap_err(),
			Error::Empty {
				height: 1,
				bitrate: 350_000
			}
		);
		assert_eq!(
			Ladder::new([Rung::new(240, 0)]).unwrap_err(),
			Error::Empty {
				height: 240,
				bitrate: 0
			}
		);
	}

	#[test]
	fn empty_ladder_is_allowed() {
		assert!(Ladder::new([]).unwrap().rungs().is_empty());
	}
}
