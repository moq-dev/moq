//! One-shot CMAF muxing for individually fetched groups.

use std::time::Duration;

use bytes::Bytes;
use hang::catalog::{AudioConfig, Container as CatalogContainer, VideoConfig};
use mp4_atom::Encode;

use crate::catalog::hang::Container as HangContainer;
use crate::container::Frame;
use crate::container::source::{VideoTransform, build_video_transform};

use super::export::{
	apply_codec_durations, catalog_timescale_audio, catalog_timescale_video, extract_init, infer_missing_durations,
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
	/// own scale: overriding would leave the init and the fragments on different timelines.
	pub fn with_timescale(mut self, timescale: moq_net::Timescale) -> crate::Result<Self> {
		if matches!(self.catalog_container(), CatalogContainer::Cmaf { .. }) {
			return Err(Error::TimescaleOverride.into());
		}
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

		let ftyp = ftyp.unwrap_or(mp4_atom::Ftyp {
			major_brand: b"isom".into(),
			minor_version: 0x200,
			compatible_brands: vec![b"isom".into(), b"iso6".into(), b"mp41".into()],
		});
		let timescale = traks.first().map(|t| t.mdia.mdhd.timescale).unwrap_or(1000);

		let moov = mp4_atom::Moov {
			mvhd: mp4_atom::Mvhd {
				timescale,
				..Default::default()
			},
			trak: traks,
			mvex: if trexs.is_empty() {
				None
			} else {
				Some(mp4_atom::Mvex {
					trex: trexs,
					..Default::default()
				})
			},
			..Default::default()
		};

		let mut buf = Vec::new();
		ftyp.encode(&mut buf).map_err(Error::from)?;
		moov.encode(&mut buf).map_err(Error::from)?;
		Ok(Some(Bytes::from(buf)))
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
	/// A caller that owns a continuous decode timeline across calls, typically one emitting a
	/// fragment per frame, wants [`fragment_at`](Self::fragment_at) instead.
	pub fn fragment(&self, sequence: u32, frames: &[Frame]) -> crate::Result<Bytes> {
		let Some(base_dts) = frames.first().map(|f| f.timestamp) else {
			return Ok(Bytes::new());
		};
		self.fragment_at(sequence, base_dts, frames)
	}

	/// Encode frames as one moof+mdat fragment whose decode timeline starts at `base_dts`.
	///
	/// [`fragment`](Self::fragment) anchors each fragment at `frames[0].timestamp`, which keeps
	/// it self-contained but re-anchors on every call. A caller emitting one fragment per frame
	/// owns the decode timeline itself: pass the DTS it authored and the composition offsets
	/// (`PTS - DTS`) come out correct, so reordered frames keep their presentation order and
	/// `tfdt` stays monotonic across fragments.
	///
	/// `base_dts` must be monotonically non-decreasing across a track's fragments. Returns an
	/// empty `Bytes` when `frames` is empty.
	pub fn fragment_at(&self, sequence: u32, base_dts: moq_net::Timestamp, frames: &[Frame]) -> crate::Result<Bytes> {
		let mut frames = frames.to_vec();
		apply_codec_durations(&mut frames, self.opus);
		let frames = infer_missing_durations(frames, None, self.default_frame);
		Ok(super::encode_fragment_at(
			TRACK_ID,
			self.timescale,
			sequence,
			base_dts,
			&frames,
		)?)
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

	// A Cmaf rendition's init passes through from the catalog at its own scale, so an override
	// would leave the init and the fragments on different timelines.
	#[test]
	fn with_timescale_rejects_a_cmaf_rendition() {
		// Any valid single-track init will do; a synthesized one saves a fixture.
		let init = video_muxer().init().unwrap().unwrap();
		let mut config = VideoConfig::new(VideoCodec::VP8);
		config.container = CatalogContainer::Cmaf { init };

		let muxer = Muxer::video(&config).unwrap();
		assert_eq!(muxer.timescale().as_u64(), 30_000, "read from the init segment");
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
