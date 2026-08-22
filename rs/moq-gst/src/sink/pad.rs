//! Per-pad media state: caps -> producer, SEGMENT/running-time policy, frame import.
//!
//! Pure media logic with no GStreamer threading. GStreamer serializes a pad's events and buffers on
//! that pad's own streaming thread, so this type is touched from one thread and needs no generation
//! tagging or cross-thread failure map.

use anyhow::{Context, Result, ensure};
use bytes::Bytes;

use hang::moq_net;
use moq_mux::import;

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

/// What a pad publishes into. Opaque data has no codec to import through, so it writes frames
/// straight onto the track producer.
enum Producer {
	/// Boxed to keep the variants comparable in size (clippy::large_enum_variant).
	Media(Box<import::Track>),
	Opaque(moq_net::track::Producer),
}

/// One sink pad's producer plus its timeline policy.
pub struct Pad {
	track: Option<Producer>,
	caps: Option<gst::Caps>,
	/// Set once a producer build rejects this pad's caps or bitstream; further buffers are dropped and
	/// the track stays finalized. Isolated to the pad, so the session and other pads keep going.
	failed: bool,
	state: PadState,
	segment_info: Option<SegmentInfo>,
	/// Kept only to map a buffer PTS to a running time.
	segment: Option<gst::FormattedSegment<gst::ClockTime>>,
	/// Set once we have surfaced "buffers but no TIME segment" on the bus, so it is reported once per
	/// pad rather than per dropped frame.
	no_segment_reported: bool,
}

impl Pad {
	/// A fresh pad with no caps, no segment, and no producer yet.
	pub fn new() -> Self {
		Self {
			track: None,
			caps: None,
			failed: false,
			state: PadState::NoSegment,
			segment_info: None,
			segment: None,
			no_segment_reported: false,
		}
	}

	/// True once this pad has been invalidated by a bad caps/bitstream; the caller drops its buffers.
	pub fn is_failed(&self) -> bool {
		self.failed
	}

	/// (Re)build the producer when the pad's caps change, under `track` when the pad was given a name.
	/// Returns the name the broadcast reserved, so the caller can publish it. A build failure invalidates
	/// only this pad; the caller keeps the session and other pads alive. Identical caps re-sent as a
	/// sticky event keep the live producer.
	pub fn observe_caps(
		&mut self,
		broadcast: &moq_net::broadcast::Producer,
		catalog: &moq_mux::catalog::Producer,
		caps: &gst::Caps,
		track: Option<&str>,
	) -> Option<String> {
		if self.failed || (self.track.is_some() && self.caps.as_deref() == Some(caps)) {
			return None;
		}
		match self.build(broadcast, catalog, caps, track) {
			Ok(name) => Some(name),
			Err(err) => {
				gst::warning!(CAT, "invalidating pad: {err:?}");
				self.fail();
				None
			}
		}
	}

