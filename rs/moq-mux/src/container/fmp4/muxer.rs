//! One-shot CMAF muxing for application-selected media frames.

use std::time::Duration;

use bytes::Bytes;
use hang::catalog::{AudioConfig, Container as CatalogContainer, VideoConfig};

use crate::catalog::hang::Container as HangContainer;
use crate::container::Frame;
use crate::container::source::{VideoTransform, build_video_transform};

use super::export::{
	apply_codec_durations, catalog_timescale_audio, catalog_timescale_video, extract_init, infer_missing_durations,
};
use super::{Error, Fragmenter, fragment, synthesize_audio_trak, synthesize_video_trak};

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

/// CMAF data produced from one batch of encoded media frames.
#[derive(Clone, Debug)]
pub struct Output {
	/// The current initialization segment, once codec configuration is available.
	pub initialization: Option<Bytes>,
	/// The media fragment, or `None` when the batch produced no usable samples.
	pub fragment: Option<Bytes>,
}

/// Muxes one encoded rendition into standalone CMAF, without owning a subscription.
///
/// The pull-based [`Export`](super::Export) subscribes to a whole broadcast and interleaves
/// its tracks. `Muxer` instead packages frames selected independently by an application.
/// [`mux`](Self::mux) is the high-level transport-independent entry point: it normalizes a
/// frame batch, rebases its timestamps, and returns matching initialization and fragment bytes.
///
/// 1. [`read`](Self::read) decodes a fetched group into media [`Frame`]s, normalizing the
///    codec shape (Annex-B H.264/H.265 becomes length-prefixed, with the config record
///    synthesized from the in-band parameter sets).
/// 2. [`init`](Self::init) builds the rendition's init segment (ftyp+moov).
/// 3. [`fragment`](Self::fragment) encodes frames as one moof+mdat whose `tfdt` carries their
///    real presentation time, so a fragment built from a mid-stream group stands alone.
///    [`fragmenter`](Self::fragmenter) instead cuts a stream into one separately addressable
///    fragment per frame, for a consumer that stores media per encoded frame.
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
		let framerate = super::usable_video_framerate(config).unwrap_or(30.0);
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
	/// Derived from the catalog: a `Cmaf` rendition's own init scale, a cadence-compatible scale
	/// for video (falling back to 90 kHz) and the sample rate for audio, unless
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
			out.extend(self.normalize(frames)?);
		}
		if let Some(first) = out.first_mut() {
			first.keyframe = true;
		}
		Ok(out)
	}

	fn normalize(&mut self, frames: Vec<Frame>) -> crate::Result<Vec<Frame>> {
		let Some(mut transform) = self.transform.clone() else {
			return Ok(frames);
		};
		let mut description = self.description.clone();
		let mut out = Vec::with_capacity(frames.len());
		for (index, frame) in frames.into_iter().enumerate() {
			let Frame {
				timestamp,
				payload,
				keyframe,
				duration,
			} = frame;
			let payload = transform.transform(payload)?;
			let next_description = transform.codec_private().cloned();
			if description != next_description {
				if !out.is_empty() {
					return Err(Error::CodecConfigChanged { index }.into());
				}
				description = next_description;
			}
			// A length-prefixed sample without its config record is not independently usable.
			// Match the streaming exporter and discard pre-config slices.
			if let Some(payload) = payload
				&& description.is_some()
			{
				out.push(Frame {
					timestamp,
					payload,
					keyframe,
					duration,
				});
			}
		}
		self.transform = Some(transform);
		self.description = description;
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
					Kind::Video(config) => synthesize_video_trak(
						TRACK_ID,
						self.timescale.as_u64(),
						config,
						self.description.as_deref(),
						self.transform.is_some(),
					)?,
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
	/// [`fragmenter`](Self::fragmenter).
	pub fn fragment(&self, sequence: u32, frames: &[Frame]) -> crate::Result<Bytes> {
		self.fragment_owned(sequence, frames.to_vec())
	}

	/// A [`Fragmenter`] cutting this rendition into one fragment per frame.
	///
	/// It emits at the same timescale as [`init`](Self::init) and [`fragment`](Self::fragment),
	/// and owns its own decode timeline and sequence numbering, so feed it every frame of the
	/// stream in decode order. One fragmenter per continuous stream: a fetch-on-demand caller
	/// serving unrelated groups builds a fresh one per group and
	/// [`flush`](Fragmenter::flush)es it.
	pub fn fragmenter(&self, config: fragment::Config) -> Fragmenter {
		let is_video = matches!(self.kind, Kind::Video(_));
		Fragmenter {
			track_id: TRACK_ID,
			timescale: self.timescale,
			default_frame: self.default_frame,
			is_video,
			opus: self.opus,
			infer_missing: !is_video
				|| matches!(
					config.missing_duration,
					fragment::MissingDuration::InferFromPresentationTime
				),
			pending: None,
			dts: None,
			sequence: 0,
		}
	}

	/// Give every frame a duration: the one its codec states, else the gap to its successor in
	/// this slice, else the catalog frame rate / sample rate.
	fn resolve_durations(&self, frames: &[Frame]) -> crate::Result<Vec<Frame>> {
		let mut frames = frames.to_vec();
		apply_codec_durations(&mut frames, self.opus);
		infer_missing_durations(&mut frames, None, self.default_frame, self.timescale)?;
		Ok(frames)
	}

	fn fragment_owned(&self, sequence: u32, frames: Vec<Frame>) -> crate::Result<Bytes> {
		let frames = self.resolve_durations(&frames)?;
		Ok(super::encode_fragment(self.fragment_info(sequence), &frames)?)
	}

	/// Where a fragment sits: this muxer's single track, at its resolved timescale.
	fn fragment_info(&self, sequence: u32) -> super::FragmentInfo {
		super::FragmentInfo {
			track_id: TRACK_ID,
			timescale: self.timescale,
			sequence_number: sequence,
		}
	}

	/// Encode a fragment after subtracting an application-selected presentation origin.
	///
	/// This is useful whenever independently requested fragments must share a zero-based timeline.
	/// The origin must not be later than any supplied frame timestamp.
	pub fn fragment_rebased(&self, sequence: u32, origin: Duration, frames: &[Frame]) -> crate::Result<Bytes> {
		let mut rebased = Vec::with_capacity(frames.len());
		for frame in frames {
			let track_origin = moq_net::Timestamp::try_from(origin)?.convert(frame.timestamp.scale())?;
			let timestamp = frame.timestamp.checked_sub(track_origin)?;
			rebased.push(Frame {
				timestamp,
				..frame.clone()
			});
		}
		self.fragment_owned(sequence, rebased)
	}

	/// Normalize encoded frames and package them with the matching CMAF initialization.
	///
	/// Inline H.264/H.265 parameter sets are absorbed and used to build the init segment before
	/// the media fragment is encoded. Timestamps in the media fragment are relative to `origin`.
	/// Pre-config samples are discarded. A batch that crosses a codec configuration boundary is
	/// rejected so one init segment always describes every emitted sample. The origin must not be
	/// later than any emitted frame timestamp.
	pub fn mux(&mut self, sequence: u32, origin: Duration, frames: Vec<Frame>) -> crate::Result<Output> {
		let frames = self.normalize(frames)?;
		let initialization = self.init()?;
		let fragment = if frames.is_empty() {
			None
		} else {
			Some(self.fragment_rebased(sequence, origin, &frames)?)
		};
		Ok(Output {
			initialization,
			fragment,
		})
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

	#[test]
	fn muxer_rebases_fragment_to_explicit_origin() {
		let mut video = VideoConfig::new(VideoCodec::VP8);
		video.framerate = Some(30.0);
		let muxer = Muxer::video(&video).unwrap();
		let video_frames = vec![frame(10_000_000, true), frame(10_033_000, false)];
		let video_fragment = muxer
			.fragment_rebased(12, Duration::from_secs(10), &video_frames)
			.unwrap();
		let decoded_video = super::super::decode(video_fragment, moq_net::Timescale::new(30_000).unwrap()).unwrap();
		assert!(decoded_video[0].timestamp.is_zero());
		assert_eq!(decoded_video[1].timestamp.as_micros(), 33_000);
	}

	#[test]
	fn muxer_resolves_inline_h264_before_emitting_cmaf() {
		use hang::catalog::H264;

		let mut video = VideoConfig::new(H264 {
			profile: 0x42,
			constraints: 0xc0,
			level: 0x1f,
			inline: true,
		});
		video.coded_width = Some(320);
		video.coded_height = Some(240);
		video.framerate = Some(30.0);

		let mut muxer = Muxer::video(&video).unwrap();
		assert!(muxer.init().unwrap().is_none());

		let output = muxer
			.mux(
				7,
				Duration::from_secs(10),
				vec![crate::container::test_util::video_frame(10_000_000, true)],
			)
			.unwrap();
		assert_eq!(&output.initialization.unwrap()[4..8], b"ftyp");
		assert_eq!(&output.fragment.unwrap()[4..8], b"moof");
	}

	#[test]
	fn muxer_uses_cmaf_init_without_duplicate_codec_description() {
		use hang::catalog::H264;

		let mut video = VideoConfig::new(H264 {
			profile: 0x42,
			constraints: 0xc0,
			level: 0x1f,
			inline: true,
		});
		video.coded_width = Some(320);
		video.coded_height = Some(240);
		video.framerate = Some(30.0);

		let mut source = Muxer::video(&video).unwrap();
		let output = source
			.mux(
				0,
				Duration::ZERO,
				vec![crate::container::test_util::video_frame(0, true)],
			)
			.unwrap();
		video.container = CatalogContainer::Cmaf {
			init: output.initialization.unwrap(),
		};
		video.description = None;

		let remuxer = Muxer::video(&video).unwrap();
		assert!(remuxer.init().unwrap().is_some());
	}

	#[test]
	fn muxer_does_not_emit_inline_samples_before_configuration() {
		use hang::catalog::H264;

		let video = VideoConfig::new(H264 {
			profile: 0x42,
			constraints: 0xc0,
			level: 0x1f,
			inline: true,
		});
		let mut muxer = Muxer::video(&video).unwrap();
		let output = muxer
			.mux(
				0,
				Duration::ZERO,
				vec![crate::container::test_util::video_frame(0, false)],
			)
			.unwrap();
		assert!(output.initialization.is_none());
		assert!(output.fragment.is_none());
	}

	#[test]
	fn muxer_rejects_a_codec_change_after_emitting_a_sample() {
		use hang::catalog::H264;

		let video = VideoConfig::new(H264 {
			profile: 0x42,
			constraints: 0xc0,
			level: 0x1f,
			inline: true,
		});
		let first = crate::container::test_util::video_frame(0, true);
		let mut changed_sps = crate::container::test_util::SPS.to_vec();
		changed_sps[3] ^= 1;
		let mut changed_payload = Vec::new();
		for nal in [
			changed_sps.as_slice(),
			crate::container::test_util::PPS,
			crate::container::test_util::IDR,
		] {
			changed_payload.extend_from_slice(&[0, 0, 0, 1]);
			changed_payload.extend_from_slice(nal);
		}
		let changed = Frame {
			timestamp: Timestamp::from_micros(33_000).unwrap(),
			payload: Bytes::from(changed_payload),
			keyframe: true,
			duration: None,
		};

		let mut muxer = Muxer::video(&video).unwrap();
		let error = muxer.mux(0, Duration::ZERO, vec![first.clone(), changed]).unwrap_err();
		assert!(matches!(
			error,
			crate::Error::Cmaf(Error::CodecConfigChanged { index: 1 })
		));
		// Normalization is transactional, so the caller can split and retry the prefix.
		assert!(muxer.mux(0, Duration::ZERO, vec![first]).is_ok());
	}

	#[test]
	fn muxer_advertises_normalized_inline_h265_as_hvc1() {
		use hang::catalog::{H265, VideoCodec};
		use mp4_atom::DecodeMaybe;

		let mut video = crate::codec::h265::config_from_hvcc(&crate::codec::h265::fixtures::hvcc()).unwrap();
		let VideoCodec::H265(h265) = &video.codec else {
			panic!("fixture must describe H.265");
		};
		video.codec = VideoCodec::H265(H265 {
			in_band: true,
			..h265.clone()
		});
		video.description = None;

		let mut payload = Vec::new();
		for nal in [
			crate::codec::h265::fixtures::VPS,
			crate::codec::h265::fixtures::SPS,
			crate::codec::h265::fixtures::PPS,
			&[0x26, 0x01, 0x80, 0xaa],
		] {
			payload.extend_from_slice(&[0, 0, 0, 1]);
			payload.extend_from_slice(nal);
		}

		let mut muxer = Muxer::video(&video).unwrap();
		let output = muxer
			.mux(
				0,
				Duration::ZERO,
				vec![Frame {
					payload: Bytes::from(payload),
					..frame(0, true)
				}],
			)
			.unwrap();

		let mut cursor = std::io::Cursor::new(output.initialization.unwrap());
		while let Some(atom) = mp4_atom::Any::decode_maybe(&mut cursor).unwrap() {
			if let mp4_atom::Any::Moov(moov) = atom {
				assert!(matches!(
					moov.trak[0].mdia.minf.stbl.stsd.codecs[0],
					mp4_atom::Codec::Hvc1(_)
				));
				return;
			}
		}
		panic!("initialization missing moov");
	}

	#[test]
	fn muxer_preserves_explicit_sample_duration() {
		let mut video = VideoConfig::new(VideoCodec::VP8);
		video.framerate = Some(30.0);
		let mut muxer = Muxer::video(&video).unwrap();
		let exact = Timestamp::from_micros(17_000).unwrap();
		let output = muxer
			.mux(
				0,
				Duration::ZERO,
				vec![Frame {
					duration: Some(exact),
					..frame(0, true)
				}],
			)
			.unwrap();
		let decoded = super::super::decode(output.fragment.unwrap(), moq_net::Timescale::new(30_000).unwrap()).unwrap();
		assert_eq!(decoded[0].duration.unwrap().as_micros(), 17_000);
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

	#[test]
	fn fragment_with_no_frames_is_empty() {
		assert!(video_muxer().fragment(0, &[]).unwrap().is_empty());
	}

	#[test]
	fn ntsc_fallback_duration_uses_the_derived_timescale() {
		let mut config = VideoConfig::new(VideoCodec::VP8);
		config.framerate = Some(30_000.0 / 1001.0);
		let muxer = Muxer::video(&config).unwrap();
		assert_eq!(muxer.timescale().as_u64(), 30_000);

		let frame = Frame {
			timestamp: Timestamp::ZERO,
			payload: Bytes::from_static(&[0xDE, 0xAD]),
			keyframe: true,
			duration: None,
		};
		let fragment = muxer.fragment(0, std::slice::from_ref(&frame)).unwrap();
		assert_eq!(super::super::sample_durations(&fragment), vec![Some(1001)]);

		let mut fragmenter = muxer.fragmenter(fragment::Config {
			missing_duration: fragment::MissingDuration::InferFromPresentationTime,
		});
		assert!(fragmenter.push(frame).unwrap().is_empty());
		let fragment = fragmenter.flush().unwrap().unwrap();
		assert_eq!(super::super::sample_durations(&fragment.data), vec![Some(1001)]);
	}

	#[test]
	fn microsecond_pts_quantize_without_timeline_drift() {
		let muxer = video_muxer();
		let input = [0, 33_333, 66_667].map(|micros| Frame {
			timestamp: Timestamp::from_micros(micros).unwrap(),
			payload: Bytes::from_static(&[0xDE, 0xAD]),
			keyframe: micros == 0,
			duration: None,
		});

		let fragment = muxer.fragment(0, &input).unwrap();
		assert_eq!(super::super::sample_durations(&fragment), vec![Some(1_000); 3]);
		assert_eq!(super::super::timeline(&fragment), (0, vec![0; 3]));

		let mut fragmenter = muxer.fragmenter(fragment::Config {
			missing_duration: fragment::MissingDuration::InferFromPresentationTime,
		});
		let mut fragments = Vec::new();
		for frame in input {
			fragments.extend(fragmenter.push(frame).unwrap());
		}
		fragments.extend(fragmenter.flush().unwrap());
		let durations: Vec<_> = fragments
			.iter()
			.map(|fragment| super::super::sample_durations(&fragment.data)[0])
			.collect();
		assert_eq!(durations, vec![Some(1_000); 3]);
		let timelines: Vec<_> = fragments
			.iter()
			.map(|fragment| super::super::timeline(&fragment.data))
			.collect();
		assert_eq!(timelines, vec![(0, vec![0]), (1_000, vec![0]), (2_000, vec![0])]);
	}

	#[test]
	fn low_framerate_fallback_fits_mp4_timing_fields() {
		let mut config = VideoConfig::new(VideoCodec::VP8);
		config.framerate = Some(0.0011);
		let muxer = Muxer::video(&config).unwrap();
		assert_eq!(muxer.timescale().as_u64(), 11);
		assert!(muxer.init().unwrap().is_some());

		let frame = Frame {
			timestamp: Timestamp::ZERO,
			payload: Bytes::from_static(&[0xDE, 0xAD]),
			keyframe: true,
			duration: None,
		};
		let fragment = muxer.fragment(0, &[frame]).unwrap();
		assert_eq!(super::super::sample_durations(&fragment), vec![Some(10_000)]);
	}

	#[test]
	fn unusable_framerate_uses_the_standard_fallback_rate() {
		let mut config = VideoConfig::new(VideoCodec::VP8);
		config.framerate = Some(0.0005);
		let muxer = Muxer::video(&config).unwrap();
		let timescale = moq_net::Timescale::new(90_000).unwrap();
		assert_eq!(muxer.timescale(), timescale);

		let frame = Frame {
			timestamp: Timestamp::ZERO,
			payload: Bytes::from_static(&[0xDE, 0xAD]),
			keyframe: true,
			duration: None,
		};
		let decoded = super::super::decode(muxer.fragment(0, &[frame]).unwrap(), timescale).unwrap();
		assert_eq!(decoded[0].duration.unwrap().as_scale(timescale), 3_000);
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

	#[test]
	fn with_timescale_recomputes_the_fallback_frame_duration() {
		let timescale = moq_net::Timescale::new(90_000).unwrap();
		let muxer = video_muxer().with_timescale(timescale).unwrap();
		let frame = Frame {
			timestamp: Timestamp::ZERO,
			payload: Bytes::from_static(&[0xDE, 0xAD]),
			keyframe: true,
			duration: None,
		};

		let decoded = super::super::decode(muxer.fragment(0, &[frame]).unwrap(), timescale).unwrap();
		assert_eq!(decoded[0].duration.unwrap().as_scale(timescale), 3_000);
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

	// A consumer may accumulate every group of a multi-group audio interval into ONE fragment,
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
