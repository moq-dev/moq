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

/// Where a pad's buffers land: a codec importer, or a subtitle track written as WebVTT cues.
///
/// Both payloads are large (a codec importer, a container producer), so each is boxed to keep the
/// enum small.
enum Sink {
	/// `audio` decides grouping. `import::Track` draws no audio boundaries of its own (every
	/// packet is independently decodable, so there is no keyframe to group on), leaving them to
	/// whoever knows the latency target. A live sink wants each packet forwarded without waiting,
	/// so it cuts per frame. Video groups at its own keyframes and needs nothing.
	Media {
		track: Box<import::Track>,
		audio: bool,
	},
	Text(Box<Text>),
}

/// A subtitle pad. GStreamer hands us one decoded cue per buffer (`text/x-raw`, UTF-8) with the
/// presentation time and duration already resolved by the demuxer, so each buffer becomes one
/// self-contained WebVTT segment in its own group, on the same media clock as audio and video.
///
/// The rendition guard is held for the pad's lifetime: dropping it retires the catalog entry, so a
/// pad that fails or finalizes stops advertising a track nobody is writing to.
///
/// No jitter is estimated. The estimator measures the smallest gap between consecutive frames,
/// which for a codec is the frame duration but for cues is just how close two subtitles happen to
/// sit. That says nothing about how long a consumer must buffer, and feeding it to the catalog
/// would inflate every consumer's playback buffer by an arbitrary amount. Each cue is written and
/// cut immediately, so the absent field says what is true: flushed as produced.
struct Text {
	producer: moq_mux::container::Producer<moq_mux::catalog::hang::Container>,
	#[allow(dead_code, reason = "held so dropping the pad retires the catalog rendition")]
	rendition: moq_mux::catalog::TextTrack,
}

impl Text {
	/// Publish one cue spanning `[start, start + duration)` on the media clock.
	///
	/// A cue with no text after escaping is skipped rather than published: the `vtt` format carries
	/// an explicit end time, so there is nothing for an empty cue to express.
	fn write(&mut self, text: &str, start_micros: u64, duration_micros: u64) -> Result<()> {
		let cue = escape_cue(text);
		if cue.is_empty() {
			return Ok(());
		}

		let payload = format!(
			"WEBVTT\n\n{} --> {}\n{}\n",
			format_timestamp(start_micros),
			format_timestamp(start_micros + duration_micros),
			cue
		);

		self.producer.write(moq_mux::container::Frame {
			timestamp: moq_net::Timestamp::from_micros(start_micros)?,
			duration: None,
			payload: Bytes::from(payload.into_bytes()),
			keyframe: true,
		})?;
		// One cue per group, so a late joiner tunes in on the current caption.
		self.producer.cut(None)?;
		Ok(())
	}
}

/// Format microseconds as a WebVTT `HH:MM:SS.mmm` timestamp.
fn format_timestamp(micros: u64) -> String {
	let ms = micros / 1000;
	format!(
		"{:02}:{:02}:{:02}.{:03}",
		ms / 3_600_000,
		(ms / 60_000) % 60,
		(ms / 1000) % 60,
		ms % 1000
	)
}