	fn build(
		&mut self,
		broadcast: &moq_net::broadcast::Producer,
		catalog: &moq_mux::catalog::Producer,
		caps: &gst::Caps,
		requested: Option<&str>,
	) -> Result<String> {
		let structure = caps.structure(0).context("empty caps")?;
		// Renegotiation: finalize the previous producer before replacing it (closed once, not abandoned).
		self.finalize()?;
		// Opaque data has no codec importer and no catalog entry, so it never reaches the codec match.
		if structure.name() == "application/octet-stream" {
			// A generated name would leave the track unfindable: nothing advertises it.
			let name = requested
				.context("an opaque data pad requires a track name")?
				.to_owned();
			let mut broadcast = broadcast.clone();
			let request = broadcast.reserve_track(name.clone())?;
			// Followed at the live edge, so it keeps the default retention the media helper raises.
			let info = moq_net::track::Info::default().with_timescale(moq_net::Timescale::MICRO);
			self.track = Some(Producer::Opaque(request.accept(info)));
			self.caps = Some(caps.clone());
			return Ok(name);
		}
		let mut broadcast = broadcast.clone();
		let catalog = catalog.clone();
		// Every codec converges on one import::Track; only the caps -> importer construction differs. The
		// pad template fixes the structural fields (h264/h265 byte-stream/au, AAC mpegversion=4/stream-format=raw),
		// so negotiation rejects non-conforming caps before they reach here; only fields the template can't
		// pin (the AAC codec_data) are checked below. The importer reserves the pad's track, which it
		// accepts (setting the timescale) inside `Track::new`.
		let (track, name): (import::Track, String) = match structure.name().as_str() {
			"video/x-h264" => Self::reserve(&mut broadcast, catalog, requested, ".avc3", "avc3", &[])?,
			"video/x-h265" => Self::reserve(&mut broadcast, catalog, requested, ".hev1", "hev1", &[])?,
			"video/x-av1" => Self::reserve(&mut broadcast, catalog, requested, ".av01", "av01", &[])?,
			"video/x-vp8" => Self::reserve(&mut broadcast, catalog, requested, ".vp8", "vp8", &[])?,
			"video/x-vp9" => Self::reserve(&mut broadcast, catalog, requested, ".vp9", "vp9", &[])?,
			// MP3: no config blob to parse (the config lives in each frame header), so the importer is
			// built straight from the caps rate/channels. Keyed on `layer == 3`, which positively
			// identifies Layer III: AAC (`audio/mpeg`, no layer field) and MP2 (`layer=2`) fall through
			// to the AAC arm below.
			"audio/mpeg" if structure.get::<i32>("layer").ok() == Some(3) => {
				let rate: i32 = structure.get("rate").context("MP3 caps missing rate")?;
				let channels: i32 = structure.get("channels").context("MP3 caps missing channels")?;
				ensure!(rate > 0, "MP3 caps has non-positive sample rate {rate}");
				ensure!(channels > 0, "MP3 caps has non-positive channel count {channels}");
				let config = moq_mux::codec::mp3::Config {
					sample_rate: rate as u32,
					channel_count: channels as u32,
				};
				// MP3 builds its config from caps, so like Opus it constructs the codec importer
				// directly and lifts it into a `Track` via `.into()`.
				let name = Self::track_name(&broadcast, requested, ".mp3");
				let request = broadcast.reserve_track(name.clone())?;
				let producer = request.accept(hang::container::track_info());
				(
					moq_mux::codec::mp3::Import::new(producer, catalog.reserve(), config.into())?.into(),
					name,
				)
			}
			"audio/mpeg" => {
				// AAC: the AudioSpecificConfig rides in caps as codec_data, not in the bitstream.
				let codec_data = structure
					.get::<gst::Buffer>("codec_data")
					.context("AAC caps missing codec_data")?;
				let map = codec_data.map_readable().context("failed to map AAC codec_data")?;
				Self::reserve(&mut broadcast, catalog, requested, ".aac", "aac", map.as_slice())?
			}
			"audio/x-opus" => {
				// Opus: GStreamer carries channels/rate in caps (not an OpusHead), and valid Opus caps
				// always include them. Require them rather than guessing a stereo/48k default that could
				// misadvertise the stream.
				let channels: i32 = structure.get("channels").context("Opus caps missing channels")?;
				let rate: i32 = structure.get("rate").context("Opus caps missing rate")?;
				ensure!(channels > 0, "Opus caps has non-positive channel count {channels}");
				ensure!(
					channels <= 2,
					"multichannel Opus is not supported yet (channels={channels})"
				);
				ensure!(rate > 0, "Opus caps has non-positive sample rate {rate}");
				let config = moq_mux::codec::opus::Config::new(rate as u32, channels as u32);
				// Opus builds its config from caps (not an OpusHead init buffer), so it constructs the codec
				// importer directly and lifts it into a `Track` via `.into()`.
				let name = Self::track_name(&broadcast, requested, ".opus");
				let request = broadcast.reserve_track(name.clone())?;
				let producer = request.accept(hang::container::track_info());
				(
					moq_mux::codec::opus::Import::new(producer, catalog.reserve(), config.into())?.into(),
					name,
				)
			}
			other => anyhow::bail!("unsupported caps: {other}"),
		};
		self.track = Some(Producer::Media(Box::new(track)));
		self.caps = Some(caps.clone());
		Ok(name)
	}

