//! Stateful per-frame fragmenting on one continuous decode timeline.

use std::time::Duration;

use crate::container::Frame;

use super::Fragment;
use super::export::{apply_codec_durations, fragment_seconds, infer_missing_durations};

/// Cuts a live stream of frames into one moof+mdat fragment each, on one continuous
/// decode timeline.
///
/// This is the per-frame counterpart to [`Muxer::fragment`](super::Muxer::fragment), for a
/// consumer that stores media per encoded frame: LL-HLS Partial Segments are cut at
/// stored-object boundaries, so one fragment per group would make a whole GOP (~1s) the
/// smallest addressable unit, coarser than a typical `PART-TARGET`. Calling `fragment` once
/// per frame instead re-anchors every fragment at its own frame, which collapses the
/// composition offsets to zero (losing B-frame presentation order) and times every sample
/// by the catalog cadence rather than its real successor.
///
/// The fragmenter owns everything a per-frame caller would otherwise have to reproduce: the
/// `tfdt` decode timeline advances by exactly the durations written into the preceding
/// `trun`s, the moof sequence numbers count up on their own, and a frame that carries no
/// [`duration`](Frame::duration) is held until its successor arrives to time it. Feed it
/// frames in decode order with [`push`](Self::push) as they arrive, and [`flush`](Self::flush)
/// at the end of the stream:
///
/// ```no_run
/// # fn example(muxer: &moq_mux::container::fmp4::Muxer, frames: Vec<moq_mux::container::Frame>) -> moq_mux::Result<()> {
/// let mut fragmenter = muxer.fragmenter(moq_mux::container::fmp4::fragment::Config::default());
/// for frame in frames {
///     if let Some(fragment) = fragmenter.push(frame)? {
///         // store fragment.data; fragment.duration and .independent describe the Part
///     }
/// }
/// if let Some(fragment) = fragmenter.flush()? {
///     // the final frame, timed by its stated duration or the catalog cadence
/// }
/// # Ok(())
/// # }
/// ```
///
/// A frame that states its own duration (CMAF input, an Opus TOC byte, or a publisher that
/// sets [`Frame::duration`]) normally emits with no lookahead. If the preceding frame was
/// waiting for this one to determine its duration, that preceding fragment is returned first
/// and the stated frame remains queued for the next `push` or [`flush`](Self::flush). A
/// successor that opens a new group (a keyframe) is never used as a duration bound, since the
/// publisher may have paused across the boundary; the pending frame then takes the catalog
/// cadence, exactly like the last frame of a [`Muxer::fragment`](super::Muxer::fragment) call.
///
/// Each group re-anchors the decode timeline at its keyframe's presentation time. When the
/// durations tile the timeline this is a no-op, and when they don't (a publisher pause, a
/// variable frame rate with no stated durations) it keeps `tfdt` truthful instead of letting
/// the composition offsets grow without bound.
pub struct Fragmenter {
	/// The `tfhd` track id, matching the one the muxer's init segment declares.
	pub(super) track_id: u32,
	/// The media timescale fragments are expressed in, matching the init segment.
	pub(super) timescale: moq_net::Timescale,
	/// Fallback duration for a frame with no stated duration and no usable successor.
	pub(super) default_frame: Duration,
	/// Video fragments are independent only at a GOP boundary; audio always is.
	pub(super) is_video: bool,
	/// True for Opus audio, whose packets state their own duration in the TOC byte.
	pub(super) opus: bool,
	/// The next frame waiting for either its duration or its turn to be emitted.
	pub(super) pending: Option<Frame>,
	/// The next fragment's decode time in ticks at `timescale`; `None` before the first frame.
	pub(super) dts: Option<u64>,
	/// The next moof sequence number.
	pub(super) sequence: u32,
}

impl Fragmenter {
	/// Feed the next frame in decode order, returning a finished fragment when one is ready.
	///
	/// Returns `Some` for the earliest frame ready to emit: normally the frame itself when it
	/// states its own duration, or the previously pushed frame once this one has timed it. At
	/// most one fragment is returned per push, so a stated frame stays queued when the previous
	/// frame uses that push's output slot. The final queued frame is retrieved with
	/// [`flush`](Self::flush).
	pub fn push(&mut self, mut frame: Frame) -> crate::Result<Option<Fragment>> {
		// Opus states its duration in the TOC byte; read it now so such a frame never waits.
		apply_codec_durations(std::slice::from_mut(&mut frame), self.opus);

		let Some(mut pending) = self.pending.take() else {
			if stated(&frame) {
				return Ok(Some(self.emit(frame)?));
			}
			self.pending = Some(frame);
			return Ok(None);
		};

		// The incoming frame is the successor that times the pending one. A keyframe may
		// open a new group, and a group boundary is never a duration (the publisher may
		// have paused across it), so the pending frame then takes the catalog cadence.
		infer_missing_durations(std::slice::from_mut(&mut pending), Some(&frame), self.default_frame);
		let fragment = self.emit(pending)?;
		self.pending = Some(frame);
		Ok(Some(fragment))
	}

