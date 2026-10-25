use std::collections::VecDeque;

use hang::catalog::VideoConfig;

/// The reorder depth `config` declares, or `None` when it declares none.
///
/// Refuses a depth its codec and level cannot reach, so a declared clock never holds more frames
/// than a conforming decoder's picture buffer.
pub(crate) fn depth(config: &VideoConfig) -> crate::Result<Option<usize>> {
	let Some(reorder) = config.reorder else {
		return Ok(None);
	};
	let max = crate::codec::video::max_reorder(config);
	if u32::from(reorder) > max {
		return Err(ClockError::Depth {
			reorder,
			max,
			codec: config.codec.to_string(),
		}
		.into());
	}
	Ok(Some(usize::from(reorder)))
}

/// One group's decode clock, at its rendition's declared reorder depth.
///
/// The n-th frame of a group in decode order decodes at the group's (n - depth)-th presentation
/// slot. Once the group's first depth + 1 frames are read, no frame still to come can be presented
/// below the earliest of them, so from then on each frame is released as it is read: it takes the
/// least slot not yet taken, which a later frame could undercut only by following more than depth
/// frames in presentation order. The first depth frames decode one tick apart up to that earliest
/// slot, which needs no frame spacing, so a DTS can fall before the timeline's zero. A tick is the
/// caller's timescale, the finest its container writes: 90 kHz for MPEG-TS, 1 ms for FLV.
///
/// A clock lives for one group, so its DTS depends on nothing read before the group, and a frame
/// reordered deeper than declared fails rather than move the timeline.
pub(crate) struct DeclaredClock<T> {
	depth: usize,
	/// The group's earliest PTS, once enough frames are read that none to come can precede it.
	earliest: Option<u64>,
	/// Frames still to be released that decode before [`Self::earliest`].
	leading: usize,
	/// Frames read but not yet released, in decode order, with their PTS.
	held: VecDeque<(T, u64)>,
	/// Presentation slots not yet taken, ascending.
	slots: Vec<u64>,
	/// The largest depth + 1 PTS read, ascending: all a deeper reorder is checked against.
	highest: Vec<u64>,
	/// The last DTS released.
	last: Option<i128>,
}

impl<T> DeclaredClock<T> {
	/// A clock for one group of a rendition reordering at most `depth` frames, from [`depth`].
	pub(crate) fn new(depth: usize) -> Self {
		Self {
			depth,
			earliest: None,
			leading: depth,
			held: VecDeque::new(),
			slots: Vec::new(),
			highest: Vec::new(),
			last: None,
		}
	}

	/// How many frames are held.
	#[cfg(test)]
	pub(crate) fn len(&self) -> usize {
		self.held.len()
	}

	/// Hold the group's next frame in decode order, presented at `pts`.
	pub(crate) fn push(&mut self, item: T, pts: u64) -> crate::Result<()> {
		if self.held.len() > self.depth {
			return Err(ClockError::Undrained.into());
		}
		let at = self.highest.partition_point(|&prior| prior <= pts);
		if self.highest.len() - at > self.depth {
			return Err(ClockError::Reordered(self.depth).into());
		}
		self.highest.insert(at, pts);
		if self.highest.len() > self.depth + 1 {
			self.highest.remove(0);
		}
		let at = self.slots.partition_point(|&slot| slot <= pts);
		self.slots.insert(at, pts);
		self.held.push_back((item, pts));
		Ok(())
	}

	/// The next frame and its DTS in signed ticks, once the group's earliest slot is known,
	/// or at once when `flush` because the group ended.
	pub(crate) fn pop(&mut self, flush: bool) -> crate::Result<Option<(T, i128)>> {
		if self.held.is_empty() || (self.earliest.is_none() && !flush && self.held.len() <= self.depth) {
			return Ok(None);
		}
		let earliest = *self.earliest.get_or_insert(self.slots[0]);
		let dts = match self.leading {
			0 => i128::from(self.slots.remove(0)),
			leading => {
				self.leading -= 1;
				i128::from(earliest) - leading as i128
			}
		};
		let (item, pts) = self.held.pop_front().expect("the held queue is nonempty");
		if dts > i128::from(pts) || self.last.is_some_and(|last| dts <= last) {
			return Err(ClockError::Presentation.into());
		}
		self.last = Some(dts);
		Ok(Some((item, dts)))
	}
}

#[derive(Debug, thiserror::Error)]
enum ClockError {
	#[error("video reorder depth {reorder} exceeds the {max} that {codec} can reach")]
	Depth { reorder: u8, max: u32, codec: String },
	#[error("video reorders past its declared depth {0}")]
	Reordered(usize),
	#[error("video presentation slots cannot author increasing DTS at or before PTS")]
	Presentation,
	#[error("the decode clock must be drained before reading another frame")]
	Undrained,
}

