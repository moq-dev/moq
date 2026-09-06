//! Stateful per-frame fragmenting on one continuous decode timeline.

use std::time::Duration;

use crate::container::Frame;

use super::Fragment;
use super::export::{apply_codec_durations, infer_missing_duration};

/// A frame waiting for its duration, with its decode-timeline boundary preserved.
pub(super) struct Pending {
	frame: Frame,
	group_start: bool,
}

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
/// [`duration`](Frame::duration) is held until its successor arrives to time it when that is
/// safe. Audio can always infer this way. Video rejects missing durations by default because
/// presentation timestamps cannot reveal decode duration when frames are reordered; a caller
/// that knows its video is presentation ordered can opt into inference with
/// [`fragment::MissingDuration`](super::fragment::MissingDuration). Feed frames in decode
/// order with [`push`](Self::push) as they arrive, and [`flush`](Self::flush) at the end:
///
/// ```no_run
/// # fn example(muxer: &moq_mux::container::fmp4::Muxer, frames: Vec<moq_mux::container::Frame>) -> moq_mux::Result<()> {
/// let mut fragmenter = muxer.fragmenter(moq_mux::container::fmp4::fragment::Config::default());
/// for frame in frames {
///     for fragment in fragmenter.push(frame)? {
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
/// Use [`push_group`](Self::push_group) for the first frame after an actual MoQ group
/// boundary. A mid-group sync sample still uses [`push`](Self::push), even though its
/// [`keyframe`](Frame::keyframe) flag is set.
///
/// A frame that states its own duration (CMAF input, an Opus TOC byte, or a publisher that
/// sets [`Frame::duration`]) emits with no lookahead. If the preceding frame was waiting for
/// this one to determine its duration, the same `push` returns both fragments in decode order.
/// A successor passed to [`push_group`](Self::push_group) is never used as a duration bound,
/// since the publisher may have paused across the boundary; the pending frame then takes the
/// catalog cadence, exactly like the last frame of a
/// [`Muxer::fragment`](super::Muxer::fragment) call.
///
/// Each explicit group start re-anchors the decode timeline at its presentation time. When the
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
	/// Whether a missing duration can be inferred from the successor's presentation time.
	pub(super) infer_missing: bool,
	/// A duration-less frame held until its successor arrives to time it.
	pub(super) pending: Option<Pending>,
	/// The next fragment's decode time in ticks at `timescale`; `None` before the first frame.
	pub(super) dts: Option<u64>,
	/// The next moof sequence number.
	pub(super) sequence: u32,
}

impl Fragmenter {
	/// Feed the next frame in decode order, returning every fragment that became ready.
	///
	/// Returns no fragments when the frame must wait for a successor, one in the normal case,
	/// or two when the incoming frame both times a pending frame and states its own duration.
	/// The final pending frame is retrieved with [`flush`](Self::flush).
	pub fn push(&mut self, frame: Frame) -> crate::Result<Vec<Fragment>> {
		self.push_inner(frame, false)
	}

	/// Feed the first frame after an actual MoQ group boundary.
	///
	/// The prior duration-less frame takes the catalog cadence, and this frame re-anchors the
	/// decode timeline at its presentation time. Use [`push`](Self::push) for every other frame,
	/// including a mid-group sync sample.
	pub fn push_group(&mut self, frame: Frame) -> crate::Result<Vec<Fragment>> {
		self.push_inner(frame, true)
	}

