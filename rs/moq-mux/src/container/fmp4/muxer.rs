//! One-shot CMAF muxing for individually fetched groups.

use bytes::Bytes;
use hang::catalog::{AudioConfig, Container as CatalogContainer, VideoConfig};

use crate::catalog::hang::Container as HangContainer;
use crate::container::Frame;
use crate::container::source::{VideoTransform, build_video_transform};

use super::export::{
	Fragment, apply_codec_durations, catalog_timescale_audio, catalog_timescale_video, extract_init, fallback_duration,
	fragment_seconds, infer_missing_durations,
};
use super::{Error, synthesize_audio_trak, synthesize_video_trak};

/// The single track id used by a muxer's init segment and fragments.
///
/// A muxer serves one rendition standalone, so the id carries no information; a fixed value
/// keeps a synthesized init and its fragments trivially consistent.
const TRACK_ID: u32 = 1;

/// Whether the muxer serves a video or an audio rendition, with its catalog config.
enum Kind {
	Video(VideoConfig),
	Audio(AudioConfig),
}

/// The rendition's nominal frame period, exact at `timescale`.
///
/// A zero / NaN / infinite catalog framerate would divide the timescale into a non-finite tick
/// count, so it takes the same 30 fps default the rest of the muxer does.
fn default_frame(kind: &Kind, timescale: moq_net::Timescale) -> crate::Result<moq_net::Timestamp> {
	let rate = match kind {
		Kind::Video(config) => config
			.framerate
			.filter(|fps| fps.is_finite() && *fps > 0.0)
			.unwrap_or(30.0),
		// ~1024 samples per packet is the AAC frame; Opus states its own duration anyway.
		Kind::Audio(config) => config.sample_rate.max(1) as f64 / 1024.0,
	};
	fallback_duration(timescale.as_u64(), rate)
}

/// Muxes one rendition's fetched groups into standalone CMAF, without a live subscription.
///
/// The pull-based [`Export`](super::Export) subscribes to a whole broadcast and interleaves
/// its tracks; `Muxer` is the building block for a fetch-on-demand consumer (an HLS/DASH
/// origin) that retrieves one group at a time via
/// [`track::Consumer::fetch_group`](moq_net::track::Consumer::fetch_group):
///
/// 1. [`read`](Self::read) decodes a fetched group into media [`Frame`]s, normalizing the
///    codec shape (Annex-B H.264/H.265 becomes length-prefixed, with the config record
///    synthesized from the in-band parameter sets).
/// 2. [`init`](Self::init) builds the rendition's init segment (ftyp+moov).
/// 3. [`fragment`](Self::fragment) encodes frames as one moof+mdat whose `tfdt` carries their
///    real presentation time, so a fragment built from a mid-stream group stands alone.
///    [`fragments`](Self::fragments) cuts the same run into one fragment per frame, for a
///    consumer that addresses individual frames (LL-HLS Partial Segments).
///
/// For inline-parameter-set codecs (catalog `description` absent), [`init`](Self::init) returns
/// `None` until a group has been [`read`](Self::read) to resolve the config from a keyframe.
pub struct Muxer {
	kind: Kind,
	container: HangContainer,
	transform: Option<VideoTransform>,
	/// Resolved codec config record: the catalog `description`, or synthesized by the
	/// transform from in-band parameter sets.
	description: Option<Bytes>,
	timescale: moq_net::Timescale,
	/// Fallback duration for frames that carry none (Legacy / LOC sources), derived from the
	/// catalog framerate / sample rate and exact at [`Self::timescale`].
	default_frame: moq_net::Timestamp,
	/// True for Opus audio, whose packets state their own duration in the TOC byte.
	opus: bool,
}

impl Muxer {
	/// A muxer for a video rendition described by `config`.
	pub fn video(config: &VideoConfig) -> crate::Result<Self> {
		let container = (&config.container).try_into()?;
		let kind = Kind::Video(config.clone());
		let timescale = moq_net::Timescale::new(catalog_timescale_video(config)?).map_err(Error::from)?;
		Ok(Self {
			container,
			transform: build_video_transform(config),
			description: config.description.as_ref().filter(|b| !b.is_empty()).cloned(),
			default_frame: default_frame(&kind, timescale)?,
			timescale,
			opus: false,
			kind,
		})
	}

