//! Per-pad media state: caps -> producer, SEGMENT/running-time policy, frame import.
//!
//! Pure media logic with no GStreamer threading. The aggregator funnels every serialized event and
//! buffer for a pad onto its single aggregate thread, so this type is touched from one place and needs
//! no generation tagging or cross-thread failure map.

use anyhow::{Context, Result};
use bytes::Bytes;

use hang::moq_net;
use moq_mux::import::{Framed, FramedFormat};

use super::session::CAT;
use super::timeline::{SegmentInfo, classify_segment, frame_micros};

/// Per-pad timeline state. Buffers only map and emit while `Active`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PadState {
	/// No valid SEGMENT seen yet.
	NoSegment,
	/// A valid timeline is anchored.
	Active,
	/// A live timeline broke (discontinuity, non-TIME, or rate != 1.0); buffers drop until a valid
	/// SEGMENT re-anchors the pad.
	Invalid,
}

/// One sink pad's media producer plus its timeline policy.
pub struct Pad {
	framed: Option<Framed>,
	caps: Option<gst::Caps>,
	/// Set once a producer build rejects this pad's caps or bitstream; further buffers are dropped and
	/// the track stays finalized. Isolated to the pad, so the session and other pads keep going.
	failed: bool,
	state: PadState,
	segment_info: Option<SegmentInfo>,
	/// Kept only to map a buffer PTS to a running time.
	segment: Option<gst::FormattedSegment<gst::ClockTime>>,
}

impl Pad {
	pub fn new() -> Self {
		Self {
			framed: None,
			caps: None,
			failed: false,
			state: PadState::NoSegment,
			segment_info: None,
			segment: None,
		}
	}

	/// True once this pad has been invalidated by a bad caps/bitstream; the caller drops its buffers.
	pub fn is_failed(&self) -> bool {
		self.failed
	}

	/// (Re)build the producer when the pad's caps change. A build failure invalidates only this pad
	/// (`failed` is set); the caller keeps the session and other pads alive. Identical caps re-sent as a
	/// sticky event keep the live producer.
	pub fn observe_caps(
		&mut self,
		broadcast: &moq_net::BroadcastProducer,
		catalog: &moq_mux::catalog::Producer,
		caps: &gst::Caps,
	) {
		if self.failed || (self.framed.is_some() && self.caps.as_deref() == Some(caps)) {
			return;
		}
		if let Err(err) = self.build(broadcast, catalog, caps) {
			gst::warning!(CAT, "invalidating pad: {err:?}");
			self.fail();
		}
	}

	fn build(
		&mut self,
		broadcast: &moq_net::BroadcastProducer,
		catalog: &moq_mux::catalog::Producer,
		caps: &gst::Caps,
	) -> Result<()> {
		let structure = caps.structure(0).context("empty caps")?;
		// Renegotiation: finalize the previous producer before replacing it (closed once, not abandoned).
		self.finalize()?;
		let broadcast = broadcast.clone();
		let catalog = catalog.clone();
		// Every codec converges on one Framed; only the caps -> producer construction differs. The pad
		// template fixes the structural fields (h264/h265 byte-stream/au, AAC mpegversion=4/stream-format=raw),
		// so negotiation rejects non-conforming caps before they reach here; only fields the template can't
		// pin (the AAC codec_data) are checked below.
		let framed: Framed = match structure.name().as_str() {
			"video/x-h264" => Framed::new(broadcast, catalog, FramedFormat::Avc3, &mut Bytes::new())?,
			"video/x-h265" => Framed::new(broadcast, catalog, FramedFormat::Hev1, &mut Bytes::new())?,
			"video/x-av1" => Framed::new(broadcast, catalog, FramedFormat::Av01, &mut Bytes::new())?,
			"video/x-vp8" => Framed::new(broadcast, catalog, FramedFormat::Vp8, &mut Bytes::new())?,
			"video/x-vp9" => Framed::new(broadcast, catalog, FramedFormat::Vp9, &mut Bytes::new())?,
			"audio/mpeg" => {
				// AAC: the AudioSpecificConfig rides in caps as codec_data, not in the bitstream.
				let codec_data = structure
					.get::<gst::Buffer>("codec_data")
					.context("AAC caps missing codec_data")?;
				let map = codec_data.map_readable().context("failed to map AAC codec_data")?;
				let mut data = Bytes::copy_from_slice(map.as_slice());
				Framed::new(broadcast, catalog, FramedFormat::Aac, &mut data)?
			}
			"audio/x-opus" => {
				// Opus: GStreamer gives channels/rate in caps (not an OpusHead), so build the config here.
				let channels: i32 = structure.get("channels").unwrap_or(2);
				let rate: i32 = structure.get("rate").unwrap_or(48_000);
				let channel_count = u32::try_from(channels)
					.with_context(|| format!("Opus caps has negative channel count {channels}"))?;
				let sample_rate =
					u32::try_from(rate).with_context(|| format!("Opus caps has negative sample rate {rate}"))?;
				let config = moq_mux::codec::opus::Config {
					sample_rate,
					channel_count,
				};
				moq_mux::codec::opus::Import::new(broadcast, catalog, config)?.into()
			}
			other => anyhow::bail!("unsupported caps: {other}"),
		};
		self.framed = Some(framed);
		self.caps = Some(caps.clone());
		Ok(())
	}