	/// Feed one frame with the caller's explicit group-boundary knowledge.
	fn push_inner(&mut self, mut frame: Frame, group_start: bool) -> crate::Result<Vec<Fragment>> {
		// Opus states its duration in the TOC byte; read it now so such a frame never waits.
		apply_codec_durations(std::slice::from_mut(&mut frame), self.opus);
		if !stated(&frame) && !self.infer_missing {
			return Err(super::Error::MissingVideoDuration.into());
		}

		// Build the next state separately so a failure while encoding the second ready frame
		// does not consume the pending frame or advance the timeline without returning output.
		let mut next = self.snapshot();
		let mut fragments = Vec::new();

		if let Some(mut pending) = next.pending.take() {
			// The incoming frame is the successor that times the pending one. A group boundary
			// is never a duration (the publisher may have paused across it), so the pending frame
			// then takes the catalog cadence.
			let successor = (!group_start).then_some(&frame);
			infer_missing_duration(&mut pending.frame, successor, next.default_frame, next.timescale)?;
			fragments.push(next.emit(pending.frame, pending.group_start)?);
		}

		if stated(&frame) {
			fragments.push(next.emit(frame, group_start)?);
		} else {
			next.pending = Some(Pending { frame, group_start });
		}

		*self = next;
		Ok(fragments)
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
		infer_missing_duration(&mut pending.frame, None, self.default_frame, self.timescale)?;
		Ok(Some(self.emit(pending.frame, pending.group_start)?))
	}