	/// The name this pad publishes under: the one it was given, else a generated `0{suffix}`.
	fn track_name(broadcast: &moq_net::broadcast::Producer, requested: Option<&str>, suffix: &str) -> String {
		requested
			.map(str::to_owned)
			.unwrap_or_else(|| broadcast.unique_name(suffix))
	}

	/// Reserve the pad's track and hand it to the single-codec importer, which accepts the request
	/// (setting the microsecond timescale) and registers the catalog rendition once the config resolves.
	/// Returns the reserved name alongside the importer.
	fn reserve(
		broadcast: &mut moq_net::broadcast::Producer,
		catalog: moq_mux::catalog::Producer,
		requested: Option<&str>,
		suffix: &str,
		format: &str,
		init: &[u8],
	) -> Result<(import::Track, String)> {
		let name = Self::track_name(broadcast, requested, suffix);
		let request = broadcast.reserve_track(name.clone())?;
		Ok((
			import::Track::new(request, catalog.reserve(), import::Init::new(format, init.to_vec()))?,
			name,
		))
	}

	/// Drops the producer (closing its track) and marks the pad failed so further buffers are dropped.
	fn fail(&mut self) {
		if let Err(err) = self.finalize() {
			gst::warning!(CAT, "finalize on failed pad: {err:?}");
		}
		self.failed = true;
	}

