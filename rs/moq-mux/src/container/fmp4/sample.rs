//! Resolving the per-sample values a `trun` leaves out, and the `mdat` bytes each sample takes.
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

/// Resolve one fragment's samples against its mdat, sharing byte ownership across tracks.
pub(crate) struct Resolver {
	data_start: usize,
	len: usize,
	claimed: Claimed,
}

/// A sample whose byte range and duration have been validated.
pub(crate) struct Resolved {
	pub sample: Sample,
	pub cts: i64,
	pub range: std::ops::Range<usize>,
}

impl Resolver {
	pub fn new(data_start: usize, len: usize) -> Self {
		Self {
			data_start,
			len,
			claimed: Claimed::default(),
		}
	}

	/// Walk samples without allocating from the untrusted sample count.
	pub fn resolve<'a>(
		&'a mut self,
		traf: &'a mp4_atom::Traf,
		defaults: Defaults,
	) -> Result<impl Iterator<Item = Result<Resolved, super::Error>> + 'a, super::Error> {
		if traf.tfhd.base_data_offset.is_some() {
			return Err(super::Error::UnsupportedBaseDataOffset);
		}
		let mut remaining: u64 = traf.trun.iter().map(|run| run.samples.len() as u64).sum();
		if remaining == 0 {
			return Err(super::Error::MissingTrun);
		}
		let mut runs = traf.trun.iter();
		let mut entries = mp4_atom::TrunSamplesIter::default();
		let mut offset = 0usize;
		Ok(std::iter::from_fn(move || {
			if remaining == 0 {
				return None;
			}
			let result = (|| {
				let entry = loop {
					if let Some(entry) = entries.next() {
						break entry;
					}
					let run = runs.next().ok_or(super::Error::MissingTrun)?;
					if let Some(start) = run.data_offset {
						offset = usize::try_from(start)
							.ok()
							.and_then(|start| start.checked_sub(self.data_start))
							.ok_or(super::Error::InvalidDataOffset)?;
					}
					entries = run.samples.iter();
				};
				let sample = defaults.resolve(&traf.tfhd, &entry);
				if sample.size == 0 {
					return Err(super::Error::EmptySample);
				}
				let end = offset
					.checked_add(sample.size as usize)
					.ok_or(super::Error::InvalidDataOffset)?;
				if end > self.len {
					return Err(super::Error::SampleRangeOutOfBounds {
						start: offset,
						end,
						len: self.len,
					});
				}
				let range = offset..end;
				self.claimed.take(range.clone())?;
				if sample.duration.is_none() && remaining > 1 {
					return Err(super::Error::MissingSampleDuration);
				}
				offset = end;
				remaining -= 1;
				Ok(Resolved {
					sample,
					cts: entry.cts.unwrap_or_default(),
					range,
				})
			})();
			if result.is_err() {
				remaining = 0;
			}
			Some(result)
		}))
	}
}

/// The `mdat` bytes a fragment's samples have taken, so no two samples share any.
///
/// Each sample's bytes are copied or published once per sample, so runs that all pointed at one
/// range would turn a small fragment into output as large as its run count times its `mdat`.
#[derive(Default)]
struct Claimed(std::collections::BTreeMap<usize, usize>);