	/// Encode one frame as its own fragment and advance the timeline past it.
	fn emit(&mut self, frame: Frame, group_start: bool) -> crate::Result<Fragment> {
		let pts = super::base_ticks(&frame, self.timescale)?;
		// A group may open after a gap the durations didn't cover, so each group re-anchors
		// the decode timeline at its first frame's presentation time. When
		// the durations tile, the accumulated time already equals it and this is a no-op.
		let dts = match self.dts {
			Some(dts) if !group_start => dts,
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
		// Advance by the value the trun stores, so the next tfdt continues exactly what
		// this fragment claimed.
		let next_dts = dts.checked_add(u64::from(ticks)).ok_or(super::Error::PtsOverflow)?;
		let data = super::encode_at(info, dts, std::slice::from_ref(&frame))?;
		self.sequence = self.sequence.wrapping_add(1);
		self.dts = Some(next_dts);

		Ok(Fragment {
			data,
			// Audio has no keyframes, so every audio fragment is independent; video is
			// independent only at a GOP boundary. Matches what the exporter advertises.
			independent: !self.is_video || frame.keyframe,
			// Describe the exact duration written into trun, including its timescale
			// quantization, so playlist metadata and media advance by the same amount.
			duration: Duration::from_secs_f64(f64::from(ticks) / self.timescale.as_u64() as f64),
		})
	}

	/// Copy the small timeline state so a push that produces two fragments commits atomically.
	fn snapshot(&self) -> Self {
		Self {
			track_id: self.track_id,
			timescale: self.timescale,
			default_frame: self.default_frame,
			is_video: self.is_video,
			opus: self.opus,
			infer_missing: self.infer_missing,
			pending: self.pending.as_ref().map(|pending| Pending {
				frame: pending.frame.clone(),
				group_start: pending.group_start,
			}),
			dts: self.dts,
			sequence: self.sequence,
		}
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

	// Explicitly assert that duration-less video is presentation ordered before inferring from
	// successor PTS. The default rejects it because a later B-frame can invalidate the gap.
	fn infer_video_durations() -> super::super::fragment::Config {
		super::super::fragment::Config {
			missing_duration: super::super::fragment::MissingDuration::InferFromPresentationTime,
		}
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

	/// Unwrap the one fragment expected from this push.
	fn one(mut fragments: Vec<Fragment>) -> Fragment {
		assert_eq!(fragments.len(), 1);
		fragments.pop().unwrap()
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
			.map(|frame| one(fragmenter.push(frame.clone()).unwrap()))
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
	// the fragmenter exists to avoid: a real 1500-tick cadence differs from the catalog's
	// 1000-tick 30 fps default.
	#[test]
	fn a_pending_frame_is_timed_by_its_real_successor() {
		let muxer = video_muxer();
		let mut fragmenter = muxer.fragmenter(infer_video_durations());
		let input = [
			untimed_frame(0, true),
			untimed_frame(1_500, false),
			untimed_frame(3_000, false),
		];

		assert!(
			fragmenter.push(input[0].clone()).unwrap().is_empty(),
			"no duration and no successor yet"
		);
		let first = one(fragmenter.push(input[1].clone()).unwrap());
		let second = one(fragmenter.push(input[2].clone()).unwrap());
		let last = fragmenter.flush().unwrap().expect("the final pending frame");

		let durations: Vec<_> = [&first, &second, &last]
			.iter()
			.map(|f| super::super::sample_durations(&f.data))
			.collect();
		assert_eq!(
			durations,
			vec![vec![Some(1_500)], vec![Some(1_500)], vec![Some(1_000)]],
			"timed by the successor; only the flushed tail takes the catalog cadence"
		);

		// tfdt advances by exactly what the preceding trun claimed, so the fragments tile.
		let tfdts: Vec<u64> = [&first, &second, &last]
			.iter()
			.map(|f| super::super::timeline(&f.data).0)
			.collect();
		assert_eq!(tfdts, vec![0, 1_500, 3_000]);
	}

	// LOC permits each frame to carry its own timescale. Normalize the successor before
	// subtraction so a scale change does not discard the real PTS gap for the pending frame.
	#[test]
	fn a_successor_with_another_scale_times_the_pending_frame() {
		let muxer = video_muxer();
		let mut fragmenter = muxer.fragmenter(infer_video_durations());
		assert!(fragmenter.push(untimed_frame(0, true)).unwrap().is_empty());

		let successor = Frame {
			timestamp: Timestamp::from_micros(50_000).unwrap(),
			duration: None,
			..tick_frame(0, false)
		};
		let first = one(fragmenter.push(successor).unwrap());

		assert_eq!(super::super::sample_durations(&first.data), vec![Some(1_500)]);
	}

	// Convert both instants to an exact common scale only after finding that scale. Converting
	// the 1.5-second successor to the predecessor's 1 Hz scale first would erase this gap.
	#[test]
	fn a_coarse_predecessor_does_not_quantize_the_successor_gap() {
		let muxer = video_muxer();
		let mut fragmenter = muxer.fragmenter(infer_video_durations());
		let first = Frame {
			timestamp: Timestamp::from_secs(1).unwrap(),
			duration: None,
			..tick_frame(0, true)
		};
		let successor = Frame {
			timestamp: Timestamp::from_scale(3, 2).unwrap(),
			duration: None,
			..tick_frame(0, false)
		};

		assert!(fragmenter.push(first).unwrap().is_empty());
		let first = one(fragmenter.push(successor).unwrap());

		assert_eq!(super::super::sample_durations(&first.data), vec![Some(15_000)]);
	}

	// Reduce the rational difference directly against the output scale. The LCM of these
	// legal coprime timestamp scales exceeds the Timestamp range even though the gap is one
	// second and needs only 30,000 output ticks.
	#[test]
	fn a_simple_gap_does_not_require_a_representable_lcm() {
		let muxer = video_muxer();
		let mut fragmenter = muxer.fragmenter(infer_video_durations());
		let start_scale = 4_000_000_007;
		let end_scale = 4_000_000_009;
		let first = Frame {
			timestamp: Timestamp::from_scale(0, start_scale).unwrap(),
			duration: None,
			..tick_frame(0, true)
		};
		let successor = Frame {
			timestamp: Timestamp::from_scale(end_scale, end_scale).unwrap(),
			duration: None,
			..tick_frame(0, false)
		};

		assert!(fragmenter.push(first).unwrap().is_empty());
		let first = one(fragmenter.push(successor).unwrap());

		assert_eq!(super::super::sample_durations(&first.data), vec![Some(30_000)]);
	}

	// Keep the catalog fallback in its native precision too. Converting 1/30 second to the
	// frame's 1 Hz timestamp scale would turn a positive tail duration into zero.
	#[test]
	fn a_coarse_tail_timestamp_keeps_the_catalog_fallback() {
		let muxer = video_muxer();
		let mut fragmenter = muxer.fragmenter(infer_video_durations());
		let tail = Frame {
			timestamp: Timestamp::from_secs(1).unwrap(),
			duration: None,
			..tick_frame(0, true)
		};

		assert!(fragmenter.push(tail).unwrap().is_empty());
		let tail = fragmenter.flush().unwrap().expect("the pending tail");

		assert_eq!(super::super::sample_durations(&tail.data), vec![Some(1_000)]);
	}

	// A stated frame that arrives behind a pending frame is already ready too. Returning both
	// keeps the live edge available even when no later frame arrives and flush is not called.
	#[test]
	fn a_stated_frame_emits_with_the_pending_fragment() {
		let muxer = video_muxer();
		let mut fragmenter = muxer.fragmenter(infer_video_durations());
		assert!(fragmenter.push(untimed_frame(0, true)).unwrap().is_empty());

		let stated = Frame {
			duration: Some(Timestamp::from_scale(1_000, 30_000).unwrap()),
			..tick_frame(1_500, false)
		};
		let fragments = fragmenter.push(stated).unwrap();
		assert_eq!(fragments.len(), 2);
		let first = &fragments[0];
		let second = &fragments[1];

		assert_eq!(super::super::sample_durations(&first.data), vec![Some(1_500)]);
		assert_eq!(super::super::sample_durations(&second.data), vec![Some(1_000)]);
		assert!(fragmenter.flush().unwrap().is_none());
	}

	// The first successor PTS in an I, P, B decode-order run looks like a long I-frame duration,
	// but the later B-frame proves that it was a composition offset. Reject before emitting it.
	#[test]
	fn durationless_reordered_video_is_rejected_by_default() {
		let muxer = video_muxer();
		let mut fragmenter = muxer.fragmenter(Default::default());
		let input = [
			untimed_frame(0, true),
			untimed_frame(3_000, false),
			untimed_frame(1_000, false),
		];

		let err = fragmenter.push(input[0].clone()).unwrap_err();
		assert!(matches!(
			err,
			crate::Error::Cmaf(super::super::Error::MissingVideoDuration)
		));
	}

	// A positive frame duration must remain positive at the chosen output timescale. A zero
	// trun duration would keep tfdt stationary while Fragment::duration still advances.
	#[test]
	fn a_coarse_timescale_rejects_a_sub_tick_duration() {
		let muxer = video_muxer().with_timescale(moq_net::Timescale::SECOND).unwrap();
		let mut fragmenter = muxer.fragmenter(Default::default());

		let err = fragmenter.push(tick_frame(0, true)).unwrap_err();
		assert!(matches!(
			err,
			crate::Error::Cmaf(super::super::Error::SampleDurationTooSmall(1))
		));
	}

	// Flooring every 1/24-second frame to 41 milliseconds would make the stateful decode
	// timeline lose 16 milliseconds per second, so reject an incompatible override.
	#[test]
	fn fragmenter_rejects_an_inexact_sample_duration() {
		let muxer = video_muxer().with_timescale(moq_net::Timescale::MILLI).unwrap();
		let mut fragmenter = muxer.fragmenter(Default::default());
		let input_scale = moq_net::Timescale::new(24).unwrap();
		let frame = Frame {
			timestamp: Timestamp::new(0, input_scale).unwrap(),
			duration: Some(Timestamp::new(1, input_scale).unwrap()),
			..tick_frame(0, true)
		};

		let err = fragmenter.push(frame).unwrap_err();
		assert!(matches!(
			err,
			crate::Error::Cmaf(super::super::Error::SampleDurationInexact(1_000))
		));
	}

	// An EXT-X-PART needs a DURATION and an INDEPENDENT flag. The duration has to be the
	// resolved one written into the trun, not the raw PTS gap: the caller has nothing to read
	// for the stream's last frame and would disagree with the media it is describing.
	#[test]
	fn fragments_carry_the_part_metadata() {
		let muxer = video_muxer();
		let mut fragmenter = muxer.fragmenter(infer_video_durations());

		let mut fragments = Vec::new();
		for (pts, keyframe) in [(0u64, true), (1_500, false), (3_000, false)] {
			fragments.extend(fragmenter.push(untimed_frame(pts, keyframe)).unwrap());
		}
		fragments.extend(fragmenter.flush().unwrap());

		assert_eq!(
			fragments.iter().map(|f| f.independent).collect::<Vec<_>>(),
			vec![true, false, false],
			"video is independent only at a GOP boundary"
		);

		// 1500/30000 and 1000/30000: the trun durations, not the 0.05 gap for all three.
		let durations: Vec<_> = fragments.iter().map(|f| f.duration.as_micros() as u64).collect();
		assert_eq!(durations, vec![50_000, 50_000, 33_333], "microseconds");
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
			let fragment = one(fragmenter.push(frame).unwrap());
			assert!(fragment.independent, "audio fragments are always independent");
			assert_eq!(fragment.duration, Duration::from_millis(20), "the 20 ms TOC duration");
		}
	}

	// A keyframe may open a new group, and a group boundary is never a duration: the publisher
	// may have paused across it (the 2405 second sample of moq-dev/moq.pro#814). The pending
	// frame takes the catalog cadence, and the new group re-anchors the decode timeline at its
	// keyframe's presentation time rather than pretending the stream was continuous.
	#[test]
	fn a_new_group_does_not_time_the_pending_frame() {
		let muxer = video_muxer();
		let mut fragmenter = muxer.fragmenter(infer_video_durations());
		// A 40 minute pause between the first group and the second, in 30 kHz ticks.
		let paused_until = 2_405 * 30_000;

		assert!(fragmenter.push(untimed_frame(0, true)).unwrap().is_empty());
		let first = one(fragmenter.push(untimed_frame(1_500, false)).unwrap());
		let second = one(fragmenter.push_group(untimed_frame(paused_until, true)).unwrap());
		let third = fragmenter.flush().unwrap().expect("the keyframe itself");

		assert_eq!(super::super::sample_durations(&first.data), vec![Some(1_500)]);
		assert_eq!(
			super::super::sample_durations(&second.data),
			vec![Some(1_000)],
			"the pause is a discontinuity, not a 2405 second sample"
		);
		assert_eq!(
			super::super::timeline(&third.data).0,
			paused_until,
			"the new group re-anchors at its keyframe's presentation time"
		);
	}

	// A group may contain another sync sample. Its keyframe bit makes it independently
	// decodable, but only push_group is allowed to reset the continuous decode timeline.
	#[test]
	fn a_mid_group_keyframe_does_not_reanchor_the_timeline() {
		let muxer = video_muxer();
		let mut fragmenter = muxer.fragmenter(Default::default());

		let first = one(fragmenter.push(tick_frame(0, true)).unwrap());
		let second = one(fragmenter.push(tick_frame(1_000, false)).unwrap());
		let sync = one(fragmenter.push(tick_frame(5_000, true)).unwrap());
		let next_group = one(fragmenter.push_group(tick_frame(10_000, true)).unwrap());

		assert_eq!(super::super::timeline(&first.data).0, 0);
		assert_eq!(super::super::timeline(&second.data).0, 1_000);
		assert_eq!(super::super::timeline(&sync.data).0, 2_000);
		assert_eq!(super::super::timeline(&next_group.data).0, 10_000);
		assert!(
			sync.independent,
			"the mid-group sync sample stays independently decodable"
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
				let fragment = one(fragmenter.push(tick_frame(i * 1_000, i == 0)).unwrap());
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
		let pushed = one(muxer.fragmenter(Default::default()).push(frame).unwrap());
		assert_eq!(pushed.data, batch);
	}
}