	/// A muxer for an audio rendition described by `config`.
	pub fn audio(config: &AudioConfig) -> crate::Result<Self> {
		let container = (&config.container).try_into()?;
		let kind = Kind::Audio(config.clone());
		let timescale = moq_net::Timescale::new(catalog_timescale_audio(config)?).map_err(Error::from)?;
		Ok(Self {
			container,
			transform: None,
			description: config.description.as_ref().filter(|b| !b.is_empty()).cloned(),
			default_frame: default_frame(&kind, timescale)?,
			timescale,
			opus: matches!(config.codec, hang::catalog::AudioCodec::Opus),
			kind,
		})
	}

	/// The media timescale this muxer's init segment and fragments are expressed in.
	///
	/// Derived from the catalog: a `Cmaf` rendition's own init scale, the framerate for video
	/// (`framerate * 1000`, falling back to 90 kHz) and the sample rate for audio, unless
	/// [`with_timescale`](Self::with_timescale) overrode it.
	pub fn timescale(&self) -> moq_net::Timescale {
		self.timescale
	}

	/// Emit at an explicit timescale instead of the one derived from the catalog.
	///
	/// For a consumer whose downstream timeline is fixed, for example an HLS or DASH origin
	/// working in 90 kHz ticks.
	///
	/// Errors for a `Cmaf` rendition, whose init segment passes through from the catalog at its
	/// own scale: overriding would leave the init and the fragments on different timelines. Also
	/// errors above `u32::MAX`, which the init segment's `mdhd.timescale` field cannot hold.
	pub fn with_timescale(mut self, timescale: moq_net::Timescale) -> crate::Result<Self> {
		if matches!(self.catalog_container(), CatalogContainer::Cmaf { .. }) {
			return Err(Error::TimescaleOverride.into());
		}
		// Reject here rather than at init(), so the failure lands on the call that is wrong.
		super::mdhd_timescale(timescale.as_u64())?;
		// The fallback is a tick count at the muxer's scale, so moving the scale moves it too.
		self.default_frame = default_frame(&self.kind, timescale)?;
		self.timescale = timescale;
		Ok(self)
	}

	/// The rendition's catalog container, whichever kind of track this is.
	fn catalog_container(&self) -> &CatalogContainer {
		match &self.kind {
			Kind::Video(config) => &config.container,
			Kind::Audio(config) => &config.container,
		}
	}

	/// Decode one fetched group into media frames, in decode order.
	///
	/// Reads the group to its end, so call it only on a finished group (a live group would
	/// block until the publisher closes it). Parameter-set frames are absorbed into the codec
	/// config record; the group's first emitted frame is marked a keyframe (a group opens on
	/// one by convention).
	pub async fn read(&mut self, group: &mut moq_net::group::Consumer) -> crate::Result<Vec<Frame>> {
		use crate::container::Container as _;

		let mut out: Vec<Frame> = Vec::new();
		while let Some(frames) = self.container.read(group).await? {
			for frame in frames {
				let Some(transform) = self.transform.as_mut() else {
					out.push(frame);
					continue;
				};
				let payload = transform.transform(frame.payload.clone())?;
				// Track the transform's record even after it is first set: a mid-stream
				// reconfiguration rebuilds the avcC/hvcC with new parameter sets.
				if let Some(d) = transform.codec_private()
					&& self.description.as_ref() != Some(d)
				{
					self.description = Some(d.clone());
				}
				if let Some(payload) = payload {
					out.push(Frame { payload, ..frame });
				}
			}
		}
		if let Some(first) = out.first_mut() {
			first.keyframe = true;
		}
		Ok(out)
	}

	/// Build the rendition's CMAF init segment (ftyp+moov), or `None` if it isn't buildable yet.
	///
	/// A `Cmaf` rendition's catalog init passes through (with the track id normalized to match
	/// [`fragment`](Self::fragment)); a `Legacy`/`Loc` rendition's is synthesized from the catalog
	/// config. `None` means an inline-parameter-set video rendition whose codec config hasn't been
	/// resolved yet: [`read`](Self::read) a group (its keyframe carries the parameter sets) and call
	/// again.
	pub fn init(&self) -> crate::Result<Option<Bytes>> {
		// An inline codec carries its config in-band, so the init can't be built until a keyframe
		// group has been read.
		if self.transform.is_some() && self.description.is_none() {
			return Ok(None);
		}

		let mut traks: Vec<mp4_atom::Trak> = Vec::new();
		let mut trexs: Vec<mp4_atom::Trex> = Vec::new();
		let mut ftyp: Option<mp4_atom::Ftyp> = None;

		match self.catalog_container() {
			CatalogContainer::Cmaf { init, .. } => {
				extract_init(init, TRACK_ID, &mut ftyp, &mut traks, &mut trexs)?;
			}
			CatalogContainer::Legacy | CatalogContainer::Loc => {
				let trak = match &self.kind {
					Kind::Video(config) => {
						synthesize_video_trak(TRACK_ID, self.timescale.as_u64(), config, self.description.as_deref())?
					}
					Kind::Audio(config) => synthesize_audio_trak(TRACK_ID, self.timescale.as_u64(), config)?,
				};
				trexs.push(mp4_atom::Trex {
					track_id: trak.tkhd.track_id,
					default_sample_description_index: 1,
					..Default::default()
				});
				traks.push(trak);
			}
			CatalogContainer::Unknown(unknown) => return Err(crate::Error::unsupported_container(unknown)),
		}

		Ok(Some(super::encode_init(ftyp, traks, trexs)?))
	}

