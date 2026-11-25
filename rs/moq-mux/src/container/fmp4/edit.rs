//! A track's edit list (ISO/IEC 14496-12 §8.6.6), applied to its sample times on import.

use super::Error;

/// The shift a track's edit list makes to its sample times, in the track's media ticks.
///
/// The importer applies it to every timestamp and drops the `edts` from the catalog init, so no
/// consumer needs edit list code. It covers the shape packagers write: empty edits delaying the
/// track, then one edit starting the media at `media_time` at normal rate. A sample before that
/// start is dropped, since a frame timestamp can't be negative; that includes one straddling the
/// start, such as Apple's 2112 AAC priming samples, which are not a whole number of frames. The
/// fMP4 import ignores the edit's duration, since a live source has no end; a file reader trims
/// the end with [`end`](Self::end).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Edit {
	/// The media time presented at the edit's start.
	start: u64,
	/// The empty edits' duration, presented before `start`.
	delay: u64,
	/// The media time where the presentation ends, when the media edit declares a duration.
	end: Option<u64>,
}

impl Edit {
	/// The edit `trak` declares, in a movie counting `movie_timescale` ticks a second, refusing an
	/// edit list this shape can't express.
	pub fn new(trak: &mp4_atom::Trak, movie_timescale: u32) -> Result<Self, Error> {
		match Self::parse(trak, movie_timescale)? {
			(edit, None) => Ok(edit),
			(_, Some(reason)) => Err(Error::UnsupportedEditList(reason)),
		}
	}

	/// The edit `trak` declares, as [`new`](Self::new), but an edit list this shape can't express
	/// plays the media whole from the first media edit's start, with a warning.
	///
	/// For a file, where refusing an edited QuickTime export would refuse the whole file over
	/// what is usually a splice of a few frames.
	pub fn lenient(trak: &mp4_atom::Trak, movie_timescale: u32) -> Result<Self, Error> {
		let (edit, unsupported) = Self::parse(trak, movie_timescale)?;
		if let Some(reason) = unsupported {
			tracing::warn!(
				track = trak.tkhd.track_id,
				reason,
				"unsupported edit list, playing the media from the first edit's start"
			);
		}
		Ok(edit)
	}

