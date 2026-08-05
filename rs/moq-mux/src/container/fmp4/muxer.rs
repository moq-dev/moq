//! One-shot CMAF muxing for individually fetched groups.

use std::time::Duration;

use bytes::Bytes;
use hang::catalog::{AudioConfig, Container as CatalogContainer, VideoConfig};

use crate::catalog::hang::Container as HangContainer;
use crate::container::Frame;
use crate::container::source::{VideoTransform, build_video_transform};

use super::export::{
	apply_codec_durations, catalog_timescale_audio, catalog_timescale_video, extract_init, fragment_seconds,
	infer_missing_durations,
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
///    [`fragments`](Self::fragments) cuts the same frames into one separately addressable
///    fragment each, for a consumer that stores media per encoded frame.
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
	/// catalog framerate / sample rate.
	default_frame: Duration,
	/// True for Opus audio, whose packets state their own duration in the TOC byte.
	opus: bool,
}

impl Muxer {
	/// A muxer for a video rendition described by `config`.
	pub fn video(config: &VideoConfig) -> crate::Result<Self> {
		let container = (&config.container).try_into()?;
		let framerate = config
			.framerate
			.filter(|fps| fps.is_finite() && *fps > 0.0)
			.unwrap_or(30.0);
		Ok(Self {
			container,
			transform: build_video_transform(config),
			description: config.description.as_ref().filter(|b| !b.is_empty()).cloned(),
			timescale: moq_net::Timescale::new(catalog_timescale_video(config)?).map_err(Error::from)?,
			default_frame: Duration::from_secs_f64(1.0 / framerate),
			opus: false,
			kind: Kind::Video(config.clone()),
		})
	}