/// Make arbitrary demuxer text safe to drop into a WebVTT cue block.
///
/// Three things in the payload can corrupt the block rather than just render oddly: `<` and `&`
/// open a WebVTT tag or escape, a blank line terminates the cue early (silently truncating a
/// multi-line caption), and a line containing `-->` reads as another cue's timing. Escaping the
/// markup characters covers the first and the third (`-->` becomes `--&gt;`); dropping blank lines
/// covers the second.
///
/// Markup is escaped rather than forwarded, so a source that already carries `<i>` shows the tag
/// instead of italics. That's the deliberate trade: the demuxers we read from (`qtdemux` on tx3g)
/// hand us plain text, and passing arbitrary tags through risks an unbalanced one swallowing the
/// caption. Forwarding a safelist of WebVTT-legal tags is the upgrade path if a source needs it.
fn escape_cue(text: &str) -> String {
	text.trim_end()
		.lines()
		// A blank line would end the cue, so drop it: the surrounding lines stay in one caption.
		.filter(|line| !line.trim().is_empty())
		// `&` first, so it doesn't double-escape the ampersands the others introduce.
		.map(|line| line.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;"))
		.collect::<Vec<_>>()
		.join("\n")
}

/// One sink pad's media producer plus its timeline policy.
pub struct Pad {
	track: Option<Sink>,
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

	/// (Re)build the producer when the pad's caps change. A build failure invalidates only this pad
	/// (`failed` is set); the caller keeps the session and other pads alive. Identical caps re-sent as a
	/// sticky event keep the live producer.
	pub fn observe_caps(
		&mut self,
		broadcast: &moq_net::broadcast::Producer,
		catalog: &moq_mux::catalog::Producer,
		caps: &gst::Caps,
	) {
		if self.failed || (self.track.is_some() && self.caps.as_deref() == Some(caps)) {
			return;
		}
		if let Err(err) = self.build(broadcast, catalog, caps) {
			gst::warning!(CAT, "invalidating pad: {err:?}");
			self.fail();
		}
	}

	fn build(
		&mut self,
		broadcast: &moq_net::broadcast::Producer,
		catalog: &moq_mux::catalog::Producer,
		caps: &gst::Caps,
	) -> Result<()> {
		let structure = caps.structure(0).context("empty caps")?;
		// Renegotiation: finalize the previous producer before replacing it (closed once, not abandoned).
		self.finalize()?;
		let mut broadcast = broadcast.clone();
		let catalog = catalog.clone();
		// Every codec converges on one import::Track; only the caps -> importer construction differs. The
		// pad template fixes the structural fields (h264/h265 byte-stream/au, AAC mpegversion=4/stream-format=raw),
		// so negotiation rejects non-conforming caps before they reach here; only fields the template can't
		// pin (the AAC codec_data) are checked below. The importer reserves a uniquely named track, which it
		// accepts (setting the timescale) inside `Track::new`.
		// Subtitles skip the codec importers entirely: the demuxer already resolved each cue to UTF-8
		// text with a presentation time, so there is nothing to parse, only a text rendition to declare.
		if structure.name().as_str() == "text/x-raw" {
			self.track = Some(Sink::Text(Box::new(Self::reserve_text(
				&mut broadcast,
				catalog,
				structure,
			)?)));
			self.caps = Some(caps.clone());
			return Ok(());
		}

		let (track, audio): (import::Track, bool) = match structure.name().as_str() {
			"video/x-h264" => (Self::reserve(&mut broadcast, catalog, ".avc3", "avc3", &[])?, false),
			"video/x-h265" => (Self::reserve(&mut broadcast, catalog, ".hev1", "hev1", &[])?, false),
			"video/x-av1" => (Self::reserve(&mut broadcast, catalog, ".av01", "av01", &[])?, false),
			"video/x-vp8" => (Self::reserve(&mut broadcast, catalog, ".vp8", "vp8", &[])?, false),
			"video/x-vp9" => (Self::reserve(&mut broadcast, catalog, ".vp9", "vp9", &[])?, false),
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
				let name = broadcast.unique_name(".mp3");
				let request = broadcast.reserve_track(name)?;
				let producer = request.accept(hang::container::track_info());
				(
					moq_mux::codec::mp3::Import::new(producer, catalog.reserve(), config.into())?.into(),
					true,
				)
			}
			"audio/mpeg" => {
				// AAC: the AudioSpecificConfig rides in caps as codec_data, not in the bitstream.
				let codec_data = structure
					.get::<gst::Buffer>("codec_data")
					.context("AAC caps missing codec_data")?;
				let map = codec_data.map_readable().context("failed to map AAC codec_data")?;
				(
					Self::reserve(&mut broadcast, catalog, ".aac", "aac", map.as_slice())?,
					true,
				)
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
				let name = broadcast.unique_name(".opus");
				let request = broadcast.reserve_track(name)?;
				let producer = request.accept(hang::container::track_info());
				(
					moq_mux::codec::opus::Import::new(producer, catalog.reserve(), config.into())?.into(),
					true,
				)
			}
			other => anyhow::bail!("unsupported caps: {other}"),
		};
		self.track = Some(Sink::Media {
			track: Box::new(track),
			audio,
		});
		self.caps = Some(caps.clone());
		Ok(())
	}

	/// Declare a subtitle rendition: a plain track plus its catalog entry. Unlike the codec paths there
	/// is no config to detect from the bitstream, so the entry is set complete up front, and the
	/// returned guard retires it when the pad goes away.
	fn reserve_text(
		broadcast: &mut moq_net::broadcast::Producer,
		catalog: moq_mux::catalog::Producer,
		structure: &gst::StructureRef,
	) -> Result<Text> {
		let name = broadcast.unique_name(".vtt");
		let request = broadcast.reserve_track(name.clone())?;
		let producer = request.accept(hang::container::track_info());

		let mut config = hang::catalog::TextConfig::new(hang::catalog::TextFormat::Vtt);
		// A demuxed text track is a subtitle track unless something says otherwise. Claiming
		// `caption` would advertise a transcription of non-speech audio we have no evidence for.
		config.role = hang::catalog::TextRole::Subtitle;
		// GStreamer surfaces the track language as a BCP-47-ish tag when the container carries one.
		config.lang = structure.get::<String>("language-code").ok();

		// Go through the reservation like every codec pad, so the first catalog snapshot waits for
		// this track and dropping the rendition removes it again.
		let mut rendition = catalog.reserve().text(name);
		rendition.set(config);

		Ok(Text {
			producer: moq_mux::container::Producer::new(producer, moq_mux::catalog::hang::Container::Legacy),
			rendition,
		})
	}

	/// Reserve a uniquely named track and hand it to the single-codec importer, which accepts the request
	/// (setting the microsecond timescale) and registers the catalog rendition once the config resolves.
	fn reserve(
		broadcast: &mut moq_net::broadcast::Producer,
		catalog: moq_mux::catalog::Producer,
		suffix: &str,
		format: &str,
		init: &[u8],
	) -> Result<import::Track> {
		let name = broadcast.unique_name(suffix);
		let request = broadcast.reserve_track(name)?;
		Ok(import::Track::new(
			request,
			catalog.reserve(),
			import::Init::new(format, init.to_vec()),
		)?)
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
	pub fn push_buffer(&mut self, data: Bytes, pts: Option<gst::ClockTime>, duration: Option<gst::ClockTime>) -> bool {
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
				let result: Result<()> = match self.track.as_mut().expect("track present") {
					Sink::Media { track, audio } => {
						let ts = hang::container::Timestamp::from_micros(micros).ok();
						track
							.decode(&data, ts)
							// One group (one QUIC stream) per audio packet, so the relay forwards
							// it without waiting for the next. See `Sink::Media`.
							.and_then(|()| if *audio { track.cut(None) } else { Ok(()) })
							.map_err(Into::into)
					}
					Sink::Text(text) => match std::str::from_utf8(&data) {
						// A cue with no duration would never be dismissed, so drop it rather than pin it
						// on screen; the demuxer supplies one for every real subtitle sample.
						Ok(cue) => match duration {
							Some(duration) => text.write(cue, micros, duration.useconds()),
							None => {
								gst::warning!(CAT, "dropping subtitle cue without a duration");
								return false;
							}
						},
						Err(err) => Err(anyhow::anyhow!("subtitle cue is not valid UTF-8: {err}")),
					},
				};
				if let Err(err) = result {
					gst::warning!(CAT, "invalidating pad: {err}");
					self.fail();
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
		let Some(track) = self.track.take() else {
			return Ok(false);
		};
		match track {
			Sink::Media { mut track, .. } => track.finish()?,
			Sink::Text(mut text) => text.producer.finish()?,
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
		"video/x-h264"
			| "video/x-h265"
			| "video/x-av1"
			| "video/x-vp8"
			| "video/x-vp9"
			| "audio/mpeg"
			| "audio/x-opus"
			| "text/x-raw"
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

	// Opus caps must carry channels/rate; a missing field fails the pad rather than silently defaulting
	// to stereo/48k (which would misadvertise the stream).
	#[test]
	fn opus_caps_without_channels_fails_the_pad() {
		gst::init().unwrap();
		let (broadcast, catalog) = producers();
		let mut pad = Pad::new();
		let caps = gst::Caps::builder("audio/x-opus").field("rate", 48_000i32).build();
		pad.observe_caps(&broadcast, &catalog, &caps);
		assert!(pad.is_failed(), "Opus without channels fails the pad");
	}

	// A pad with caps but no TIME segment drops buffers and reports the missing timeline exactly once,
	// so the element surfaces it on the bus instead of dropping every frame in silence.
	#[test]
	fn no_time_segment_reports_once() {
		gst::init().unwrap();
		let (broadcast, catalog) = producers();
		let mut pad = Pad::new();
		pad.observe_caps(&broadcast, &catalog, &h264_caps());
		// No observe_segment: the pad stays in NoSegment.
		assert!(
			pad.push_buffer(h264_keyframe_au(), Some(gst::ClockTime::ZERO), None),
			"first no-segment buffer is reported"
		);
		assert!(
			!pad.push_buffer(h264_keyframe_au(), Some(gst::ClockTime::ZERO), None),
			"subsequent no-segment buffers are not re-reported"
		);
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
		pad.push_buffer(Bytes::from_static(b"x"), Some(gst::ClockTime::ZERO), None);
	}

	// A real IDR AU emits a frame to the published track (not just a rendition off the SPS).
	#[tokio::test]
	async fn frame_through_h264_emits_a_frame() {
		gst::init().unwrap();
		let (broadcast, catalog) = producers();
		let mut pad = Pad::new();
		pad.observe_caps(&broadcast, &catalog, &h264_caps());
		pad.observe_segment(time_segment());
		pad.push_buffer(h264_keyframe_au(), Some(gst::ClockTime::ZERO), None);

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

	fn text_caps() -> gst::Caps {
		gst::Caps::builder("text/x-raw").field("format", "utf8").build()
	}

	// A subtitle pad declares a complete rendition up front, since nothing about a cue track is
	// detected from the payload.
	#[test]
	fn text_caps_declares_a_subtitle_rendition() {
		gst::init().unwrap();
		let (broadcast, catalog) = producers();
		let mut pad = Pad::new();
		pad.observe_caps(&broadcast, &catalog, &text_caps());
		assert!(!pad.is_failed());

		let snapshot = catalog.snapshot();
		let (name, config) = snapshot.text.renditions.iter().next().expect("a text rendition");
		assert!(name.ends_with(".vtt"), "unexpected track name: {name}");
		assert_eq!(config.format, hang::catalog::TextFormat::Vtt);
		// Not `Caption`: a demuxed text track carries no evidence that it transcribes non-speech audio.
		assert_eq!(config.role, hang::catalog::TextRole::Subtitle);
	}

	// Dropping the pad retires the rendition, so a failed or finished subtitle track stops being
	// advertised instead of pointing at a producer that is gone.
	#[test]
	fn failed_text_pad_retires_its_rendition() {
		gst::init().unwrap();
		let (broadcast, catalog) = producers();
		let mut pad = Pad::new();
		pad.observe_caps(&broadcast, &catalog, &text_caps());
		pad.observe_segment(time_segment());
		assert_eq!(catalog.snapshot().text.renditions.len(), 1);

		// Invalid UTF-8 fails the pad, which finalizes and drops the producer.
		pad.push_buffer(
			Bytes::from_static(&[0xff, 0xfe]),
			Some(gst::ClockTime::ZERO),
			Some(gst::ClockTime::from_seconds(1)),
		);
		assert!(pad.is_failed());
		assert!(
			catalog.snapshot().text.renditions.is_empty(),
			"failed pad left a phantom rendition"
		);
	}

	// Cue spacing is not a buffering requirement. Publishing it as `jitter` would inflate every
	// consumer's shared playback buffer, so toggling captions on would re-anchor audio and video.
	#[test]
	fn text_rendition_declares_no_jitter() {
		gst::init().unwrap();
		let (broadcast, catalog) = producers();
		let mut pad = Pad::new();
		pad.observe_caps(&broadcast, &catalog, &text_caps());
		pad.observe_segment(time_segment());

		// Two cues 500ms apart: the estimator's minimum-gap heuristic would report 500ms here.
		for (start_ms, dur_ms) in [(0u64, 400u64), (500, 400)] {
			pad.push_buffer(
				Bytes::from_static(b"hello"),
				Some(gst::ClockTime::from_mseconds(start_ms)),
				Some(gst::ClockTime::from_mseconds(dur_ms)),
			);
		}
		assert!(!pad.is_failed());

		let snapshot = catalog.snapshot();
		let config = snapshot.text.renditions.values().next().expect("a text rendition");
		assert_eq!(config.jitter, None, "cue spacing leaked into the catalog as jitter");
	}

	// A cue with no duration has no end, so it would pin on screen forever: drop it instead.
	#[test]
	fn text_cue_without_duration_is_dropped() {
		gst::init().unwrap();
		let (broadcast, catalog) = producers();
		let mut pad = Pad::new();
		pad.observe_caps(&broadcast, &catalog, &text_caps());
		pad.observe_segment(time_segment());
		pad.push_buffer(Bytes::from_static(b"hello"), Some(gst::ClockTime::ZERO), None);
		assert!(!pad.is_failed(), "a durationless cue drops the buffer, not the pad");
	}

	#[test]
	fn vtt_timestamps_are_hms() {
		assert_eq!(format_timestamp(0), "00:00:00.000");
		assert_eq!(format_timestamp(1_500_000), "00:00:01.500");
		assert_eq!(format_timestamp(3_661_042_000), "01:01:01.042");
	}

	// Cue text is arbitrary demuxer output: markup characters must not open a WebVTT tag, and a
	// blank line must not truncate the cue at the first paragraph break.
	#[test]
	fn cue_text_is_escaped() {
		assert_eq!(escape_cue("<i>hi</i>"), "&lt;i&gt;hi&lt;/i&gt;");
		assert_eq!(escape_cue("Tom & Jerry"), "Tom &amp; Jerry");
		// `&` is escaped first, so the ampersand it introduces isn't escaped again.
		assert_eq!(escape_cue("a & <b"), "a &amp; &lt;b");
		assert_eq!(escape_cue("first\n\nsecond"), "first\nsecond");
		// An arrow in the text would otherwise read as another cue's timing.
		assert!(!escape_cue("00:01 --> 00:02").contains("-->"));
		// Nothing but whitespace leaves no cue to publish.
		assert_eq!(escape_cue(" \n\n \n"), "");
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
