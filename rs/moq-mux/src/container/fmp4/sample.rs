//! Resolving the per-sample values a `trun` leaves out.
//!
//! Every reader of a CMAF fragment resolves them the same way, and `js/hang`'s CMAF decoder
//! mirrors it, so a sample is the same size, duration, and keyframe whichever side reads it.

/// `sample_is_non_sync_sample`, bit 16 of the ISO/IEC 14496-12 sample flags.
const NON_SYNC: u32 = 0x0001_0000;

/// `sample_depends_on == 1`: the sample references others.
const DEPENDS_ON_OTHERS: u32 = 0x0100_0000;

/// The sample values a track's fragments fall back to, from its `trex`.
///
/// Each of size, duration, and flags resolves as the `trun` entry, then the `tfhd` default, then
/// this (ISO/IEC 14496-12 §8.8). mp4-atom already folds a run's `first_sample_flags` into its
/// first entry. A track without a `trex` falls back to zero.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Defaults {
	size: u32,
	duration: u32,
	flags: u32,
}

impl Defaults {
	/// The defaults a track's `trex` declares, if it has one.
	pub fn new(trex: Option<&mp4_atom::Trex>) -> Self {
		trex.map(|trex| Self {
			size: trex.default_sample_size,
			duration: trex.default_sample_duration,
			flags: trex.default_sample_flags,
		})
		.unwrap_or_default()
	}

	/// The defaults for the track `track_id` in an init segment's `moov`.
	pub fn from_moov(moov: &mp4_atom::Moov, track_id: u32) -> Self {
		Self::new(
			moov.mvex
				.as_ref()
				.and_then(|mvex| mvex.trex.iter().find(|trex| trex.track_id == track_id)),
		)
	}

	/// Resolve one `trun` entry of a fragment whose `traf` carries `tfhd`.
	pub fn resolve(&self, tfhd: &mp4_atom::Tfhd, entry: &mp4_atom::TrunEntry) -> Sample {
		let duration = entry.duration.or(tfhd.default_sample_duration).unwrap_or(self.duration);

		Sample {
			size: entry.size.or(tfhd.default_sample_size).unwrap_or(self.size),
			// A zero duration is "unknown", not "instantaneous".
			duration: (duration != 0).then_some(duration),
			flags: entry.flags.or(tfhd.default_sample_flags).unwrap_or(self.flags),
		}
	}
}

/// One sample's values, resolved through [`Defaults::resolve`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Sample {
	/// The sample's length in the `mdat`.
	pub size: u32,
	/// The sample's duration in track ticks, if known.
	pub duration: Option<u32>,
	/// The ISO/IEC 14496-12 sample flags.
	pub flags: u32,
}

impl Sample {
	/// Whether this is a sync sample: `sample_is_non_sync_sample` clear and not declared to
	/// reference other samples, the rule ffmpeg applies. Flags that say nothing (zero) describe a
	/// sync sample, so an encoder that only flags its non-sync samples is read correctly.
	pub fn is_sync(&self) -> bool {
		self.flags & (NON_SYNC | DEPENDS_ON_OTHERS) == 0
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn entry(size: Option<u32>, duration: Option<u32>, flags: Option<u32>) -> mp4_atom::TrunEntry {
		mp4_atom::TrunEntry {
			size,
			duration,
			flags,
			cts: None,
		}
	}

	#[test]
	fn trun_then_tfhd_then_trex() {
		let defaults = Defaults::new(Some(&mp4_atom::Trex {
			track_id: 1,
			default_sample_description_index: 1,
			default_sample_duration: 10,
			default_sample_size: 20,
			default_sample_flags: 30,
		}));
		let bare = mp4_atom::Tfhd {
			track_id: 1,
			..Default::default()
		};
		let tfhd = mp4_atom::Tfhd {
			default_sample_duration: Some(11),
			default_sample_size: Some(21),
			default_sample_flags: Some(31),
			..bare.clone()
		};

		let trex_only = defaults.resolve(&bare, &entry(None, None, None));
		assert_eq!(
			(trex_only.size, trex_only.duration, trex_only.flags),
			(20, Some(10), 30)
		);

		let from_tfhd = defaults.resolve(&tfhd, &entry(None, None, None));
		assert_eq!(
			(from_tfhd.size, from_tfhd.duration, from_tfhd.flags),
			(21, Some(11), 31)
		);

		let from_trun = defaults.resolve(&tfhd, &entry(Some(22), Some(12), Some(32)));
		assert_eq!(
			(from_trun.size, from_trun.duration, from_trun.flags),
			(22, Some(12), 32)
		);
	}

	/// A zero duration at whichever level wins is unknown; it doesn't fall through to the next.
	#[test]
	fn zero_duration_is_unknown() {
		let tfhd = mp4_atom::Tfhd {
			track_id: 1,
			default_sample_duration: Some(11),
			..Default::default()
		};
		let sample = Defaults::default().resolve(&tfhd, &entry(None, Some(0), None));
		assert_eq!(sample.duration, None);

		let sample = Defaults::default().resolve(&Default::default(), &entry(None, None, None));
		assert_eq!(sample.duration, None);
	}

	#[test]
	fn sync_rule() {
		let sync = |flags| {
			Sample {
				size: 0,
				duration: None,
				flags,
			}
			.is_sync()
		};

		assert!(sync(0x0200_0000), "depends on no other");
		assert!(sync(0x0000_0000), "flags that say nothing");
		assert!(sync(0x0000_0040), "the degradation priority doesn't matter");
		assert!(!sync(0x0001_0000), "non-sync");
		assert!(!sync(0x0101_0000), "non-sync, depends on others");
		assert!(!sync(0x0100_0000), "depends on others without the non-sync bit");
		assert!(!sync(0x0201_0000), "an I-frame that isn't a random access point");
	}
}