	/// Emit the pending frame at the end of the stream.
	///
	/// A duration-less tail takes the catalog cadence; a stated duration is preserved. Consumes
	/// the fragmenter because there is no successor left to time anything by. Returns `None`
	/// when every pushed frame was already emitted.
	pub fn flush(mut self) -> crate::Result<Option<Fragment>> {
		let Some(mut pending) = self.pending.take() else {
			return Ok(None);
		};
		infer_missing_durations(std::slice::from_mut(&mut pending), None, self.default_frame);
		Ok(Some(self.emit(pending)?))
	}

	/// Encode one frame as its own fragment and advance the timeline past it.
	fn emit(&mut self, frame: Frame) -> crate::Result<Fragment> {
		let pts = super::base_ticks(&frame, self.timescale)?;
		// A keyframe may open a new group after a gap the durations didn't cover, so each
		// group re-anchors the decode timeline at its keyframe's presentation time. When
		// the durations tile, the accumulated time already equals it and this is a no-op.
		let dts = match self.dts {
			Some(dts) if !frame.keyframe => dts,
			_ => pts,
		};

		let info = super::FragmentInfo {
			track_id: self.track_id,
			timescale: self.timescale,
			sequence_number: self.sequence,
		};
		let ticks = frame
			.duration
			.map(|duration| super::trun_duration(duration, self.timescale))
			.transpose()?
			.unwrap_or(0);
		let data = super::encode_at(info, dts, std::slice::from_ref(&frame))?;
		self.sequence = self.sequence.wrapping_add(1);

		// Advance by the value the trun stores, so the next tfdt continues exactly what
		// this fragment claimed.
		self.dts = Some(dts.checked_add(u64::from(ticks)).ok_or(super::Error::PtsOverflow)?);

		Ok(Fragment {
			data,
			init: false,
			// Audio has no keyframes, so every audio fragment is independent; video is
			// independent only at a GOP boundary. Matches what the exporter advertises.
			independent: !self.is_video || frame.keyframe,
			duration: fragment_seconds(std::slice::from_ref(&frame), self.default_frame),
		})
	}
}

/// Whether the frame states a usable duration of its own.
fn stated(frame: &Frame) -> bool {
	frame.duration.is_some_and(|duration| !duration.is_zero())
}

#[cfg(test)]
mod tests {
	use bytes::Bytes;
	use hang::catalog::{AudioConfig, VideoCodec, VideoConfig};
	use moq_net::Timestamp;

	use super::super::Muxer;
	use super::*;

	// A 30 fps Legacy VP8 rendition: no description needed, so the muxer builds without media.
	fn video_muxer() -> Muxer {
		let mut config = VideoConfig::new(VideoCodec::VP8);
		config.framerate = Some(30.0);
		Muxer::video(&config).unwrap()
	}

	// One frame at the muxer's own 30_000 timescale, so a frame period is exactly 1000 ticks.
	fn tick_frame(pts: u64, keyframe: bool) -> Frame {
		Frame {
			timestamp: Timestamp::from_scale(pts, 30_000).unwrap(),
			payload: Bytes::from_static(&[0xDE, 0xAD]),
			keyframe,
			duration: Some(Timestamp::from_scale(1_000, 30_000).unwrap()),
		}
	}

	// A duration-less frame, i.e. VFR or Legacy/LOC input.
	fn untimed_frame(pts: u64, keyframe: bool) -> Frame {
		Frame {
			duration: None,
			..tick_frame(pts, keyframe)
		}
	}

	/// The moof sequence number a fragment carries.
	fn sequence(fragment: &Fragment) -> u32 {
		use mp4_atom::DecodeMaybe;

		let mut cursor = std::io::Cursor::new(fragment.data.as_ref());
		while let Some(atom) = mp4_atom::Any::decode_maybe(&mut cursor).unwrap() {
			if let mp4_atom::Any::Moof(moof) = atom {
				return moof.mfhd.sequence_number;
			}
		}
		panic!("no moof");
	}