	/// Record a SEGMENT, re-anchoring the timeline. An `Active` pad enforces continuity against its
	/// previous segment; `NoSegment` and `Invalid` re-anchor from scratch on the next valid one.
	pub fn observe_segment(&mut self, segment: gst::Segment) {
		let info = segment_info(&segment);
		// Skip only a non-Active pad re-seeing the same classification. That stops an Invalidated pad from
		// re-anchoring on the next sticky buffer (Invalid -> prev=None -> classify accepts) and recovering
		// on the same rewound segment. An Active pad always re-runs so it refreshes `self.segment`:
		// `SegmentInfo` omits `start`, so a SEGMENT with the same base/rate but a moved start must still
		// update the segment used for PTS -> running-time mapping.
		if self.segment_info == Some(info) && self.state != PadState::Active {
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
	/// Returns `true` the first time a buffer is dropped because the pad has no TIME segment, so the
	/// caller can surface it once on the bus: without a timeline the pad can never publish.
	pub fn push_buffer(&mut self, data: Bytes, pts: Option<gst::ClockTime>) -> bool {
		if self.failed {
			return false;
		}
		let timestamp = self.frame_timestamp(pts);
		if self.track.is_none() {
			gst::warning!(CAT, "dropping buffer received before caps");
			return false;
		}
		match timestamp {
			Ok(micros) => {
				let ts = hang::container::Timestamp::from_micros(micros).ok();
				// Resolved before acting on it: `fail()` needs `self` back.
				let result = match self.track.as_mut().expect("track present") {
					Producer::Media(track) => Some(track.decode(&data, ts).map_err(|err| err.to_string())),
					// A raw frame carries its timestamp, so there is nothing to publish without one.
					Producer::Opaque(producer) => {
						ts.map(|ts| producer.write_frame(ts, &data).map_err(|err| err.to_string()))
					}
				};
				match result {
					Some(Err(err)) => {
						gst::warning!(CAT, "invalidating pad: {err}");
						self.fail();
					}
					Some(Ok(())) => {}
					None => gst::warning!(CAT, "dropping frame: timestamp out of range"),
				}
				false
			}
			Err(reason) => {
				gst::warning!(CAT, "dropping frame: {reason}");
				// A pad stuck in NoSegment has no timeline and will never publish; report it once.
				let first = self.state == PadState::NoSegment && !self.no_segment_reported;
				self.no_segment_reported |= first;
				first
			}
		}
	}

	/// Consumes the producer so a second call is a no-op (`Track::finish()` is not idempotent). Returns
	/// whether a producer was finalized. The importer accepts its track up front (in `Track::new`), so
	/// `finish()` is safe even when no frame was ever decoded.
	pub fn finalize(&mut self) -> Result<bool> {
		// take() up front makes this attempt-once: after a failed finish() the producer is already gone.
		let Some(mut track) = self.track.take() else {
			return Ok(false);
		};
		match &mut track {
			Producer::Media(track) => track.finish()?,
			Producer::Opaque(producer) => producer.finish()?,
		}
		Ok(true)
	}
}

/// Media types moqsink can build a producer for, plus `application/octet-stream` for opaque data.
/// Checked synchronously at the CAPS event so an unsupported type is rejected with NotNegotiated. The
/// structural fields (byte-stream/au, AAC mpegversion/stream-format) are pinned by the pad template,
/// so negotiation enforces them.
pub fn caps_supported(caps: &gst::CapsRef) -> bool {
	let Some(s) = caps.structure(0) else { return false };
	matches!(
		s.name().as_str(),
		"video/x-h264"
			| "video/x-h265"
			| "video/x-av1"
			| "video/x-vp8"
			| "video/x-vp9"
			| "audio/mpeg"
			| "audio/x-opus"
			| "application/octet-stream"
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

	/// Local producers, no network: a broadcast plus its catalog, exactly what the element holds.
	fn producers() -> (moq_net::broadcast::Producer, moq_mux::catalog::Producer) {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast).unwrap();
		(broadcast, catalog)
	}

	fn h264_caps() -> gst::Caps {
		gst::Caps::builder("video/x-h264")
			.field("stream-format", "byte-stream")
			.field("alignment", "au")
			.build()
	}

	fn opaque_caps() -> gst::Caps {
		gst::Caps::builder("application/octet-stream").build()
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
		pad.observe_caps(&broadcast, &catalog, &h264_caps(), None);
		assert!(!pad.is_failed());
		assert!(pad.finalize().unwrap(), "a producer was built");
		assert!(!pad.finalize().unwrap(), "second finalize is a no-op");
	}

	// A named pad reserves that track instead of the generated one, and the catalog advertises the same
	// name (the rendition resolves off the SPS, so it needs one AU).
	#[test]
	fn an_explicit_name_reaches_the_broadcast_and_the_catalog() {
		gst::init().unwrap();
		let (broadcast, catalog) = producers();
		let mut pad = Pad::new();
		assert_eq!(
			pad.observe_caps(&broadcast, &catalog, &h264_caps(), Some("camera")),
			Some("camera".to_string()),
			"the reserved name is the requested one"
		);
		pad.observe_segment(time_segment());
		pad.push_buffer(h264_keyframe_au(), Some(gst::ClockTime::ZERO));

		let snapshot = catalog.snapshot();
		let renditions: Vec<String> = snapshot.video.renditions.keys().map(|name| name.to_string()).collect();
		assert_eq!(renditions, ["camera"], "the catalog advertises the explicit name");
	}

	// Without a name the generated one is kept, and it is reported so the element can publish it.
	#[test]
	fn a_generated_name_is_reported_too() {
		gst::init().unwrap();
		let (broadcast, catalog) = producers();
		let mut pad = Pad::new();
		assert_eq!(
			pad.observe_caps(&broadcast, &catalog, &h264_caps(), None),
			Some("0.avc3".to_string())
		);
	}

	// A name another pad already holds invalidates only the second pad: the broadcast, the catalog and
	// the first pad's producer survive.
	#[test]
	fn a_colliding_name_invalidates_only_that_pad() {
		gst::init().unwrap();
		let (broadcast, catalog) = producers();
		let mut first = Pad::new();
		let mut second = Pad::new();
		assert_eq!(
			first.observe_caps(&broadcast, &catalog, &h264_caps(), Some("camera")),
			Some("camera".to_string())
		);
		assert_eq!(
			second.observe_caps(&broadcast, &catalog, &h264_caps(), Some("camera")),
			None,
			"the duplicate reservation is rejected"
		);
		assert!(second.is_failed(), "the collision fails the second pad");
		assert!(!first.is_failed(), "the first pad keeps its producer");
		assert!(first.finalize().unwrap(), "the first producer is still live");
	}

	// Renegotiation re-reserves the same explicit name. `build` finalizes the old producer first and the
	// broadcast reclaims closed entries on insert, so the pad does not collide with itself.
	#[test]
	fn renegotiation_keeps_the_explicit_name() {
		gst::init().unwrap();
		let (broadcast, catalog) = producers();
		let mut pad = Pad::new();
		assert_eq!(
			pad.observe_caps(&broadcast, &catalog, &h264_caps(), Some("camera")),
			Some("camera".to_string())
		);
		let renegotiated = gst::Caps::builder("video/x-h264")
			.field("stream-format", "byte-stream")
			.field("alignment", "au")
			.field("width", 1280i32)
			.build();
		assert_eq!(
			pad.observe_caps(&broadcast, &catalog, &renegotiated, Some("camera")),
			Some("camera".to_string()),
			"the same name is reserved again, not rejected as a duplicate"
		);
		assert!(!pad.is_failed());
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
		pad.observe_caps(&broadcast, &catalog, &caps, None);
		assert!(pad.is_failed(), "AAC without codec_data fails the pad");
	}

	// Opus caps must carry channels/rate; a missing field fails the pad rather than silently defaulting
	// to stereo/48k (which would misadvertise the stream).
	#[test]
	fn opus_caps_without_channels_fails_the_pad() {
		gst::init().unwrap();
		let (broadcast, catalog) = producers();
		let mut pad = Pad::new();
		let caps = gst::Caps::builder("audio/x-opus").field("rate", 48_000i32).build();
		pad.observe_caps(&broadcast, &catalog, &caps, None);
		assert!(pad.is_failed(), "Opus without channels fails the pad");
	}

	// A pad with caps but no TIME segment drops buffers and reports the missing timeline exactly once,
	// so the element surfaces it on the bus instead of dropping every frame in silence.
	#[test]
	fn no_time_segment_reports_once() {
		gst::init().unwrap();
		let (broadcast, catalog) = producers();
		let mut pad = Pad::new();
		pad.observe_caps(&broadcast, &catalog, &h264_caps(), None);
		// No observe_segment: the pad stays in NoSegment.
		assert!(
			pad.push_buffer(h264_keyframe_au(), Some(gst::ClockTime::ZERO)),
			"first no-segment buffer is reported"
		);
		assert!(
			!pad.push_buffer(h264_keyframe_au(), Some(gst::ClockTime::ZERO)),
			"subsequent no-segment buffers are not re-reported"
		);
	}

	// An unsupported media type fails the pad rather than the session.
	#[test]
	fn unsupported_caps_fails_the_pad() {
		gst::init().unwrap();
		let (broadcast, catalog) = producers();
		let mut pad = Pad::new();
		pad.observe_caps(&broadcast, &catalog, &gst::Caps::builder("video/x-raw").build(), None);
		assert!(pad.is_failed());
	}

	// An opaque track nobody can name is unfindable: it is absent from the catalog by design, so a
	// generated name would publish bytes no consumer could ask for.
	#[test]
	fn an_opaque_pad_requires_a_name() {
		gst::init().unwrap();
		let (broadcast, catalog) = producers();
		let mut pad = Pad::new();
		assert_eq!(pad.observe_caps(&broadcast, &catalog, &opaque_caps(), None), None);
		assert!(
			pad.is_failed(),
			"an unnamed opaque pad fails instead of generating a name"
		);
	}

	// MSF defines no packaging for raw bytes, so the opaque track is not advertised. The media pad's
	// rendition still resolves, which also shows the opaque pad never reserved a catalog slot: an
	// unresolved reservation would hold the snapshot back.
	#[test]
	fn an_opaque_pad_stays_out_of_the_catalog() {
		gst::init().unwrap();
		let (broadcast, catalog) = producers();
		let mut video = Pad::new();
		let mut data = Pad::new();
		video.observe_caps(&broadcast, &catalog, &h264_caps(), Some("camera"));
		data.observe_caps(&broadcast, &catalog, &opaque_caps(), Some("audiolevels"));
		assert!(!data.is_failed());
		video.observe_segment(time_segment());
		video.push_buffer(h264_keyframe_au(), Some(gst::ClockTime::ZERO));

		let snapshot = catalog.snapshot();
		let renditions: Vec<String> = snapshot.video.renditions.keys().map(|name| name.to_string()).collect();
		assert_eq!(renditions, ["camera"], "only the media pad is advertised");
	}

	// The data-track contract: bytes out untouched, one buffer per group, stamped with the PTS the TIME
	// segment maps.
	#[tokio::test]
	async fn an_opaque_pad_publishes_raw_bytes_one_group_per_buffer() {
		gst::init().unwrap();
		let (broadcast, catalog) = producers();
		let mut pad = Pad::new();
		assert_eq!(
			pad.observe_caps(&broadcast, &catalog, &opaque_caps(), Some("audiolevels")),
			Some("audiolevels".to_string())
		);
		pad.observe_segment(time_segment());
		// Opens with a zero byte and carries a non-UTF-8 one: nothing here may be reinterpreted.
		pad.push_buffer(
			Bytes::from_static(b"\x00\xffLEVELS"),
			Some(gst::ClockTime::from_mseconds(40)),
		);
		pad.push_buffer(Bytes::from_static(b"second"), Some(gst::ClockTime::from_mseconds(80)));

		let mut subscriber = broadcast
			.consume()
			.track("audiolevels")
			.expect("the opaque track is published")
			.subscribe(None)
			.await
			.expect("subscribe to the opaque track");

		let mut group = subscriber.next_group().await.unwrap().expect("a first group");
		let frame = group.read_frame().await.unwrap().expect("a frame in the first group");
		assert_eq!(
			frame.payload.as_ref(),
			b"\x00\xffLEVELS",
			"the payload goes out untouched"
		);
		assert_eq!(
			std::time::Duration::from(frame.timestamp).as_micros(),
			40_000,
			"the frame carries the PTS mapped through the segment"
		);
		assert!(
			group.read_frame().await.unwrap().is_none(),
			"one buffer produces one group with one frame"
		);

		let mut group = subscriber.next_group().await.unwrap().expect("a second group");
		let frame = group.read_frame().await.unwrap().expect("a frame in the second group");
		assert_eq!(frame.payload.as_ref(), b"second");
		assert_eq!(std::time::Duration::from(frame.timestamp).as_micros(), 80_000);
	}

	// The opaque track declares microseconds so the PTS maps 1:1, and keeps moq-net's retention: the
	// media helper raises it to 30s for a segmented egress reading history, which a data track never is.
	#[tokio::test]
	async fn an_opaque_track_declares_micros_and_the_default_retention() {
		gst::init().unwrap();
		let (broadcast, catalog) = producers();
		let mut pad = Pad::new();
		pad.observe_caps(&broadcast, &catalog, &opaque_caps(), Some("audiolevels"));

		let subscriber = broadcast
			.consume()
			.track("audiolevels")
			.expect("the opaque track is published")
			.subscribe(None)
			.await
			.expect("subscribe to the opaque track");
		assert_eq!(subscriber.info().timescale, moq_net::Timescale::MICRO);
		assert_eq!(
			subscriber.info().latency_max,
			moq_net::track::DEFAULT_LATENCY_MAX,
			"an opaque track keeps the default retention"
		);
	}

	// A buffer with no PTS is dropped rather than stamped with something invented, and the pad keeps
	// publishing the ones that do carry a timestamp.
	#[tokio::test]
	async fn an_opaque_pad_drops_a_buffer_without_pts() {
		gst::init().unwrap();
		let (broadcast, catalog) = producers();
		let mut pad = Pad::new();
		pad.observe_caps(&broadcast, &catalog, &opaque_caps(), Some("audiolevels"));
		pad.observe_segment(time_segment());
		pad.push_buffer(Bytes::from_static(b"no pts"), None);
		pad.push_buffer(Bytes::from_static(b"stamped"), Some(gst::ClockTime::from_mseconds(40)));
		assert!(
			!pad.is_failed(),
			"a missing PTS drops the buffer, it does not fail the pad"
		);

		let mut subscriber = broadcast
			.consume()
			.track("audiolevels")
			.expect("the opaque track is published")
			.subscribe(None)
			.await
			.expect("subscribe to the opaque track");
		let mut group = subscriber.next_group().await.unwrap().expect("a group");
		let frame = group.read_frame().await.unwrap().expect("a frame");
		assert_eq!(
			frame.payload.as_ref(),
			b"stamped",
			"only the stamped buffer was published"
		);
	}

	// A failed pad drops further buffers (and never panics) instead of writing them.
	#[test]
	fn failed_pad_drops_buffers() {
		gst::init().unwrap();
		let (broadcast, catalog) = producers();
		let mut pad = Pad::new();
		pad.observe_caps(&broadcast, &catalog, &gst::Caps::builder("video/x-raw").build(), None);
		assert!(pad.is_failed());
		pad.observe_segment(time_segment());
		pad.push_buffer(Bytes::from_static(b"x"), Some(gst::ClockTime::ZERO));
	}

	// A real IDR AU emits a frame to the published track (not just a rendition off the SPS).
	#[tokio::test]
	async fn frame_through_h264_emits_a_frame() {
		gst::init().unwrap();
		let (broadcast, catalog) = producers();
		let mut pad = Pad::new();
		pad.observe_caps(&broadcast, &catalog, &h264_caps(), None);
		pad.observe_segment(time_segment());
		pad.push_buffer(h264_keyframe_au(), Some(gst::ClockTime::ZERO));

		let snapshot = catalog.snapshot();
		let track = snapshot.video.renditions.keys().next().expect("a video rendition");
		let subscriber = broadcast
			.consume()
			.track(track)
			.expect("the rendition track is published")
			.subscribe(None)
			.await
			.expect("subscribe to the rendition track");
		assert!(subscriber.latest().is_some(), "the IDR AU emitted a frame to the track");
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

	// A new SEGMENT with the same base/rate but a moved `start` must refresh the cached segment, since
	// `SegmentInfo` (the dedup key) omits `start` and the PTS -> running-time mapping depends on it.
	#[test]
	fn moved_start_with_equal_base_refreshes_timestamp_mapping() {
		gst::init().unwrap();
		let mut pad = Pad::new();
		pad.observe_segment(time_segment_at(0, 5_000));
		pad.observe_segment(time_segment_at(3_000, 5_000));
		assert_eq!(
			pad.frame_timestamp(Some(gst::ClockTime::from_mseconds(6_000))),
			Ok(8_000_000)
		);
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
		pad.observe_caps(&broadcast, &catalog, &h264_caps(), None);
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