	/// Encode frames as one moof+mdat fragment.
	///
	/// The `tfdt` base decode time is the first frame's real presentation timestamp (at the
	/// init segment's timescale), so the fragment is self-contained regardless of which group
	/// it came from. Frames without a duration get one inferred from the following frame's
	/// timestamp (falling back to the catalog frame rate / sample rate), so multi-sample
	/// fragments stay decodable. `sequence` is the moof sequence number, informative only.
	///
	/// `frames` may span several groups, and a sample is never timed by one in the next group
	/// even so: consecutive sequence numbers say nothing about whether the publisher paused
	/// across the boundary.
	///
	/// A caller emitting one fragment per frame wants [`fragments`](Self::fragments), which cuts
	/// a whole run at once, or [`fragment_at`](Self::fragment_at) if it authors the timeline.
	pub fn fragment(&self, sequence: u32, frames: &[Frame]) -> crate::Result<Bytes> {
		let Some(base_dts) = frames.first().map(|f| f.timestamp) else {
			return Ok(Bytes::new());
		};
		let frames = self.resolve_durations(frames);
		Ok(super::encode_fragment_at(self.info(sequence), base_dts, &frames)?)
	}

	/// Encode each frame as its own moof+mdat, timed as one continuous run.
	///
	/// This is [`fragment`](Self::fragment)'s timing with a cut between every sample, for a
	/// consumer whose smallest addressable unit is one frame (an LL-HLS origin cutting Partial
	/// Segments at stored-object boundaries). Durations are resolved across the whole slice
	/// *before* cutting, so each frame is timed by its real successor and each fragment's `tfdt`
	/// advances by exactly what the previous `trun` claimed. Calling `fragment` once per frame
	/// instead re-anchors every fragment, collapsing `cts` to zero and timing every sample by the
	/// catalog cadence.
	///
	/// Reordering survives: `tfdt` walks decode order while each `cts` carries `PTS - DTS`.
	/// Fragment `i` is numbered `sequence + i`. Returns an empty `Vec` when `frames` is empty.
	///
	/// The run's last frame has no successor to be timed by, so it takes the catalog cadence like
	/// any trailing sample. Set [`Frame::duration`] to pin it.
	pub fn fragments(&self, sequence: u32, frames: &[Frame]) -> crate::Result<Vec<Fragment>> {
		let frames = self.resolve_durations(frames);
		let Some(first) = frames.first() else {
			return Ok(Vec::new());
		};

		// Walk the timeline in the track's own ticks: `encode_fragment_at` rescales the anchor
		// the same way, so an accumulated tick count and the durations in the trun can't drift
		// apart the way re-deriving from each frame's PTS would.
		let mut dts = first.timestamp.as_scale(self.timescale) as u64;
		let mut out = Vec::with_capacity(frames.len());

		for (i, frame) in frames.iter().enumerate() {
			let base = moq_net::Timestamp::from_scale(dts, self.timescale.as_u64()).map_err(Error::from)?;
			// The moof sequence number is informative only, so wrapping past u32 is harmless.
			let info = self.info(sequence.wrapping_add(i as u32));
			let data = super::encode_fragment_at(info, base, std::slice::from_ref(frame))?;

			// Advance by the number the trun actually claims, not the untruncated one: they are
			// the same value only because both go through the same checked narrowing.
			if let Some(duration) = frame.duration {
				let ticks = super::trun_duration(duration, self.timescale)?;
				dts = dts.checked_add(u64::from(ticks)).ok_or(Error::PtsOverflow)?;
			}

			out.push(Fragment {
				data,
				init: false,
				// Audio has no keyframes, so every audio fragment can start a segment.
				independent: !self.is_video() || frame.keyframe,
				duration: fragment_seconds(std::slice::from_ref(frame), self.default_frame),
			});
		}

		Ok(out)
	}

