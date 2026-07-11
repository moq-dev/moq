//! Per-codec bridges between moq-mux and str0m.
//!
//! Two directions:
//! - **Ingest** ([`Bridge`]): str0m hands a decoded codec frame via
//!   `Event::MediaData`; the bridge converts it into the shape the
//!   moq-mux importer expects and publishes it.
//! - **Egress** ([`Track`]): the egress source subscribes to a moq-mux
//!   broadcast and the track yields RTP-ready codec frames that the
//!   session loop hands to [`str0m::media::Writer::write`].

pub mod av1;
pub mod h264;
pub mod h265;
pub mod opus;
pub mod vp8;
pub mod vp9;

#[cfg(test)]
mod bitstream_test;

use bytes::Bytes;
use hang::catalog::VideoConfig;

use crate::Result;

/// One codec frame received from str0m, paired with a microsecond timestamp.
///
/// Used by the ingest path. The session loop converts str0m's
/// [`MediaTime`](str0m::media::MediaTime) to microseconds so individual
/// bridges don't need to repeat the math.
#[derive(Clone, Debug)]
pub struct Frame {
	pub timestamp_us: u64,
	pub payload: Bytes,
}

/// Bridges depacketized media frames from str0m to a hang broadcast track.
///
/// One bridge per `m=` line on the ingest side. The session loop calls
/// [`Bridge::push`] once per [`MediaData`](str0m::media::MediaData) event
/// with the codec frame; the bridge handles any codec-specific transformations
/// (e.g. Annex-B to AVCC for H.264) and forwards the frame into the matching
/// moq-mux importer.
pub trait Bridge: Send {
	fn push(&mut self, frame: Frame) -> Result<()>;

	/// Abort the published track with `err` so subscribers see the real cause
	/// (the peer disconnected, an ICE failure) rather than a bare `Error::Dropped`.
	fn abort(&mut self, err: moq_net::Error);
}

/// One RTP-ready codec frame produced by an egress [`Track`].
///
/// `timestamp_us` stays in microseconds; the session loop converts it to
/// the negotiated codec's clock domain when calling
/// [`Writer::write`](str0m::media::Writer::write).
#[derive(Clone, Debug)]
pub struct PacketizedFrame {
	pub timestamp_us: u64,
	pub payload: Bytes,
}

/// A subscribed moq-mux track, normalized to the bitstream shape str0m's
/// Frame API expects.
///
/// One [`Track`] per `m=` line on the egress side. The egress source spawns
/// a pump task per track that polls [`Track::next`] and forwards frames to
/// the session loop.
pub struct Track {
	consumer: moq_mux::container::Consumer<moq_mux::catalog::hang::Container>,
	convert: TrackConvert,
}

/// Codec-specific per-frame transform.
enum TrackConvert {
	/// Opus / VP8 / VP9 / AV1, plus inline-parameter H.264 (avc3) and H.265
	/// (hev1): the stored bitstream is already in the shape str0m's
	/// packetizer wants, so it passes through untouched.
	Passthrough,
	/// Out-of-band-parameter H.264 (avc1) and H.265 (hvc1): length-prefixed
	/// NALU rewritten to Annex-B, with the cached parameter sets (SPS+PPS,
	/// plus VPS for H.265) prepended to every keyframe. Both codecs share this
	/// path; only the config record parsed to build it differs (avcC vs hvcC).
	/// Mirrors moq-mux's `h264::Export` / `h265::Export`.
	LengthPrefixed { length_size: usize, keyframe_prefix: Bytes },
}

impl Track {
	/// Audio track for an Opus rendition, from a subscribed `track`.
	pub fn opus(track: moq_net::track::Subscriber) -> Self {
		let container = moq_mux::catalog::hang::Container::Legacy;
		let consumer = moq_mux::container::Consumer::new(track, container);
		Self {
			consumer,
			convert: TrackConvert::Passthrough,
		}
	}

	/// Video track from a subscribed `track` consumer. Codec inferred from
	/// `config.codec`; for H.264 / H.265 the bitstream shape (inline vs out-of-band
	/// parameter sets) is inferred from `config.description` (avc1/hvc1 vs avc3/hev1).
	pub fn video(track: moq_net::track::Subscriber, config: &VideoConfig) -> Result<Self> {
		let container: moq_mux::catalog::hang::Container = (&config.container).try_into()?;
		let consumer = moq_mux::container::Consumer::new(track, container);

		let convert = match &config.codec {
			hang::catalog::VideoCodec::VP8 => TrackConvert::Passthrough,
			hang::catalog::VideoCodec::VP9(_) => TrackConvert::Passthrough,
			hang::catalog::VideoCodec::AV1(_) => TrackConvert::Passthrough,
			hang::catalog::VideoCodec::H264(_) => h264_convert(config)?,
			hang::catalog::VideoCodec::H265(_) => h265_convert(config)?,
			other => return Err(crate::Error::UnsupportedCodec(format!("{other:?}"))),
		};

		Ok(Self { consumer, convert })
	}