impl Claimed {
	/// Take `range` of the `mdat` payload, or fail if an earlier sample holds any of it.
	pub fn take(&mut self, range: std::ops::Range<usize>) -> Result<(), super::Error> {
		if range.is_empty() {
			return Err(super::Error::EmptySample);
		}
		// The taken ranges are disjoint, so the last one starting before `range` ends reaches furthest.
		if let Some((_, &end)) = self.0.range(..range.end).next_back()
			&& end > range.start
		{
			return Err(super::Error::InvalidDataOffset);
		}
		self.0.insert(range.start, range.end);
		Ok(())
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

	/// Adjacent ranges are taken; empty ranges and any shared byte are refused.
	#[test]
	fn claimed_ranges_are_disjoint() {
		let mut claimed = Claimed::default();
		claimed.take(10..20).unwrap();
		claimed.take(20..30).unwrap();
		claimed.take(0..10).unwrap();
		assert!(claimed.take(15..15).is_err());
		for overlap in [10..20, 5..11, 29..40, 12..13, 0..40] {
			assert!(claimed.take(overlap.clone()).is_err(), "{overlap:?}");
		}
		claimed.take(30..31).unwrap();
	}

	fn traf(offsets: &[Option<i32>]) -> mp4_atom::Traf {
		mp4_atom::Traf {
			tfhd: mp4_atom::Tfhd {
				default_sample_size: Some(2),
				default_sample_duration: Some(1),
				..Default::default()
			},
			trun: offsets
				.iter()
				.map(|&data_offset| mp4_atom::Trun {
					data_offset,
					samples: mp4_atom::TrunSamples::Inherited {
						count: 1,
						first_flags: None,
					},
				})
				.collect(),
			..Default::default()
		}
	}

	#[test]
	fn resolves_disjoint_runs_in_decode_order() {
		let traf = traf(&[Some(104), Some(100), None]);
		let mut resolver = Resolver::new(100, 6);
		let ranges: Vec<_> = resolver
			.resolve(&traf, Defaults::default())
			.unwrap()
			.map(|sample| sample.unwrap().range)
			.collect();
		assert_eq!(ranges, [4..6, 0..2, 2..4]);
		// Ownership includes other tracks of the same fragment.
		let other = traf.clone();
		assert!(matches!(
			resolver.resolve(&other, Defaults::default()).unwrap().next().unwrap(),
			Err(super::super::Error::InvalidDataOffset)
		));
	}

	#[test]
	fn refuses_incorrect_sample_ranges() {
		use super::super::Error;
		for offset in [i32::MIN, -1, 99, 105, i32::MAX] {
			let traf = traf(&[Some(offset)]);
			let mut resolver = Resolver::new(100, 6);
			let mut samples = resolver.resolve(&traf, Defaults::default()).unwrap();
			assert!(samples.next().unwrap().is_err(), "offset {offset}");
			assert!(samples.next().is_none(), "errors end iteration");
		}
		let mut traf = traf(&[None]);
		for size in [None, Some(0)] {
			traf.tfhd.default_sample_size = size;
			let mut resolver = Resolver::new(100, 6);
			assert!(matches!(
				resolver.resolve(&traf, Defaults::default()).unwrap().next().unwrap(),
				Err(Error::EmptySample)
			));
		}
		for base in [0, 100, u64::MAX] {
			traf.tfhd.base_data_offset = Some(base);
			let mut resolver = Resolver::new(100, 6);
			assert!(matches!(
				resolver.resolve(&traf, Defaults::default()),
				Err(Error::UnsupportedBaseDataOffset)
			));
		}
	}

	/// A moof of many field-less runs decodes as counts rather than one entry per sample, and
	/// resolving it stops at the end of the mdat.
	#[test]
	fn fieldless_runs_do_not_amplify() {
		use mp4_atom::{Decode, Encode};

		let samples = mp4_atom::TrunSamples::Inherited {
			count: u32::MAX,
			first_flags: None,
		};
		let mut traf = traf(&[None; 1000]);
		for run in &mut traf.trun {
			run.samples = samples.clone();
		}
		let moof = mp4_atom::Moof {
			mfhd: mp4_atom::Mfhd { sequence_number: 1 },
			traf: vec![traf],
		};
		let mut buf = Vec::new();
		moof.encode(&mut buf).unwrap();
		assert!(buf.len() < 16 * 1024);

		let decoded = mp4_atom::Moof::decode(&mut buf.as_slice()).unwrap();
		assert!(decoded.traf[0].trun.iter().all(|run| run.samples == samples));

		let mut resolver = Resolver::new(100, 6);
		let resolved: Vec<_> = resolver
			.resolve(&decoded.traf[0], Defaults::default())
			.unwrap()
			.collect();
		assert_eq!(resolved.len(), 4);
		assert!(resolved[..3].iter().all(Result::is_ok));
		assert!(matches!(
			resolved[3],
			Err(super::super::Error::SampleRangeOutOfBounds { .. })
		));
	}

	#[test]
	fn only_the_final_sample_may_lack_a_duration() {
		use super::super::Error;
		let mut traf = traf(&[None, None]);
		traf.trun[1].samples = vec![entry(None, Some(0), None)].into();
		// Empty trailing runs don't make the last sample non-final.
		traf.trun.push(Default::default());
		let mut resolver = Resolver::new(100, 4);
		let durations: Vec<_> = resolver
			.resolve(&traf, Defaults::default())
			.unwrap()
			.map(|sample| sample.unwrap().sample.duration)
			.collect();
		assert_eq!(durations, [Some(1), None]);
		traf.trun[0].samples = vec![entry(None, Some(0), None)].into();
		let mut resolver = Resolver::new(100, 4);
		assert!(matches!(
			resolver.resolve(&traf, Defaults::default()).unwrap().next().unwrap(),
			Err(Error::MissingSampleDuration)
		));
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