	/// Encode frames as one moof+mdat fragment whose decode timeline starts at `base_dts`.
	///
	/// [`fragment`](Self::fragment) anchors each fragment at `frames[0].timestamp`, which keeps
	/// it self-contained but re-anchors on every call. A caller that authors its own decode
	/// timeline passes the DTS it computed, and the composition offsets (`PTS - DTS`) come out
	/// correct, so reordered frames keep their presentation order and `tfdt` stays monotonic
	/// across fragments.
	///
	/// **Every frame must carry a [`duration`](Frame::duration)**, or this errors with
	/// [`Error::MissingFrameDuration`](super::Error::MissingFrameDuration). Inferring one would
	/// put a number in the `trun` that the caller can't reproduce, so its next `base_dts` would
	/// not line up with where this fragment ends and the two would drift apart. Opus is the
	/// exception: its packets state their own duration, which is read from the payload rather
	/// than inferred.
	///
	/// `base_dts` must be monotonically non-decreasing across a track's fragments. Returns an
	/// empty `Bytes` when `frames` is empty.
	///
	/// Prefer [`fragments`](Self::fragments) when the whole run is available: it owns the
	/// timeline, so there is no contract to keep.
	pub fn fragment_at(&self, sequence: u32, base_dts: moq_net::Timestamp, frames: &[Frame]) -> crate::Result<Bytes> {
		let mut frames = frames.to_vec();
		// Opus states its duration in the TOC byte, so reading it back is a parse of what the
		// caller already sent, not an inference the caller would have to guess at.
		apply_codec_durations(&mut frames, self.opus);

		if let Some(i) = frames.iter().position(|f| f.duration.is_none_or(|d| d.is_zero())) {
			return Err(Error::MissingFrameDuration(i).into());
		}

		Ok(super::encode_fragment_at(self.info(sequence), base_dts, &frames)?)
	}

	/// Whether this muxer serves a video rendition.
	fn is_video(&self) -> bool {
		matches!(self.kind, Kind::Video(_))
	}

	/// The per-fragment metadata every encode call shares.
	fn info(&self, sequence_number: u32) -> super::FragmentInfo {
		super::FragmentInfo {
			track_id: TRACK_ID,
			timescale: self.timescale,
			sequence_number,
		}
	}