	/// Pull the next RTP-ready frame. Returns `None` when the track ends.
	pub async fn next(&mut self) -> Result<Option<PacketizedFrame>> {
		loop {
			let Some(frame) = self.consumer.read().await? else {
				return Ok(None);
			};
			let payload = match &self.convert {
				TrackConvert::Passthrough => frame.payload,
				TrackConvert::LengthPrefixed {
					length_size,
					keyframe_prefix,
				} => {
					let prefix = frame.keyframe.then(|| keyframe_prefix.as_ref());
					moq_mux::codec::annexb::from_length_prefixed(&frame.payload, *length_size, prefix)
						.map_err(|err| crate::Error::Other(anyhow::anyhow!("annexb: {err}")))?
				}
			};
			if payload.is_empty() {
				continue;
			}
			return Ok(Some(PacketizedFrame {
				timestamp_us: frame.timestamp.as_micros() as u64,
				payload,
			}));
		}
	}
}

/// Build the per-frame transform for an H.264 rendition.
///
/// avc3 (inline SPS/PPS, empty `description`) passes through. avc1 (out-of-band
/// avcC in `description`) parses the avcC and prebuilds the Annex-B SPS+PPS
/// prefix to prepend ahead of every keyframe.
fn h264_convert(config: &VideoConfig) -> Result<TrackConvert> {
	let Some(avcc) = config.description.as_ref().filter(|d| !d.is_empty()) else {
		return Ok(TrackConvert::Passthrough);
	};
	let params = moq_mux::codec::h264::Avcc::parse(avcc)
		.map_err(|err| crate::Error::Other(anyhow::anyhow!("avcc parse: {err}")))?;
	// Without SPS+PPS the keyframe prefix would be empty and every keyframe
	// would reach the peer without inline parameter sets, i.e. undecodable.
	// Fail loudly instead, matching moq-mux's `h264::Export`.
	if params.sps.is_empty() || params.pps.is_empty() {
		return Err(crate::Error::Other(anyhow::anyhow!(
			"avc1 avcC is missing parameter sets (sps={}, pps={})",
			params.sps.len(),
			params.pps.len()
		)));
	}
	let keyframe_prefix = moq_mux::codec::annexb::build_prefix(params.sps.iter().chain(params.pps.iter()));
	Ok(TrackConvert::LengthPrefixed {
		length_size: params.length_size,
		keyframe_prefix,
	})
}

/// Build the per-frame transform for an H.265 rendition.
///
/// The H.265 analogue of [`h264_convert`]: hev1 (inline VPS/SPS/PPS) passes
/// through; hvc1 (out-of-band hvcC) parses the hvcC and prebuilds the Annex-B
/// VPS+SPS+PPS prefix to prepend ahead of every keyframe.
fn h265_convert(config: &VideoConfig) -> Result<TrackConvert> {
	let Some(hvcc) = config.description.as_ref().filter(|d| !d.is_empty()) else {
		return Ok(TrackConvert::Passthrough);
	};
	let params = moq_mux::codec::h265::Hvcc::parse(hvcc)
		.map_err(|err| crate::Error::Other(anyhow::anyhow!("hvcc parse: {err}")))?;
	// Same reasoning as `h264_convert`: a keyframe with no inline VPS/SPS/PPS
	// is undecodable, so reject an hvcC that omits any of them.
	if params.vps.is_empty() || params.sps.is_empty() || params.pps.is_empty() {
		return Err(crate::Error::Other(anyhow::anyhow!(
			"hvc1 hvcC is missing parameter sets (vps={}, sps={}, pps={})",
			params.vps.len(),
			params.sps.len(),
			params.pps.len()
		)));
	}
	let keyframe_prefix =
		moq_mux::codec::annexb::build_prefix(params.vps.iter().chain(params.sps.iter()).chain(params.pps.iter()));
	Ok(TrackConvert::LengthPrefixed {
		length_size: params.length_size,
		keyframe_prefix,
	})
}

#[cfg(test)]
mod tests {
	use hang::catalog::{H264, H265, VideoConfig};

	use super::*;

	fn config(codec: impl Into<hang::catalog::VideoCodec>, description: Option<Bytes>) -> VideoConfig {
		let mut config = VideoConfig::new(codec);
		config.description = description;
		config
	}

	fn h264(inline: bool) -> H264 {
		H264 {
			inline,
			profile: 0x42,
			constraints: 0,
			level: 0x1f,
		}
	}

