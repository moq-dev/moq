//! One-shot CMAF muxing for individually fetched groups.

use std::time::Duration;

use bytes::Bytes;
use hang::catalog::{AudioConfig, Container as CatalogContainer, VideoConfig};
use mp4_atom::Encode;

use crate::catalog::hang::Container as HangContainer;
use crate::container::Frame;
use crate::container::source::{VideoTransform, build_video_transform};

use super::export::{catalog_timescale_audio, catalog_timescale_video, extract_init, infer_missing_durations};
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
	timescale: u64,
	/// Fallback duration for frames that carry none (Legacy / LOC sources), derived from the
	/// catalog framerate / sample rate.
	default_frame: Duration,
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
			timescale: catalog_timescale_video(config),
			default_frame: Duration::from_secs_f64(1.0 / framerate),
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
			timescale: catalog_timescale_audio(config),
			// Fallback for a duration-less trailing sample (~1024 samples per frame).
			default_frame: Duration::from_secs_f64(1024.0 / config.sample_rate.max(1) as f64),
			kind: Kind::Audio(config.clone()),
		})
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

		let container = match &self.kind {
			Kind::Video(config) => &config.container,
			Kind::Audio(config) => &config.container,
		};

		match container {
			CatalogContainer::Cmaf { init, .. } => {
				extract_init(init, TRACK_ID, &mut ftyp, &mut traks, &mut trexs)?;
			}
			CatalogContainer::Legacy | CatalogContainer::Loc => {
				let trak = match &self.kind {
					Kind::Video(config) => {
						synthesize_video_trak(TRACK_ID, self.timescale, config, self.description.as_deref())?
					}
					Kind::Audio(config) => synthesize_audio_trak(TRACK_ID, self.timescale, config)?,
				};
				trexs.push(mp4_atom::Trex {
					track_id: trak.tkhd.track_id,
					default_sample_description_index: 1,
					..Default::default()
				});
				traks.push(trak);
			}
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
	pub fn fragment(&self, sequence: u32, frames: &[Frame]) -> crate::Result<Bytes> {
		let frames = infer_missing_durations(frames.to_vec(), None, self.default_frame);
		let timescale = moq_net::Timescale::new(self.timescale).map_err(Error::from)?;
		Ok(super::encode_fragment(TRACK_ID, timescale, sequence, &frames)?)
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

		// VP8 needs no description, so the init builds without reading any media.
		let mut config = VideoConfig::new(VideoCodec::VP8);
		config.framerate = Some(30.0);
		let mut muxer = Muxer::video(&config).unwrap();

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

	// The HLS origin accumulates every group of a (multi-group) audio segment into ONE fragment,
	// so per-sample durations are inferred from each frame's real successor. Only the final sample
	// falls back to the codec frame duration. This is what keeps audio contiguous across the groups
	// an audio timeline skips.
	#[tokio::test]
	async fn audio_fragment_infers_durations_from_successors() {
		use hang::catalog::AudioCodec;

		// Opus at 48 kHz: default (fallback) frame duration is 1024/48000 = 21_333 us.
		let config = AudioConfig::new(AudioCodec::Opus, 48_000, 2);
		let muxer = Muxer::audio(&config).unwrap();

		// Frames 20 ms apart, none carrying a duration (the Legacy audio wire), as if fetched from
		// several one-packet groups and concatenated.
		let frames: Vec<Frame> = (0..4).map(|i| frame(i * 20_000, true)).collect();
		let fragment = muxer.fragment(0, &frames).unwrap();

		let timescale = moq_net::Timescale::new(48_000).unwrap();
		let decoded = super::super::decode(fragment, timescale).unwrap();
		assert_eq!(decoded.len(), 4);
		// Interior samples take the real 20 ms gap to their successor, not the codec fallback.
		for f in &decoded[..3] {
			assert_eq!(f.duration.unwrap().as_micros(), 20_000, "successor-derived duration");
		}
		// Only the last sample (no successor) falls back to the ~21.3 ms Opus frame duration.
		let last = decoded[3].duration.unwrap().as_micros();
		assert!(
			(21_000..21_400).contains(&last),
			"last sample uses the codec fallback, got {last}"
		);
	}
}