	// One fragment per frame over a reordered (I, P, B) run: every frame states its duration,
	// so each push emits immediately with no lookahead, tfdt walks the decode timeline, and
	// each cts carries the reorder, so presentation order survives being cut up.
	#[test]
	fn stated_durations_emit_with_no_lookahead() {
		let muxer = video_muxer();
		let timescale = moq_net::Timescale::new(30_000).unwrap();
		let mut fragmenter = muxer.fragmenter(Default::default());
		let input = [tick_frame(0, true), tick_frame(3_000, false), tick_frame(1_000, false)];

		let fragments: Vec<Fragment> = input
			.iter()
			.map(|frame| {
				fragmenter
					.push(frame.clone())
					.unwrap()
					.expect("a stated duration emits immediately")
			})
			.collect();

		let timelines: Vec<_> = fragments.iter().map(|f| super::super::timeline(&f.data)).collect();
		assert_eq!(
			timelines,
			vec![(0, vec![0]), (1_000, vec![2_000]), (2_000, vec![-1_000])],
			"tfdt advances one frame period while cts carries the reorder"
		);

		for (fragment, expected) in fragments.iter().zip(&input) {
			let decoded = super::super::decode(fragment.data.clone(), timescale).unwrap();
			assert_eq!(decoded.len(), 1);
			assert_eq!(decoded[0].timestamp, expected.timestamp, "pts survives the reorder");
		}

		assert!(
			muxer.fragmenter(Default::default()).flush().unwrap().is_none(),
			"nothing pushed, nothing pending"
		);
		assert!(fragmenter.flush().unwrap().is_none(), "every frame was already emitted");
	}

	// A duration-less frame waits one push for its real successor, instead of falling back to
	// the catalog cadence the way a one-frame `Muxer::fragment` call must. This is the drift
	// the fragmenter exists to avoid: at a real 1500-tick cadence the catalog default of 999
	// runs a third short on every sample.
	#[test]
	fn a_pending_frame_is_timed_by_its_real_successor() {
		let muxer = video_muxer();
		let mut fragmenter = muxer.fragmenter(Default::default());
		let input = [
			untimed_frame(0, true),
			untimed_frame(1_500, false),
			untimed_frame(3_000, false),
		];

		assert!(
			fragmenter.push(input[0].clone()).unwrap().is_none(),
			"no duration and no successor yet"
		);
		let first = fragmenter
			.push(input[1].clone())
			.unwrap()
			.expect("times the pending frame");
		let second = fragmenter
			.push(input[2].clone())
			.unwrap()
			.expect("times the pending frame");
		let last = fragmenter.flush().unwrap().expect("the final pending frame");

		let durations: Vec<_> = [&first, &second, &last]
			.iter()
			.map(|f| super::super::sample_durations(&f.data))
			.collect();
		assert_eq!(
			durations,
			vec![vec![Some(1_500)], vec![Some(1_500)], vec![Some(999)]],
			"timed by the successor; only the flushed tail takes the catalog cadence"
		);

		// tfdt advances by exactly what the preceding trun claimed, so the fragments tile.
		let tfdts: Vec<u64> = [&first, &second, &last]
			.iter()
			.map(|f| super::super::timeline(&f.data).0)
			.collect();
		assert_eq!(tfdts, vec![0, 1_500, 3_000]);
	}

	// One push returns at most one fragment. When a stated frame first has to time a pending
	// frame, it remains queued and flush preserves its own duration instead of replacing it.
	#[test]
	fn a_stated_frame_queues_behind_the_pending_fragment() {
		let muxer = video_muxer();
		let mut fragmenter = muxer.fragmenter(Default::default());
		assert!(fragmenter.push(untimed_frame(0, true)).unwrap().is_none());

		let stated = Frame {
			duration: Some(Timestamp::from_scale(1_000, 30_000).unwrap()),
			..tick_frame(1_500, false)
		};
		let first = fragmenter.push(stated).unwrap().expect("the pending frame");
		let second = fragmenter.flush().unwrap().expect("the stated frame");

		assert_eq!(super::super::sample_durations(&first.data), vec![Some(1_500)]);
		assert_eq!(super::super::sample_durations(&second.data), vec![Some(1_000)]);
	}