	/// Drops the producer (closing its track) and marks the pad failed so further buffers are dropped.
	fn fail(&mut self) {
		if let Err(err) = self.finalize() {
			gst::warning!(CAT, "finalize on failed pad: {err:?}");
		}
		self.failed = true;
	}

	/// Record a SEGMENT, re-anchoring the timeline. Only acts on a change, so a sticky re-send does not
	/// spam rejections. An `Active` pad enforces continuity against its previous segment; `NoSegment` and
	/// `Invalid` re-anchor from scratch on the next valid one.
	pub fn observe_segment(&mut self, segment: gst::Segment) {
		let info = segment_info(&segment);
		// Re-observing the exact segment we last classified is a no-op. observe_segment runs on every
		// buffer (the segment is sticky), so without this an Invalidated pad would re-anchor on the next
		// buffer (Invalid -> prev=None -> classify accepts) and silently recover on the same rewound
		// segment. Recording `info` even on reject is what makes this hold.
		if self.segment_info == Some(info) {
			return;
		}
		let prev = match self.state {
			PadState::Active => self.segment_info,
			PadState::NoSegment | PadState::Invalid => None,
		};
		self.segment_info = Some(info);
		match classify_segment(prev.as_ref(), &info) {
			Ok(()) => {
				self.segment = segment.downcast::<gst::ClockTime>().ok();
				self.state = PadState::Active;
			}
			Err(reason) => {
				gst::warning!(CAT, "rejecting segment: {reason}");
				// A break only invalidates a live timeline; a bad segment before any valid one leaves
				// the pad in NoSegment.
				if self.state == PadState::Active {
					self.state = PadState::Invalid;
				}
			}
		}
	}

	/// Re-anchor on FLUSH. A flushing seek rewinds running time, so the timeline must restart: dropping
	/// the segment moves the pad to NoSegment (the next SEGMENT is accepted fresh via `prev = None`). The
	/// producer is kept (FLUSH is not EOS); the codec's partial-AU reset is a documented follow-up.
	pub fn flush(&mut self) {
		self.state = PadState::NoSegment;
		self.segment = None;
		self.segment_info = None;
	}

	/// Maps a buffer PTS to a MoQ timestamp without enforcing frame-level monotonicity: frames arrive in
	/// decode order and B-frames carry non-monotonic presentation timestamps, so a PTS regression is
	/// normal reordering. Timeline breaks are caught at the SEGMENT level (the `Invalid` state).
	fn frame_timestamp(&self, pts: Option<gst::ClockTime>) -> Result<u64, &'static str> {
		match self.state {
			PadState::Active => {
				// to_running_time_full is signed: a buffer before the segment returns Negative, which
				// frame_micros drops; to_running_time would instead clip it to None and lose the reason.
				let running_time = self
					.segment
					.as_ref()
					.zip(pts)
					.and_then(|(segment, pts)| segment.to_running_time_full(pts))
					.and_then(signed_nanos);
				frame_micros(running_time)
			}
			PadState::NoSegment => Err("buffer before a valid SEGMENT"),
			PadState::Invalid => Err("buffer on an invalidated timeline"),
		}
	}

	/// Import one buffer into the producer. A failed or producer-less pad drops the buffer; a timeline
	/// drop is logged. A bad bitstream (or an oversized frame, rejected by moq-net) invalidates only this
	/// pad.
	pub fn push_buffer(&mut self, mut data: Bytes, pts: Option<gst::ClockTime>) {
		if self.failed {
			return;
		}
		let timestamp = self.frame_timestamp(pts);
		let Some(framed) = self.framed.as_mut() else {
			gst::warning!(CAT, "dropping buffer received before caps");
			return;
		};
		match timestamp {
			Ok(micros) => {
				let ts = hang::container::Timestamp::from_micros(micros).ok();
				if let Err(err) = framed.decode_frame(&mut data, ts) {
					gst::warning!(CAT, "invalidating pad: {err}");
					self.fail();
				}
			}
			Err(reason) => gst::warning!(CAT, "dropping frame: {reason}"),
		}
	}

	/// Consumes the producer so a second call is a no-op (`Framed::finish()` is not idempotent). Returns
	/// whether a producer was finalized.
	pub fn finalize(&mut self) -> Result<bool> {
		// take() up front makes this attempt-once: after a failed finish() the producer is already gone.
		let Some(mut framed) = self.framed.take() else {
			return Ok(false);
		};
		// A lazy codec (H.265/AV1/VP8/VP9) given CAPS but no frame never created its track, so there is
		// nothing to flush and finish() would error "not initialized". track() is Ok only once a track
		// exists; a real finish error on an initialized one still surfaces.
		if framed.track().is_ok() {
			framed.finish()?;
		}
		Ok(true)
	}
}