	/// A muxer for an audio rendition described by `config`.
	pub fn audio(config: &AudioConfig) -> crate::Result<Self> {
		let container = (&config.container).try_into()?;
		Ok(Self {
			container,
			transform: None,
			description: config.description.as_ref().filter(|b| !b.is_empty()).cloned(),
			timescale: moq_net::Timescale::new(catalog_timescale_audio(config)?).map_err(Error::from)?,
			// Fallback for a duration-less trailing sample (~1024 samples per frame).
			default_frame: Duration::from_secs_f64(1024.0 / config.sample_rate.max(1) as f64),
			opus: matches!(config.codec, hang::catalog::AudioCodec::Opus),
			kind: Kind::Audio(config.clone()),
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
	/// To make each frame separately addressable instead, use
	/// [`fragments`](Self::fragments).
	pub fn fragment(&self, sequence: u32, frames: &[Frame]) -> crate::Result<Bytes> {
		let frames = self.resolve_durations(frames);
		Ok(super::encode_fragment(self.fragment_info(sequence), &frames)?)
	}

	/// Encode the same frames as one moof+mdat fragment each, on one continuous decode timeline.
	///
	/// For a consumer that stores media per encoded frame: LL-HLS Partial Segments are cut at
	/// stored-object boundaries, so one fragment per group would make a whole GOP (~1s) the
	/// smallest addressable unit, coarser than a typical `PART-TARGET` and too coarse for
	/// low-latency HLS.
	///
	/// Prefer this over calling [`fragment`](Self::fragment) once per frame. Durations are
	/// resolved across the whole slice first, so each frame is timed by its real successor, and
	/// `tfdt` then advances by exactly the durations written into the preceding `trun`s. Cutting
	/// the frames up first loses both: a one-frame call has no successor to infer from and falls
	/// back to the catalog frame rate, and every fragment re-anchors at its own frame, which
	/// collapses the composition offsets to zero and lets `tfdt` follow the reordered PTS.
	///
	/// Each [`Fragment`](super::Fragment) carries the timing written into it, which is what an
	/// LL-HLS `EXT-X-PART` needs: `duration` is the resolved sample duration rather than the raw
	/// gap a caller could compute, and `independent` says whether the part can start a segment.
	///
	/// Fragment `i` is numbered `sequence + i`, so pass a base that doesn't collide with the
	/// track's other fragments. Returns an empty `Vec` when `frames` is empty.
	pub fn fragments(&self, sequence: u32, frames: &[Frame]) -> crate::Result<Vec<super::Fragment>> {
		let frames = self.resolve_durations(frames);
		let encoded = super::encode_fragments(self.fragment_info(sequence), &frames)?;

		// Audio has no keyframes, so every audio fragment is independent; video is independent
		// only at a GOP boundary. Matches what the exporter advertises.
		let is_video = matches!(self.kind, Kind::Video(_));

		Ok(encoded
			.into_iter()
			.zip(&frames)
			.map(|(data, frame)| super::Fragment {
				data,
				init: false,
				independent: !is_video || frame.keyframe,
				duration: fragment_seconds(std::slice::from_ref(frame), self.default_frame),
			})
			.collect())
	}

	/// Give every frame a duration: the one its codec states, else the gap to its successor in
	/// this slice, else the catalog frame rate / sample rate.
	fn resolve_durations(&self, frames: &[Frame]) -> Vec<Frame> {
		let mut frames = frames.to_vec();
		apply_codec_durations(&mut frames, self.opus);
		infer_missing_durations(frames, None, self.default_frame)
	}

	/// Where a fragment sits: this muxer's single track, at its resolved timescale.
	fn fragment_info(&self, sequence: u32) -> super::FragmentInfo {
		super::FragmentInfo {
			track_id: TRACK_ID,
			timescale: self.timescale,
			sequence_number: sequence,
		}
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

	// One fragment per frame over a reordered (I, P, B) run: tfdt walks the decode timeline
	// while each cts carries the reorder, so presentation order survives being cut up.
	#[test]
	fn fragments_anchor_one_decode_timeline() {
		let muxer = video_muxer();
		let timescale = moq_net::Timescale::new(30_000).unwrap();
		let input = [tick_frame(0, true), tick_frame(3_000, false), tick_frame(1_000, false)];

		let fragments = muxer.fragments(0, &input).unwrap();
		assert_eq!(fragments.len(), input.len(), "one fragment per frame");

		let timelines: Vec<_> = fragments.iter().map(|f| super::super::timeline(&f.data)).collect();
		assert_eq!(
			timelines,
			vec![(0, vec![0]), (1_000, vec![2_000]), (2_000, vec![-1_000])],
			"tfdt advances one frame period while cts carries the reorder"
		);

		for (fragment, expected) in fragments.into_iter().zip(&input) {
			let decoded = super::super::decode(fragment.data, timescale).unwrap();
			assert_eq!(decoded.len(), 1);
			assert_eq!(decoded[0].timestamp, expected.timestamp, "pts survives the reorder");
		}
	}

	// Cutting the frames up first is what a caller would otherwise write by hand, and it loses
	// the successor every duration is inferred from. Resolving across the whole slice first is
	// the entire reason `fragments` exists rather than a per-frame `fragment` loop.
	#[test]
	fn fragments_time_each_frame_by_its_real_successor() {
		let muxer = video_muxer();
		// A 20 fps stretch of a 30 fps rendition: real gaps of 1500, no stated durations.
		let input: Vec<Frame> = [0u64, 1_500, 3_000]
			.iter()
			.map(|&pts| Frame {
				timestamp: Timestamp::from_scale(pts, 30_000).unwrap(),
				payload: Bytes::from_static(&[0xDE, 0xAD]),
				keyframe: pts == 0,
				duration: None,
			})
			.collect();

		let fragments = muxer.fragments(0, &input).unwrap();

		// Each sample lasts the real gap to its successor. Only the last has none to read, so it
		// alone falls back to the catalog frame period.
		let durations: Vec<_> = fragments
			.iter()
			.map(|f| super::super::sample_durations(&f.data))
			.collect();
		assert_eq!(
			durations,
			vec![vec![Some(1_500)], vec![Some(1_500)], vec![Some(999)]],
			"timed by the successor in the slice, not the catalog default"
		);

		// tfdt advances by exactly what the preceding trun claimed, so the fragments tile.
		let tfdts: Vec<u64> = fragments.iter().map(|f| super::super::timeline(&f.data).0).collect();
		assert_eq!(tfdts, vec![0, 1_500, 3_000]);

		// Cutting first is what a hand-written per-frame loop does: every call sees a one-frame
		// slice with no successor, so all of them fall back to 999 and drift ~33% short.
		let cut_first: Vec<_> = input
			.iter()
			.map(|frame| super::super::sample_durations(&muxer.fragment(0, std::slice::from_ref(frame)).unwrap()))
			.collect();
		assert_eq!(
			cut_first,
			vec![vec![Some(999)]; 3],
			"the drift this method exists to avoid"
		);
	}

	// An EXT-X-PART needs a DURATION and an INDEPENDENT flag. The duration has to be the resolved
	// one written into the trun, not the raw PTS gap: a caller computing the latter has nothing to
	// read for the slice's last frame and would disagree with the media it is describing.
	#[test]
	fn fragments_carry_the_part_metadata() {
		let muxer = video_muxer();
		// Real gaps of 1500 ticks at 30 kHz (0.05s), no stated durations.
		let input: Vec<Frame> = [0u64, 1_500, 3_000]
			.iter()
			.map(|&pts| Frame {
				timestamp: Timestamp::from_scale(pts, 30_000).unwrap(),
				payload: Bytes::from_static(&[0xDE, 0xAD]),
				keyframe: pts == 0,
				duration: None,
			})
			.collect();

		let fragments = muxer.fragments(0, &input).unwrap();

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

	// Audio has no keyframes, so every audio part can start a segment.
	#[test]
	fn audio_fragments_are_always_independent() {
		let config = AudioConfig::new(hang::catalog::AudioCodec::Opus, 48_000, 2);
		let muxer = Muxer::audio(&config).unwrap();

		// Two 20 ms Opus packets; the TOC byte states their duration.
		let packet = Bytes::from_static(&[0x78, 0x00, 0x00, 0x00]);
		let frames: Vec<Frame> = [0u64, 20_000]
			.into_iter()
			.map(|micros| Frame {
				payload: packet.clone(),
				..frame(micros, false)
			})
			.collect();

		let fragments = muxer.fragments(0, &frames).unwrap();
		assert!(
			fragments.iter().all(|f| f.independent),
			"audio fragments are always independent"
		);
	}

	#[test]
	fn fragment_with_no_frames_is_empty() {
		assert!(video_muxer().fragment(0, &[]).unwrap().is_empty());
		assert!(video_muxer().fragments(0, &[]).unwrap().is_empty());
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