	/// The edit `trak` declares, and why it is not the whole edit list when it isn't.
	fn parse(trak: &mp4_atom::Trak, movie_timescale: u32) -> Result<(Self, Option<&'static str>), Error> {
		let Some(elst) = trak.edts.as_ref().and_then(|edts| edts.elst.as_ref()) else {
			return Ok((Self::default(), None));
		};

		let mut entries = elst.entries.iter();
		let mut empty = 0u64;
		let (media_time, entry) = loop {
			let Some(entry) = entries.next() else {
				return Ok(match elst.entries.is_empty() {
					true => (Self::default(), None),
					false => (Self::default(), Some("no edit presents media")),
				});
			};
			match entry.media_time {
				Some(media_time) => break (media_time, entry),
				None => empty = empty.checked_add(entry.segment_duration).ok_or(Error::PtsOverflow)?,
			}
		};
		let mut unsupported = None;
		// Several media edits splice the media, which one shift can't express.
		if entries.next().is_some() {
			unsupported = Some("an edit follows the media edit");
		}
		// A rate of 0 dwells on one sample, and the standard defines no other.
		if entry.media_rate.integer() != 1 || entry.media_rate.decimal() != 0 {
			unsupported = Some("media rate is not 1");
		}

		let timescale = u128::from(trak.mdia.mdhd.timescale);
		// Movie ticks in media ticks, or `None` without a movie timescale to convert from.
		let media = |movie: u64| match movie_timescale {
			0 => Ok(None),
			scale => u64::try_from(u128::from(movie) * timescale / u128::from(scale))
				.map(Some)
				.map_err(|_| Error::PtsOverflow),
		};
		let delay = match media(empty)? {
			_ if empty == 0 => 0,
			Some(delay) => delay,
			None => {
				unsupported = Some("movie timescale is zero");
				0
			}
		};
		// A duration of zero is unknown, as a fragmented file's edit declares it, and one past the
		// media timeline trims nothing. An edit list this shape can't express plays the media
		// whole, so it trims nothing either.
		let end = match (entry.segment_duration, movie_timescale) {
			(0, _) | (_, 0) => None,
			_ if unsupported.is_some() => None,
			(duration, scale) => u64::try_from(u128::from(duration) * timescale / u128::from(scale))
				.ok()
				.and_then(|duration| media_time.checked_add(duration)),
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

		let edit = match media_time.checked_sub(skip) {
			Some(start) => Self { start, delay, end },
			None => Self {
				start: 0,
				delay: delay.checked_add(skip - media_time).ok_or(Error::PtsOverflow)?,
				end,
			},
		};
		Ok((edit, unsupported))
	}

	/// Where the sample at media time `pts` presents, or `None` if it precedes the edit.
	pub fn present(&self, pts: u64) -> Result<Option<u64>, Error> {
		let Some(since) = pts.checked_sub(self.start) else {
			return Ok(None);
		};
		since.checked_add(self.delay).map(Some).ok_or(Error::PtsOverflow)
	}

	/// Where the sample decoding at media time `dts` decodes, on the timeline
	/// [`present`](Self::present) maps onto: before zero for one decoding ahead of the edit's start.
	pub fn decode(&self, dts: i64) -> Result<i64, Error> {
		let shifted = i128::from(dts) - i128::from(self.start) + i128::from(self.delay);
		i64::try_from(shifted).map_err(|_| Error::PtsOverflow)
	}

	/// The media time where the presentation ends: a sample presenting at or after it is cut.
	pub fn end(&self) -> Option<u64> {
		self.end
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

	/// A media edit's duration ends the presentation in media ticks, and an unknown one ends
	/// nothing.
	#[test]
	fn a_duration_ends_the_presentation() {
		// One movie second (1000 ticks) is 48000 media ticks.
		let edit = Edit::new(&trak(Some(vec![entry(1000, Some(960), 1)]), false), 1000).unwrap();
		assert_eq!(edit.end(), Some(960 + 48_000));
		let edit = Edit::new(&trak(Some(vec![entry(0, Some(960), 1)]), false), 1000).unwrap();
		assert_eq!(edit.end(), None);
		// An Opus edit ends where its media does, wherever the pre-skip moves its start.
		let edit = Edit::new(&trak(Some(vec![entry(1000, Some(312), 1)]), true), 1000).unwrap();
		assert_eq!(
			(presents(edit), edit.end()),
			([Some(0), Some(959), Some(960), Some(1000)], Some(48_312))
		);
	}

	/// What the strict edit refuses, the lenient one plays from the first media edit's start, with
	/// no end when edits follow it.
	#[test]
	fn lenient_edits_play_from_the_first_media_edit() {
		let cases = [
			(
				vec![entry(1000, None, 1)],
				[Some(0), Some(959), Some(960), Some(1000)],
				None,
			),
			(
				vec![entry(1000, Some(960), 1), entry(1000, Some(0), 1)],
				[None, None, Some(0), Some(40)],
				None,
			),
			(vec![entry(1000, Some(960), 2)], [None, None, Some(0), Some(40)], None),
		];
		for (entries, expected, end) in cases {
			assert!(
				Edit::new(&trak(Some(entries.clone()), false), 1000).is_err(),
				"{entries:?}"
			);
			let edit = Edit::lenient(&trak(Some(entries.clone()), false), 1000).unwrap();
			assert_eq!((presents(edit), edit.end()), (expected, end), "{entries:?}");
		}
		// A sample decoding before the edit's start decodes before zero.
		let edit = Edit::lenient(&trak(Some(vec![entry(0, Some(960), 1)]), false), 1000).unwrap();
		assert_eq!(edit.decode(0).unwrap(), -960);
	}
}