/// Media types moqsink can build a producer for. Checked synchronously at the CAPS event so an
/// unsupported type is rejected with NotNegotiated. The structural fields (byte-stream/au, AAC
/// mpegversion/stream-format) are pinned by the pad template, so negotiation enforces them.
pub fn caps_supported(caps: &gst::CapsRef) -> bool {
	let Some(s) = caps.structure(0) else { return false };
	matches!(
		s.name().as_str(),
		"video/x-h264" | "video/x-h265" | "video/x-av1" | "video/x-vp8" | "video/x-vp9" | "audio/mpeg" | "audio/x-opus"
	)
}

fn segment_info(segment: &gst::Segment) -> SegmentInfo {
	match segment.downcast_ref::<gst::ClockTime>() {
		Some(time) => SegmentInfo {
			time_format: true,
			rate: time.rate(),
			base_nanos: time.base().map(|c| c.nseconds()).unwrap_or(0),
		},
		None => SegmentInfo {
			time_format: false,
			rate: segment.rate(),
			base_nanos: 0,
		},
	}
}

/// Flattens a signed running time to nanos, keeping the sign so the timeline can drop negatives.
/// None on overflow of u64 nanos into i64 (unreachable in practice).
fn signed_nanos(running_time: gst::Signed<gst::ClockTime>) -> Option<i64> {
	match running_time {
		gst::Signed::Positive(time) => i64::try_from(time.nseconds()).ok(),
		gst::Signed::Negative(time) => i64::try_from(time.nseconds()).ok().map(|nanos| -nanos),
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	/// Local producers, no network: a broadcast plus its catalog, exactly what the aggregate thread holds.
	fn producers() -> (moq_net::BroadcastProducer, moq_mux::catalog::Producer) {
		let mut broadcast = moq_net::Broadcast::new().produce();
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast).unwrap();
		(broadcast, catalog)
	}

	fn h264_caps() -> gst::Caps {
		gst::Caps::builder("video/x-h264")
			.field("stream-format", "byte-stream")
			.field("alignment", "au")
			.build()
	}

	/// A real Annex-B AU (SPS + PPS + IDR) so the importer publishes a rendition and a frame.
	fn h264_keyframe_au() -> Bytes {
		let sps: &[u8] = &[
			0x67, 0x42, 0xc0, 0x1f, 0xda, 0x01, 0x40, 0x16, 0xe9, 0xb8, 0x08, 0x08, 0x0a, 0x00, 0x00, 0x07, 0xd0, 0x00,
			0x01, 0xd4, 0xc0, 0x80,
		];
		let pps: &[u8] = &[0x68, 0xce, 0x3c, 0x80];
		let idr: &[u8] = &[0x65, 0x88, 0x84, 0x00, 0x21];
		let mut au = Vec::new();
		for nal in [sps, pps, idr] {
			au.extend_from_slice(&[0, 0, 0, 1]);
			au.extend_from_slice(nal);
		}
		Bytes::from(au)
	}

	fn time_segment() -> gst::Segment {
		let mut segment = gst::FormattedSegment::<gst::ClockTime>::new();
		segment.set_start(gst::ClockTime::ZERO);
		segment.upcast()
	}

	fn time_segment_at(start_ms: u64, base_ms: u64) -> gst::Segment {
		let mut segment = gst::FormattedSegment::<gst::ClockTime>::new();
		segment.set_start(gst::ClockTime::from_mseconds(start_ms));
		segment.set_base(gst::ClockTime::from_mseconds(base_ms));
		segment.upcast()
	}

	// A supported caps builds a producer; finalize is attempt-once.
	#[test]
	fn supported_caps_builds_a_producer() {
		gst::init().unwrap();
		let (broadcast, catalog) = producers();
		let mut pad = Pad::new();
		pad.observe_caps(&broadcast, &catalog, &h264_caps());
		assert!(!pad.is_failed());
		assert!(pad.finalize().unwrap(), "a producer was built");
		assert!(!pad.finalize().unwrap(), "second finalize is a no-op");
	}

	// AAC carries its config in caps; without codec_data the producer cannot be built.
	#[test]
	fn aac_without_codec_data_fails_the_pad() {
		gst::init().unwrap();
		let (broadcast, catalog) = producers();
		let mut pad = Pad::new();
		let caps = gst::Caps::builder("audio/mpeg")
			.field("mpegversion", 4i32)
			.field("stream-format", "raw")
			.build();
		pad.observe_caps(&broadcast, &catalog, &caps);
		assert!(pad.is_failed(), "AAC without codec_data fails the pad");
	}

	// An unsupported media type fails the pad rather than the session.
	#[test]
	fn unsupported_caps_fails_the_pad() {
		gst::init().unwrap();
		let (broadcast, catalog) = producers();
		let mut pad = Pad::new();
		pad.observe_caps(&broadcast, &catalog, &gst::Caps::builder("video/x-raw").build());
		assert!(pad.is_failed());
	}

	// A failed pad drops further buffers (and never panics) instead of writing them.
	#[test]
	fn failed_pad_drops_buffers() {
		gst::init().unwrap();
		let (broadcast, catalog) = producers();
		let mut pad = Pad::new();
		pad.observe_caps(&broadcast, &catalog, &gst::Caps::builder("video/x-raw").build());
		assert!(pad.is_failed());
		pad.observe_segment(time_segment());
		pad.push_buffer(Bytes::from_static(b"x"), Some(gst::ClockTime::ZERO));
	}

	// A real IDR AU emits a frame to the published track (not just a rendition off the SPS).
	#[test]
	fn frame_through_h264_emits_a_frame() {
		gst::init().unwrap();
		let (broadcast, catalog) = producers();
		let mut pad = Pad::new();
		pad.observe_caps(&broadcast, &catalog, &h264_caps());
		pad.observe_segment(time_segment());
		pad.push_buffer(h264_keyframe_au(), Some(gst::ClockTime::ZERO));

		let snapshot = catalog.snapshot();
		let track = snapshot.video.renditions.keys().next().expect("a video rendition");
		assert!(
			broadcast
				.consume()
				.subscribe_track(&moq_net::Track::new(track))
				.expect("the rendition track is published")
				.latest()
				.is_some(),
			"the IDR AU emitted a frame to the track"
		);
	}

	// A regressing PTS within an Active timeline still emits: frames arrive in decode order and B-frames
	// carry non-monotonic presentation timestamps, so a PTS regression is reordering, not an error.
	#[test]
	fn regressing_pts_within_an_active_timeline_still_emits() {
		gst::init().unwrap();
		let mut pad = Pad::new();
		pad.observe_segment(time_segment_at(0, 0));
		assert_eq!(
			pad.frame_timestamp(Some(gst::ClockTime::from_mseconds(10_000))),
			Ok(10_000_000)
		);
		assert_eq!(
			pad.frame_timestamp(Some(gst::ClockTime::from_mseconds(6_000))),
			Ok(6_000_000)
		);
	}

	// Running time is shared, so two pads keep their A/V offset through real segments.
	#[test]
	fn two_pads_keep_av_aligned_through_real_segments() {
		gst::init().unwrap();
		let mut video = Pad::new();
		let mut audio = Pad::new();
		video.observe_segment(time_segment());
		audio.observe_segment(time_segment());
		assert_eq!(video.frame_timestamp(Some(gst::ClockTime::from_mseconds(7))), Ok(7_000));
		assert_eq!(audio.frame_timestamp(Some(gst::ClockTime::from_mseconds(5))), Ok(5_000));
	}

	// A pad with no SEGMENT drops buffers (NoSegment), distinct from an invalidated timeline.
	#[test]
	fn pad_without_segment_drops_buffers() {
		let pad = Pad::new();
		assert_eq!(pad.state, PadState::NoSegment);
		assert!(pad.frame_timestamp(Some(gst::ClockTime::from_mseconds(5))).is_err());
	}

	// A moved media start stays continuous as long as the running-time base advances.
	#[test]
	fn moved_start_with_advancing_base_stays_continuous() {
		gst::init().unwrap();
		let mut pad = Pad::new();
		pad.observe_segment(time_segment_at(0, 0));
		assert_eq!(pad.state, PadState::Active);
		pad.observe_segment(time_segment_at(30_000, 5_000));
		assert_eq!(pad.state, PadState::Active);
	}

	// A buffer before the segment start yields a negative running time: drop it, never clamp to zero.
	#[test]
	fn frame_before_segment_start_is_dropped_not_clamped() {
		gst::init().unwrap();
		let mut pad = Pad::new();
		pad.observe_segment(time_segment_at(10_000, 0));
		assert!(pad.frame_timestamp(Some(gst::ClockTime::from_mseconds(5_000))).is_err());
		assert_eq!(
			pad.frame_timestamp(Some(gst::ClockTime::from_mseconds(12_000))),
			Ok(2_000_000)
		);
	}

	// A discontinuity invalidates the pad (drops), and the next valid SEGMENT re-anchors it to Active.
	#[test]
	fn invalid_segment_drops_then_a_valid_one_recovers() {
		gst::init().unwrap();
		let mut pad = Pad::new();
		pad.observe_segment(time_segment_at(0, 5_000));
		assert_eq!(pad.state, PadState::Active);

		pad.observe_segment(time_segment_at(0, 0));
		assert_eq!(pad.state, PadState::Invalid, "a rewinding base is discontinuous");

		pad.observe_segment(time_segment_at(0, 10_000));
		assert_eq!(pad.state, PadState::Active, "a valid SEGMENT re-anchors");
	}

	// observe_segment runs on every buffer, so a sticky rewound segment is re-observed repeatedly. Once
	// it has invalidated the pad, re-seeing the SAME segment must keep it Invalid (not flap back to
	// Active); only a genuinely new, valid SEGMENT recovers it.
	#[test]
	fn invalidated_pad_stays_invalid_on_a_resent_segment() {
		gst::init().unwrap();
		let mut pad = Pad::new();
		pad.observe_segment(time_segment_at(0, 5_000));
		assert_eq!(pad.state, PadState::Active);

		pad.observe_segment(time_segment_at(0, 0));
		assert_eq!(pad.state, PadState::Invalid);

		// The same rewound segment, as the next buffer would carry it, must not recover the pad.
		pad.observe_segment(time_segment_at(0, 0));
		assert_eq!(pad.state, PadState::Invalid, "a re-sent rewound segment keeps dropping");
		assert!(pad.frame_timestamp(Some(gst::ClockTime::ZERO)).is_err());
	}

	// FLUSH re-anchors to NoSegment, so a rewinding post-flush segment is accepted fresh, not rejected.
	#[test]
	fn flush_reanchors_so_a_rewinding_segment_recovers() {
		gst::init().unwrap();
		let mut pad = Pad::new();
		pad.observe_segment(time_segment_at(0, 5_000));
		assert_eq!(pad.state, PadState::Active);

		pad.flush();
		assert_eq!(pad.state, PadState::NoSegment, "flush re-anchors to NoSegment");

		pad.observe_segment(time_segment_at(0, 0));
		assert_eq!(pad.state, PadState::Active, "post-flush rewinding segment is accepted");
		assert_eq!(pad.frame_timestamp(Some(gst::ClockTime::ZERO)), Ok(0));
	}

	// FLUSH is not EOS: the producer survives a flush; only the timeline re-anchors.
	#[test]
	fn flush_keeps_the_producer() {
		gst::init().unwrap();
		let (broadcast, catalog) = producers();
		let mut pad = Pad::new();
		pad.observe_caps(&broadcast, &catalog, &h264_caps());
		pad.observe_segment(time_segment());

		pad.flush();
		assert_eq!(pad.state, PadState::NoSegment, "the timeline re-anchored");
		assert!(pad.finalize().unwrap(), "flush keeps the producer");
	}

	// Flushing a pad that never saw CAPS is a no-op, not a panic.
	#[test]
	fn flush_before_caps_is_a_noop() {
		let mut pad = Pad::new();
		pad.flush();
		assert!(!pad.is_failed());
		assert!(!pad.finalize().unwrap(), "no producer to finalize");
	}

	// All decode-order frames, including B-frames, emit: frame_timestamp must not gate on PTS monotonicity.
	#[test]
	fn bframes_in_decode_order_all_emit() {
		gst::init().unwrap();
		let mut pad = Pad::new();
		pad.observe_segment(time_segment());
		let decode_order_pts_ms = [0u64, 160, 40, 80, 120];
		let emitted = decode_order_pts_ms
			.into_iter()
			.filter(|&ms| pad.frame_timestamp(Some(gst::ClockTime::from_mseconds(ms))).is_ok())
			.count();
		assert_eq!(emitted, 5, "all five decode-order frames must emit (got {emitted})");
	}
}