impl From<ClockError> for crate::Error {
	fn from(err: ClockError) -> Self {
		anyhow::Error::new(err).into()
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	/// A B-pyramid (`max_num_reorder_frames` 2) in decode order, in 40 ms frames.
	const PYRAMID: [u64; 9] = [0, 4, 2, 1, 3, 8, 6, 5, 7];
	const FRAME: u64 = 3_600;

	/// The DTS of one group read through a declared clock of `depth`, released as each frame is
	/// read, checking it never holds more than `depth` frames between reads.
	fn declared_dts(depth: usize, read: &[u64]) -> Vec<i128> {
		let mut clock = DeclaredClock::new(depth);
		let mut out = Vec::new();
		for &pts in read {
			clock.push((), pts).unwrap();
			while let Some(((), dts)) = clock.pop(false).unwrap() {
				out.push(dts);
			}
			assert!(clock.len() <= depth);
		}
		while let Some(((), dts)) = clock.pop(true).unwrap() {
			out.push(dts);
		}
		out
	}

	#[test]
	fn declared_slots_shift_by_count_and_start_before_zero() {
		let ticks = |frames: &[i128]| frames.iter().map(|i| i * FRAME as i128).collect::<Vec<_>>();
		let leading = |dts: Vec<i128>| [vec![-2, -1], dts].concat();
		assert_eq!(
			declared_dts(2, &PYRAMID.map(|i| i * FRAME)),
			leading(ticks(&[0, 1, 2, 3, 4, 5, 6]))
		);
		// References spaced wider than any frame period still take their slots by count.
		let far = [0, 6, 10, 1, 2, 3, 4, 5, 7, 8, 9].map(|i| i * FRAME);
		assert_eq!(declared_dts(2, &far), leading(ticks(&[0, 1, 2, 3, 4, 5, 6, 7, 8])));
	}

	/// Open-GOP leading pictures present before the keyframe that starts their group, so the
	/// group's first frames decode below its earliest slot, not the keyframe's.
	#[test]
	fn leading_pictures_decode_before_the_keyframe() {
		let read = [10, 8, 9, 14, 12, 11, 13];
		assert_eq!(declared_dts(2, &read), [6, 7, 8, 9, 10, 11, 12]);
		// A group that ends before the lookahead fills still takes its earliest slot.
		assert_eq!(declared_dts(2, &read[..2]), [6, 7]);
	}

	#[test]
	fn a_zero_depth_releases_each_frame_at_its_pts() {
		let mut clock = DeclaredClock::new(0);
		for pts in [0, 1, 90_000, u64::MAX] {
			clock.push(pts, pts).unwrap();
			assert_eq!(clock.pop(false).unwrap(), Some((pts, i128::from(pts))));
			assert_eq!(clock.len(), 0);
		}
		clock.push(0, 0).unwrap_err();
	}

	/// Flat or backward PTS read past the declared depth fail within depth + 1 frames, however
	/// many follow and however large they are, rather than accumulate.
	#[test]
	fn malformed_presentation_is_refused_without_retaining_its_payload() {
		for backwards in [false, true] {
			for count in [16, 128, 1024] {
				for size in [256, 16_384] {
					let mut clock = DeclaredClock::new(2);
					let refused = (0..count).position(|index| {
						let pts = 90_000 - if backwards { index } else { 0 };
						let refused = clock.push(vec![0u8; size], pts).is_err()
							|| std::iter::from_fn(|| clock.pop(false).transpose()).any(|popped| popped.is_err());
						assert!(clock.len() <= 2);
						refused
					});
					assert!(
						refused.is_some_and(|index| index <= 3),
						"backwards={backwards} count={count}"
					);
				}
			}
		}
	}

	#[test]
	fn a_declared_depth_fits_the_codec_level() {
		use hang::catalog::{H264, VideoCodec};

		let config = |codec: VideoCodec, size: Option<(u32, u32)>, reorder: u8| {
			let mut config = VideoConfig::new(codec);
			config.coded_width = size.map(|(width, _)| width);
			config.coded_height = size.map(|(_, height)| height);
			config.reorder = Some(reorder);
			config
		};
		let h264 = |level| H264 {
			profile: 100,
			constraints: 0,
			level,
			inline: false,
		};
		let h265: VideoCodec = "hvc1.1.6.L123.B0".parse().unwrap();
		let vp9: VideoCodec = "vp09.00.10.08".parse().unwrap();
		let cases = [
			// Level 4.0's 32768 macroblocks hold four 1920x1088 frames.
			(config(h264(40).into(), Some((1920, 1080)), 4), true),
			(config(h264(40).into(), Some((1920, 1080)), 5), false),
			// Without a size, the deepest buffer any picture of the level gets.
			(config(h264(40).into(), None, 16), true),
			(config(h264(40).into(), None, 17), false),
			// Level 4.1's MaxLumaPs holds six 1920x1080 pictures, one of them the current.
			(config(h265.clone(), Some((1920, 1080)), 5), true),
			(config(h265, Some((1920, 1080)), 6), false),
			(config(vp9.clone(), None, 0), true),
			(config(vp9, None, 1), false),
		];
		for (config, valid) in cases {
			assert_eq!(depth(&config).is_ok(), valid, "{config:?}");
		}
		assert_eq!(depth(&VideoConfig::new(VideoCodec::VP8)).unwrap(), None);
	}
}