	// An EXT-X-PART needs a DURATION and an INDEPENDENT flag. The duration has to be the
	// resolved one written into the trun, not the raw PTS gap: the caller has nothing to read
	// for the stream's last frame and would disagree with the media it is describing.
	#[test]
	fn fragments_carry_the_part_metadata() {
		let muxer = video_muxer();
		let mut fragmenter = muxer.fragmenter(Default::default());

		let mut fragments = Vec::new();
		for (pts, keyframe) in [(0u64, true), (1_500, false), (3_000, false)] {
			fragments.extend(fragmenter.push(untimed_frame(pts, keyframe)).unwrap());
		}
		fragments.extend(fragmenter.flush().unwrap());

		assert!(!fragments.iter().any(|f| f.init), "these are media fragments");
		assert_eq!(
			fragments.iter().map(|f| f.independent).collect::<Vec<_>>(),
			vec![true, false, false],
			"video is independent only at a GOP boundary"
		);

		// 1500/30000 and 999/30000: the trun durations, not the 0.05 gap for all three.
		let durations: Vec<_> = fragments.iter().map(|f| (f.duration * 1e6).round() as u64).collect();
		assert_eq!(durations, vec![50_000, 50_000, 33_300], "microseconds");
	}

	// Audio has no keyframes, so every audio part can start a segment. Opus states its own
	// duration in the TOC byte, so no packet ever waits for a successor.
	#[test]
	fn audio_fragments_are_always_independent() {
		let config = AudioConfig::new(hang::catalog::AudioCodec::Opus, 48_000, 2);
		let muxer = Muxer::audio(&config).unwrap();
		let mut fragmenter = muxer.fragmenter(Default::default());

		// Two 20 ms Opus packets; the TOC byte states their duration.
		let packet = Bytes::from_static(&[0x78, 0x00, 0x00, 0x00]);
		for micros in [0u64, 20_000] {
			let frame = Frame {
				timestamp: Timestamp::from_micros(micros).unwrap(),
				payload: packet.clone(),
				keyframe: false,
				duration: None,
			};
			let fragment = fragmenter
				.push(frame)
				.unwrap()
				.expect("the TOC duration emits immediately");
			assert!(fragment.independent, "audio fragments are always independent");
			assert!((fragment.duration - 0.02).abs() < 1e-9, "the 20 ms TOC duration");
		}
	}

	// A keyframe may open a new group, and a group boundary is never a duration: the publisher
	// may have paused across it (the 2405 second sample of moq-dev/moq.pro#814). The pending
	// frame takes the catalog cadence, and the new group re-anchors the decode timeline at its
	// keyframe's presentation time rather than pretending the stream was continuous.
	#[test]
	fn a_new_group_does_not_time_the_pending_frame() {
		let muxer = video_muxer();
		let mut fragmenter = muxer.fragmenter(Default::default());
		// A 40 minute pause between the first group and the second, in 30 kHz ticks.
		let paused_until = 2_405 * 30_000;

		assert!(fragmenter.push(untimed_frame(0, true)).unwrap().is_none());
		let first = fragmenter.push(untimed_frame(1_500, false)).unwrap().expect("timed");
		let second = fragmenter
			.push(untimed_frame(paused_until, true))
			.unwrap()
			.expect("the keyframe flushes the pending frame");
		let third = fragmenter.flush().unwrap().expect("the keyframe itself");

		assert_eq!(super::super::sample_durations(&first.data), vec![Some(1_500)]);
		assert_eq!(
			super::super::sample_durations(&second.data),
			vec![Some(999)],
			"the pause is a discontinuity, not a 2405 second sample"
		);
		assert_eq!(
			super::super::timeline(&third.data).0,
			paused_until,
			"the new group re-anchors at its keyframe's presentation time"
		);
	}

	// The mfhd sequence number is informative, but a per-frame consumer still needs each
	// fragment distinguishable, so the fragmenter numbers them itself.
	#[test]
	fn fragments_number_consecutively() {
		let muxer = video_muxer();
		let mut fragmenter = muxer.fragmenter(Default::default());

		let sequences: Vec<u32> = (0..3)
			.map(|i| {
				let fragment = fragmenter.push(tick_frame(i * 1_000, i == 0)).unwrap().unwrap();
				sequence(&fragment)
			})
			.collect();

		assert_eq!(sequences, vec![0, 1, 2]);
	}

	// A single stated-duration frame comes out byte-identical to the self-anchored
	// `Muxer::fragment` encoding: same anchor, same trun, same sequence number.
	#[test]
	fn one_frame_matches_muxer_fragment() {
		let muxer = video_muxer();
		let frame = tick_frame(5_000, true);

		let batch = muxer.fragment(0, std::slice::from_ref(&frame)).unwrap();
		let pushed = muxer.fragmenter(Default::default()).push(frame).unwrap().unwrap();
		assert_eq!(pushed.data, batch);
	}
}
