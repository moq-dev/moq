//! A track's edit list (ISO/IEC 14496-12 §8.6.6), applied to its sample times on import.

use super::Error;

/// The shift a track's edit list makes to its sample times, in the track's media ticks.
///
/// The importer applies it to every timestamp and drops the `edts` from the catalog init, so no
/// consumer needs edit list code. It covers the shape packagers write: empty edits delaying the
/// track, then one edit starting the media at `media_time` at normal rate. A sample before that
/// start is dropped, since a frame timestamp can't be negative; that includes one straddling the
/// start, such as Apple's 2112 AAC priming samples, which are not a whole number of frames. The
/// edit's duration is ignored: it trims the end, and a live source has none.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Edit {
	/// The media time presented at the edit's start.
	start: u64,
	/// The empty edits' duration, presented before `start`.
	delay: u64,
}

impl Edit {
	/// The edit `trak` declares, in a movie counting `movie_timescale` ticks a second.
	pub fn new(trak: &mp4_atom::Trak, movie_timescale: u32) -> Result<Self, Error> {
		let Some(elst) = trak.edts.as_ref().and_then(|edts| edts.elst.as_ref()) else {
			return Ok(Self::default());
		};
		if elst.entries.is_empty() {
			return Ok(Self::default());
		}

		let mut entries = elst.entries.iter();
		let mut empty = 0u64;
		let (media_time, entry) = loop {
			let entry = entries
				.next()
				.ok_or(Error::UnsupportedEditList("no edit presents media"))?;
			match entry.media_time {
				Some(media_time) => break (media_time, entry),
				None => empty = empty.checked_add(entry.segment_duration).ok_or(Error::PtsOverflow)?,
			}
		};
		// Several media edits splice the media, which one shift can't express.
		if entries.next().is_some() {
			return Err(Error::UnsupportedEditList("an edit follows the media edit"));
		}
		// A rate of 0 dwells on one sample, and the standard defines no other.
		if entry.media_rate.integer() != 1 || entry.media_rate.decimal() != 0 {
			return Err(Error::UnsupportedEditList("media rate is not 1"));
		}

		let timescale = u128::from(trak.mdia.mdhd.timescale);
		let delay = match empty {
			0 => 0,
			_ if movie_timescale == 0 => return Err(Error::UnsupportedEditList("movie timescale is zero")),
			empty => u64::try_from(u128::from(empty) * timescale / u128::from(movie_timescale))
				.map_err(|_| Error::PtsOverflow)?,
		};

		// A consumer trims the OpusHead pre-skip from the first decoded frame and plays the first
		// sample it keeps at the frame timestamp (draft-lcurley-moq-hang), while in ISO BMFF the edit
		// alone trims. So the edit moves an Opus frame by its distance from the pre-skip: an edit
		// starting inside the pre-skip delays the kept samples by the rest of it.
		let skip = match trak.mdia.minf.stbl.stsd.codecs.as_slice() {
			[mp4_atom::Codec::Opus(opus)] => {
				u64::try_from(u128::from(opus.dops.pre_skip) * timescale / 48_000).map_err(|_| Error::PtsOverflow)?
			}
			_ => 0,
		};

		Ok(match media_time.checked_sub(skip) {
			Some(start) => Self { start, delay },
			None => Self {
				start: 0,
				delay: delay.checked_add(skip - media_time).ok_or(Error::PtsOverflow)?,
			},
		})
	}

	/// Where the sample at media time `pts` presents, or `None` if it precedes the edit.
	pub fn present(&self, pts: u64) -> Result<Option<u64>, Error> {
		let Some(since) = pts.checked_sub(self.start) else {
			return Ok(None);
		};
		since.checked_add(self.delay).map(Some).ok_or(Error::PtsOverflow)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn entry(segment_duration: u64, media_time: Option<u64>, rate: i16) -> mp4_atom::ElstEntry {
		mp4_atom::ElstEntry {
			segment_duration,
			media_time,
			media_rate: mp4_atom::FixedPoint::from(rate),
		}
	}

	/// A 48 kHz track with `entries` as its edit list, Opus with a 312-sample pre-skip if `opus`.
	fn trak(entries: Option<Vec<mp4_atom::ElstEntry>>, opus: bool) -> mp4_atom::Trak {
		let codec = match opus {
			true => mp4_atom::Opus {
				audio: mp4_atom::Audio {
					data_reference_index: 1,
					channel_count: 2,
					sample_size: 16,
					sample_rate: mp4_atom::FixedPoint::from(48_000),
				},
				dops: mp4_atom::Dops {
					output_channel_count: 2,
					pre_skip: 312,
					input_sample_rate: 48_000,
					output_gain: 0,
					channel_mapping: None,
				},
				btrt: None,
			}
			.into(),
			false => mp4_atom::Codec::Unknown(b"test".into(), Vec::new()),
		};
		mp4_atom::Trak {
			edts: entries.map(|entries| mp4_atom::Edts {
				elst: Some(mp4_atom::Elst { entries }),
			}),
			..super::super::build_audio_trak(1, 48_000, codec)
		}
	}

	/// Where media times 0, 959, 960, and 1000 present under `edit`.
	fn presents(edit: Edit) -> [Option<u64>; 4] {
		[0, 959, 960, 1000].map(|pts| edit.present(pts).unwrap())
	}

	#[test]
	fn edits_shift_and_drop() {
		let none = [Some(0), Some(959), Some(960), Some(1000)];
		let cases = [
			(None, false, none),
			(Some(vec![]), false, none),
			(
				Some(vec![entry(0, Some(960), 1)]),
				false,
				[None, None, Some(0), Some(40)],
			),
			// One movie second (1000 ticks) is 48000 media ticks.
			(
				Some(vec![entry(1000, None, 1), entry(0, Some(960), 1)]),
				false,
				[None, None, Some(48_000), Some(48_040)],
			),
			// Opus timestamps already count the pre-skip, so an edit of just the pre-skip moves nothing.
			(Some(vec![entry(0, Some(312), 1)]), true, none),
			(
				Some(vec![entry(0, Some(312 + 960), 1)]),
				true,
				[None, None, Some(0), Some(40)],
			),
			// An edit inside the pre-skip shows media 312, the first sample a consumer keeps, at 212.
			(
				Some(vec![entry(0, Some(100), 1)]),
				true,
				[Some(212), Some(1171), Some(1172), Some(1212)],
			),
		];
		for (entries, opus, expected) in cases {
			let edit = Edit::new(&trak(entries.clone(), opus), 1000).unwrap();
			assert_eq!(presents(edit), expected, "{entries:?}, opus {opus}");
		}
	}

	#[test]
	fn unsupported_edits_are_refused() {
		let cases = [
			vec![entry(1000, None, 1)],
			vec![entry(0, Some(0), 1), entry(0, Some(960), 1)],
			vec![entry(0, Some(0), 1), entry(1000, None, 1)],
			vec![entry(1000, Some(0), 0)],
		];
		for entries in cases {
			assert!(
				matches!(
					Edit::new(&trak(Some(entries.clone()), false), 1000),
					Err(Error::UnsupportedEditList(_))
				),
				"{entries:?}"
			);
		}
	}
}