	/// Fill in the durations the frames don't carry: the codec's own where it states one, then
	/// the gap to the following frame, then the catalog cadence for a trailing sample.
	fn resolve_durations(&self, frames: &[Frame]) -> Vec<Frame> {
		let mut frames = frames.to_vec();
		apply_codec_durations(&mut frames, self.opus);
		infer_missing_durations(frames, None, self.default_frame)
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use hang::catalog::VideoCodec;
	use moq_net::Timestamp;

	fn frame(micros: u64, keyframe: bool) -> Frame {
		Frame {
			timestamp: Timestamp::from_micros(micros).unwrap(),
			payload: Bytes::from_static(&[0xDE, 0xAD]),
			keyframe,
			duration: None,
		}
	}

	// A fetched Legacy group round-trips through the muxer into a self-contained fragment:
	// synthesized init, keyframe-marked first sample, and a tfdt carrying the real PTS.
	#[tokio::test]
	async fn legacy_group_round_trips() {
		let track = moq_net::broadcast::Info::new()
			.produce()
			.create_track("v", None)
			.unwrap();
		let mut subscriber = track.subscribe(None);
		let mut producer = crate::container::Producer::new(track, HangContainer::Legacy);
		producer.write(frame(10_000_000, true)).unwrap();
		producer.write(frame(10_033_000, false)).unwrap();
		producer.finish().unwrap();

		let mut group = subscriber.next_group().await.unwrap().expect("a group");

		let mut muxer = video_muxer();

		let init = muxer.init().unwrap().expect("init buildable for an out-of-band codec");
		assert_eq!(&init[4..8], b"ftyp");

		let frames = muxer.read(&mut group).await.unwrap();
		assert_eq!(frames.len(), 2);
		assert!(frames[0].keyframe, "the group's first frame is a keyframe");

		let fragment = muxer.fragment(7, &frames).unwrap();
		assert_eq!(&fragment[4..8], b"moof");

		// Decode it back: timestamps survive at the muxer's timescale (framerate * 1000).
		let timescale = moq_net::Timescale::new(30_000).unwrap();
		let decoded = super::super::decode(fragment, timescale).unwrap();
		assert_eq!(decoded.len(), 2);
		assert_eq!(decoded[0].timestamp.as_micros(), 10_000_000);
		assert!(decoded[0].keyframe);
		assert_eq!(decoded[1].timestamp.as_micros(), 10_033_000);
	}

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

	// A duration-less frame at a real 1500-tick cadence, i.e. VFR or Legacy/LOC input.
	fn untimed_frame(pts: u64, keyframe: bool) -> Frame {
		Frame {
			timestamp: Timestamp::from_scale(pts, 30_000).unwrap(),
			payload: Bytes::from_static(&[0xDE, 0xAD]),
			keyframe,
			duration: None,
		}
	}

	// The reason `fragments` exists rather than a loop over `fragment`. Cutting per call leaves
	// each fragment a one-element slice with no successor, so every sample falls back to the
	// catalog cadence (1000 ticks, one 30 fps frame) while the frames really arrive 1500 apart.
	// Resolving first and cutting after times each frame by its real successor, so the fragments
	// tile the timeline instead of running a third short.
	#[test]
	fn fragments_time_each_frame_by_its_real_successor() {
		let muxer = video_muxer();
		let input: Vec<Frame> = (0..4).map(|i| untimed_frame(i * 1_500, i == 0)).collect();

		let cut_per_call: Vec<_> = input
			.iter()
			.enumerate()
			.map(|(i, frame)| {
				let fragment = muxer.fragment(i as u32, std::slice::from_ref(frame)).unwrap();
				super::super::trun_durations(&fragment)
			})
			.collect();
		assert_eq!(
			cut_per_call,
			vec![vec![Some(1_000)]; 4],
			"no successor in a one-element slice, so every sample takes the catalog cadence"
		);

		let resolved_first = muxer.fragments(0, &input).unwrap();
		let durations: Vec<_> = resolved_first
			.iter()
			.map(|f| super::super::trun_durations(&f.data))
			.collect();
		assert_eq!(
			durations,
			vec![
				vec![Some(1_500)],
				vec![Some(1_500)],
				vec![Some(1_500)],
				vec![Some(1_000)]
			],
			"real successor gaps, and only the trailing sample has none to read"
		);

		// Each tfdt is where the previous trun said the run had reached.
		let tfdts: Vec<_> = resolved_first
			.iter()
			.map(|f| super::super::timeline(&f.data).0)
			.collect();
		assert_eq!(tfdts, vec![0, 1_500, 3_000, 4_500]);
	}

	// `fragments` advances tfdt by each sample's duration, which the trun stores in 32 bits. A
	// silent narrowing left fragment N claiming a wrapped duration while fragment N+1 started at
	// the untruncated time, so the run stopped tiling. u32::MAX is an accepted timescale, which
	// puts a one-second sample right at the edge of the field.
	#[test]
	fn fragments_reject_a_duration_too_large_for_trun() {
		let scale = moq_net::Timescale::new(u64::from(u32::MAX)).unwrap();
		let muxer = video_muxer().with_timescale(scale).unwrap();

		let two_seconds = 2 * u64::from(u32::MAX);
		let frames: Vec<Frame> = (0..2)
			.map(|i| Frame {
				timestamp: Timestamp::from_scale(i * two_seconds, scale.as_u64()).unwrap(),
				payload: Bytes::from_static(&[0xDE, 0xAD]),
				keyframe: i == 0,
				duration: Some(Timestamp::from_scale(two_seconds, scale.as_u64()).unwrap()),
			})
			.collect();

		let err = muxer.fragments(0, &frames).unwrap_err();
		assert!(
			matches!(err, crate::Error::Cmaf(Error::SampleDurationTooLarge(t)) if t == two_seconds),
			"got {err:?}"
		);

		// One second exactly is u32::MAX ticks at this scale, so it still encodes and tiles.
		let one_second = u64::from(u32::MAX);
		let frames: Vec<Frame> = (0..2)
			.map(|i| Frame {
				timestamp: Timestamp::from_scale(i * one_second, scale.as_u64()).unwrap(),
				payload: Bytes::from_static(&[0xDE, 0xAD]),
				keyframe: i == 0,
				duration: Some(Timestamp::from_scale(one_second, scale.as_u64()).unwrap()),
			})
			.collect();

		let out = muxer.fragments(0, &frames).unwrap();
		let tfdts: Vec<_> = out.iter().map(|f| super::super::timeline(&f.data).0).collect();
		assert_eq!(tfdts, vec![0, one_second], "each tfdt is where the last trun ended");
	}

	// The segment metadata an LL-HLS packager reads back off each part.
	#[test]
	fn fragments_carry_segment_metadata() {
		let muxer = video_muxer();
		let input = [tick_frame(0, true), tick_frame(1_000, false)];
		let fragments = muxer.fragments(7, &input).unwrap();

		assert_eq!(fragments.len(), 2);
		assert!(!fragments[0].init && !fragments[1].init);
		assert!(fragments[0].independent, "opens on a keyframe");
		assert!(!fragments[1].independent, "mid-GOP, so it can't start a segment");
		for fragment in &fragments {
			assert!((fragment.duration - 1.0 / 30.0).abs() < 1e-6, "one 30 fps frame");
		}

		assert!(muxer.fragments(0, &[]).unwrap().is_empty());
	}

	// Audio has no keyframes, so every audio part can start a segment.
	#[tokio::test]
	async fn audio_fragments_are_all_independent() {
		use hang::catalog::AudioCodec;

		let config = AudioConfig::new(AudioCodec::Opus, 48_000, 2);
		let muxer = Muxer::audio(&config).unwrap();
		let packet = Bytes::from_static(&[0x78, 0x00, 0x00, 0x00]);
		let frames: Vec<Frame> = (0..3)
			.map(|i| Frame {
				payload: packet.clone(),
				keyframe: false,
				..frame(i * 20_000, false)
			})
			.collect();

		let fragments = muxer.fragments(0, &frames).unwrap();
		assert_eq!(fragments.len(), 3);
		assert!(fragments.iter().all(|f| f.independent));
	}

	// `fragments` is the same encoder as a correct hand-rolled loop, so a caller already doing
	// the accumulation gets byte-identical output and can migrate without a diff.
	#[test]
	fn fragments_match_an_authored_timeline() {
		let muxer = video_muxer();
		let input = [tick_frame(0, true), tick_frame(3_000, false), tick_frame(1_000, false)];

		let mut base_dts = Timestamp::from_scale(0, 30_000).unwrap();
		let mut authored = Vec::new();
		for (sequence, frame) in input.iter().enumerate() {
			authored.push(
				muxer
					.fragment_at(sequence as u32, base_dts, std::slice::from_ref(frame))
					.unwrap(),
			);
			base_dts = base_dts.checked_add(frame.duration.unwrap()).unwrap();
		}

		let cut: Vec<_> = muxer
			.fragments(0, &input)
			.unwrap()
			.into_iter()
			.map(|f| f.data)
			.collect();
		assert_eq!(cut, authored);
	}

	// fragment_at hands the timeline to the caller, so inventing a duration would put a number in
	// the trun the caller can't reproduce and the two would drift apart.
	#[test]
	fn fragment_at_requires_an_explicit_duration() {
		let muxer = video_muxer();
		let base_dts = Timestamp::from_scale(0, 30_000).unwrap();
		let err = muxer
			.fragment_at(0, base_dts, &[tick_frame(0, true), untimed_frame(1_000, false)])
			.unwrap_err();
		assert!(
			matches!(err, crate::Error::Cmaf(Error::MissingFrameDuration(1))),
			"got {err:?}"
		);

		// A zero duration is as unusable as none, and reported the same way.
		let zeroed = Frame {
			duration: Some(Timestamp::from_scale(0, 30_000).unwrap()),
			..tick_frame(0, true)
		};
		assert!(matches!(
			muxer.fragment_at(0, base_dts, &[zeroed]).unwrap_err(),
			crate::Error::Cmaf(Error::MissingFrameDuration(0))
		));

		// fragment() still infers, so the whole-slice path is unaffected.
		assert!(!muxer.fragment(0, &[untimed_frame(0, true)]).unwrap().is_empty());
	}

	// Opus states its duration in the TOC byte, so reading it back is a parse of what the caller
	// already sent rather than an inference it would have to guess at.
	#[tokio::test]
	async fn fragment_at_reads_opus_durations_from_the_payload() {
		use hang::catalog::AudioCodec;

		let config = AudioConfig::new(AudioCodec::Opus, 48_000, 2);
		let muxer = Muxer::audio(&config).unwrap();
		// 20 ms of 48 kHz Opus: TOC config 15 (SILK wideband, 20 ms).
		let opus = Frame {
			payload: Bytes::from_static(&[0x78, 0x00, 0x00, 0x00]),
			..frame(0, true)
		};

		let base_dts = Timestamp::from_scale(0, 48_000).unwrap();
		let fragment = muxer.fragment_at(0, base_dts, &[opus]).unwrap();
		assert_eq!(
			super::super::trun_durations(&fragment),
			vec![Some(960)],
			"20 ms at 48 kHz"
		);
	}

	// A per-frame caller owns the decode timeline, so a reordered (I, P, B) run keeps its
	// presentation order: tfdt follows the authored DTS while each cts carries the reorder.
	#[test]
	fn fragment_at_anchors_the_decode_timeline() {
		let muxer = video_muxer();
		let timescale = moq_net::Timescale::new(30_000).unwrap();
		let input = [tick_frame(0, true), tick_frame(3_000, false), tick_frame(1_000, false)];

		let mut base_dts = Timestamp::from_scale(0, 30_000).unwrap();
		for (sequence, frame) in input.iter().enumerate() {
			let fragment = muxer
				.fragment_at(sequence as u32, base_dts, std::slice::from_ref(frame))
				.unwrap();

			let (tfdt, _) = super::super::timeline(&fragment);
			assert_eq!(tfdt, base_dts.value(), "tfdt is the authored DTS, not the PTS");

			let decoded = super::super::decode(fragment, timescale).unwrap();
			assert_eq!(decoded.len(), 1);
			assert_eq!(decoded[0].timestamp, frame.timestamp, "pts survives the reorder");

			base_dts = base_dts.checked_add(frame.duration.unwrap()).unwrap();
		}

		// The authored timeline advanced one frame period per fragment, unlike the PTS.
		assert_eq!(base_dts, Timestamp::from_scale(3_000, 30_000).unwrap());
	}

	#[test]
	fn fragment_delegates_to_fragment_at() {
		let muxer = video_muxer();
		let frames = [tick_frame(0, true), tick_frame(1_000, false)];

		for count in 1..=frames.len() {
			let frames = &frames[..count];
			assert_eq!(
				muxer.fragment(7, frames).unwrap(),
				muxer.fragment_at(7, frames[0].timestamp, frames).unwrap()
			);
		}
	}

	#[test]
	fn fragment_with_no_frames_is_empty() {
		assert!(video_muxer().fragment(0, &[]).unwrap().is_empty());
	}

	// A downstream timeline fixed at 90 kHz overrides the framerate-derived default, and the
	// init and the fragments have to agree on it.
	#[test]
	fn with_timescale_overrides_the_catalog_derived_scale() {
		let timescale = moq_net::Timescale::new(90_000).unwrap();
		assert_eq!(video_muxer().timescale().as_u64(), 30_000, "framerate * 1000");

		let muxer = video_muxer().with_timescale(timescale).unwrap();
		assert_eq!(muxer.timescale(), timescale);

		let init = muxer.init().unwrap().expect("init buildable for an out-of-band codec");
		let trak = super::super::Wire::from_init(&init).unwrap();
		assert_eq!(trak.trak().mdia.mdhd.timescale, 90_000);

		// One 30 fps frame period is 3000 ticks at 90 kHz.
		let frame = Frame {
			timestamp: Timestamp::from_scale(3_000, 90_000).unwrap(),
			payload: Bytes::from_static(&[0xDE, 0xAD]),
			keyframe: true,
			duration: Some(Timestamp::from_scale(3_000, 90_000).unwrap()),
		};
		let decoded = super::super::decode(muxer.fragment(0, &[frame]).unwrap(), timescale).unwrap();
		assert_eq!(decoded[0].timestamp.as_micros(), 33_333);
	}

	// mdhd.timescale is 32 bits, but moq_net::Timescale spans the whole QUIC varint range. A
	// wider scale used to truncate into the init while the fragments kept the full value,
	// silently putting them on different timelines.
	#[test]
	fn with_timescale_rejects_a_scale_too_large_for_mdhd() {
		let too_large = moq_net::Timescale::new(u64::from(u32::MAX) + 1).unwrap();
		// Muxer isn't Debug, so match the Result rather than unwrap_err() it.
		assert!(matches!(
			video_muxer().with_timescale(too_large),
			Err(crate::Error::Cmaf(Error::TimescaleTooLarge(_)))
		));

		// The largest scale the field can hold is still accepted.
		let largest = moq_net::Timescale::new(u64::from(u32::MAX)).unwrap();
		let muxer = video_muxer().with_timescale(largest).unwrap();
		assert_eq!(muxer.timescale(), largest);
	}

	// The same truncation was reachable without with_timescale at all: the video timescale is
	// `framerate * 1000`, so an absurd catalog framerate overflows the field on its own.
	#[test]
	fn init_rejects_a_catalog_scale_too_large_for_mdhd() {
		let mut config = VideoConfig::new(VideoCodec::VP8);
		config.framerate = Some(5_000_000.0); // 5e9 ticks, past u32::MAX
		let err = Muxer::video(&config).unwrap().init().unwrap_err();
		assert!(
			matches!(err, crate::Error::Cmaf(Error::TimescaleTooLarge(_))),
			"got {err:?}"
		);
	}

	// A Cmaf rendition's init passes through from the catalog at its own scale, so an override
	// would leave the init and the fragments on different timelines.
	#[test]
	fn with_timescale_rejects_a_cmaf_rendition() {
		// Any valid single-track init will do; a synthesized one saves a fixture. Build it at
		// 48 kHz so the scale can only have come from the init: the framerate below would
		// otherwise derive 30_000, and the catalog carries no timescale of its own.
		let init = video_muxer()
			.with_timescale(moq_net::Timescale::new(48_000).unwrap())
			.unwrap()
			.init()
			.unwrap()
			.unwrap();
		let mut config = VideoConfig::new(VideoCodec::VP8);
		config.framerate = Some(30.0);
		config.container = CatalogContainer::Cmaf { init };

		let muxer = Muxer::video(&config).unwrap();
		assert_eq!(muxer.timescale().as_u64(), 48_000, "read from the init segment");
		assert!(muxer.with_timescale(moq_net::Timescale::new(90_000).unwrap()).is_err());
	}

	// The HLS origin accumulates every group of a (multi-group) audio segment into ONE fragment,
	// and for audio those groups are often one packet each -- so every sample sits at a group
	// boundary and none of them may borrow the next packet's timestamp (consecutive sequence
	// numbers don't rule out a publisher pausing across the boundary). Opus stating its own
	// duration is what keeps the whole run exact anyway, rather than dropping every packet onto
	// the ~21.3 ms 1024/sample_rate fallback.
	#[tokio::test]
	async fn audio_fragment_takes_durations_from_the_codec() {
		use hang::catalog::AudioCodec;

		let config = AudioConfig::new(AudioCodec::Opus, 48_000, 2);
		let muxer = Muxer::audio(&config).unwrap();

		// 20 ms of 48 kHz Opus: TOC config 15 (SILK wideband, 20 ms), one frame per packet.
		let packet = Bytes::from_static(&[0x78, 0x00, 0x00, 0x00]);
		let frames: Vec<Frame> = (0..4)
			.map(|i| Frame {
				payload: packet.clone(),
				..frame(i * 20_000, true)
			})
			.collect();
		let fragment = muxer.fragment(0, &frames).unwrap();

		let timescale = moq_net::Timescale::new(48_000).unwrap();
		let decoded = super::super::decode(fragment, timescale).unwrap();
		assert_eq!(decoded.len(), 4);
		for f in &decoded {
			assert_eq!(
				f.duration.unwrap().as_micros(),
				20_000,
				"TOC duration, not the fallback"
			);
		}
	}

	// A group boundary is never a duration, even when the groups arrived consecutively: the
	// publisher may have paused across it (moq-boy runs its PTS on a clock that keeps going
	// while the encoder is off), which is what produced a 2405 second sample in
	// moq-dev/moq.pro#814.
	#[tokio::test]
	async fn audio_fragment_does_not_absorb_a_pause() {
		use hang::catalog::AudioCodec;

		let config = AudioConfig::new(AudioCodec::Opus, 48_000, 2);
		let muxer = Muxer::audio(&config).unwrap();

		// Two one-packet groups either side of a 40 minute pause, fetched back to back.
		let packet = Bytes::from_static(&[0x78, 0x00, 0x00, 0x00]);
		let frames: Vec<Frame> = [63_244, 2_405_070_000]
			.into_iter()
			.map(|micros| Frame {
				payload: packet.clone(),
				..frame(micros, true)
			})
			.collect();
		let fragment = muxer.fragment(0, &frames).unwrap();

		let timescale = moq_net::Timescale::new(48_000).unwrap();
		let decoded = super::super::decode(fragment, timescale).unwrap();
		let first = decoded[0].duration.unwrap().as_micros();
		assert_eq!(first, 20_000, "the pause is a discontinuity, not a 2405 second sample");
	}
}