	fn h265(in_band: bool) -> H265 {
		H265 {
			in_band,
			profile_space: 0,
			profile_idc: 1,
			profile_compatibility_flags: [0; 4],
			tier_flag: false,
			level_idc: 0x5d,
			constraint_flags: [0; 6],
		}
	}

	/// Minimal avcC carrying one SPS + one PPS (lengthSizeMinusOne = 3).
	fn build_avcc(sps: &[u8], pps: &[u8]) -> Bytes {
		let mut v = vec![1, sps[1], sps[2], sps[3], 0xff, 0xe1];
		v.extend_from_slice(&(sps.len() as u16).to_be_bytes());
		v.extend_from_slice(sps);
		v.push(1);
		v.extend_from_slice(&(pps.len() as u16).to_be_bytes());
		v.extend_from_slice(pps);
		Bytes::from(v)
	}

	/// Minimal hvcC carrying one VPS + SPS + PPS array (lengthSizeMinusOne = 3).
	/// VPS/SPS/PPS NAL unit types are 32/33/34.
	fn build_hvcc(vps: &[u8], sps: &[u8], pps: &[u8]) -> Bytes {
		let mut v = vec![0u8; 21];
		v.push(0xff); // [21] lengthSizeMinusOne = 3 in the low 2 bits
		v.push(3); // [22] numOfArrays
		for (nal_type, nal) in [(32u8, vps), (33, sps), (34, pps)] {
			v.push(nal_type); // array header: low 6 bits = NAL unit type
			v.extend_from_slice(&1u16.to_be_bytes()); // numNalus
			v.extend_from_slice(&(nal.len() as u16).to_be_bytes());
			v.extend_from_slice(nal);
		}
		Bytes::from(v)
	}

	#[test]
	fn h264_avc3_passthrough() {
		let cfg = config(h264(true), None);
		assert!(matches!(h264_convert(&cfg).unwrap(), TrackConvert::Passthrough));
	}

	#[test]
	fn h264_avc1_length_prefixed() {
		let sps: &[u8] = &[0x67, 0x42, 0xc0, 0x1f, 0xde];
		let pps: &[u8] = &[0x68, 0xce, 0x3c, 0x80];
		let cfg = config(h264(false), Some(build_avcc(sps, pps)));

		let TrackConvert::LengthPrefixed {
			length_size,
			keyframe_prefix,
		} = h264_convert(&cfg).unwrap()
		else {
			panic!("expected LengthPrefixed");
		};
		assert_eq!(length_size, 4);
		assert!(keyframe_prefix.starts_with(&[0, 0, 0, 1]), "Annex-B start code");
		assert!(keyframe_prefix.windows(sps.len()).any(|w| w == sps), "SPS in prefix");
		assert!(keyframe_prefix.windows(pps.len()).any(|w| w == pps), "PPS in prefix");
	}

	#[test]
	fn h265_hev1_passthrough() {
		let cfg = config(h265(true), None);
		assert!(matches!(h265_convert(&cfg).unwrap(), TrackConvert::Passthrough));
	}

	#[test]
	fn h265_hvc1_length_prefixed() {
		let vps: &[u8] = &[0x40, 0x01, 0x0c, 0x01];
		let sps: &[u8] = &[0x42, 0x01, 0x01, 0x01];
		let pps: &[u8] = &[0x44, 0x01, 0xc0, 0xf7];
		let cfg = config(h265(false), Some(build_hvcc(vps, sps, pps)));

		let TrackConvert::LengthPrefixed {
			length_size,
			keyframe_prefix,
		} = h265_convert(&cfg).unwrap()
		else {
			panic!("expected LengthPrefixed");
		};
		assert_eq!(length_size, 4);
		// Parameter sets are prefixed in VPS, SPS, PPS order.
		let v = keyframe_prefix.windows(vps.len()).position(|w| w == vps).expect("VPS");
		let s = keyframe_prefix.windows(sps.len()).position(|w| w == sps).expect("SPS");
		let p = keyframe_prefix.windows(pps.len()).position(|w| w == pps).expect("PPS");
		assert!(v < s && s < p, "VPS < SPS < PPS order in prefix");
	}

	/// An avc1 avcC that parses but carries no SPS/PPS must be rejected rather
	/// than silently producing keyframes without inline parameter sets.
	#[test]
	fn h264_avc1_missing_param_sets_errors() {
		// 6-byte header (numSPS = 0 in the low 5 bits of byte 5) + a zero PPS count.
		let avcc = Bytes::from(vec![1, 0x42, 0, 0x1f, 0xff, 0xe0, 0x00]);
		let cfg = config(h264(false), Some(avcc));
		assert!(h264_convert(&cfg).is_err(), "missing SPS/PPS must error");
	}
}
