//! Per-codec bridges between moq-mux and str0m.
//!
//! Two directions:
//! - **Ingest** ([`Bridge`]): str0m hands a decoded codec frame via
//!   `Event::MediaData`; the bridge converts it into the shape the
//!   moq-mux importer expects and publishes it.
//! - **Egress** ([`Track`]): the egress source subscribes to a moq-mux
//!   broadcast and the track yields RTP-ready codec frames that the
//!   session loop hands to [`str0m::media::Writer::write`].

pub mod h264;
pub mod opus;
pub mod vp8;
pub mod vp9;

use bytes::Bytes;
use hang::catalog::VideoConfig;
use str0m::format::Codec;

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
	codec: Codec,
	convert: TrackConvert,
}

/// Codec-specific per-frame transform.
enum TrackConvert {
	/// Opus / VP8 / VP9 / avc3-stored H.264: bitstream passes through.
	Passthrough,
	/// avc1-stored H.264: length-prefixed NALU -> Annex-B, with cached
	/// SPS+PPS prefixed onto every keyframe (see
	/// [`moq_mux::codec::h264::Export`] for the equivalent stand-alone
	/// exporter — same logic).
	H264Avc1 { length_size: usize, keyframe_prefix: Bytes },
}

impl Track {
	/// Audio track for an Opus rendition.
	pub async fn opus(broadcast: &moq_net::BroadcastConsumer, name: &str) -> Result<Self> {
		let container = moq_mux::catalog::hang::Container::Legacy;
		let track = broadcast
			.consume_track(name)
			.subscribe(moq_net::Subscription::default())
			.await?;
		let consumer = moq_mux::container::Consumer::new(track, container);
		Ok(Self {
			consumer,
			codec: Codec::Opus,
			convert: TrackConvert::Passthrough,
		})
	}

	/// Video track. Codec inferred from `config.codec`; bitstream shape
	/// inferred from `config.description` (avc1 vs avc3).
	pub async fn video(broadcast: &moq_net::BroadcastConsumer, name: &str, config: &VideoConfig) -> Result<Self> {
		let container: moq_mux::catalog::hang::Container = (&config.container).try_into()?;
		let track = broadcast
			.consume_track(name)
			.subscribe(moq_net::Subscription::default())
			.await?;
		let consumer = moq_mux::container::Consumer::new(track, container);

		let (codec, convert) = match &config.codec {
			hang::catalog::VideoCodec::VP8 => (Codec::Vp8, TrackConvert::Passthrough),
			hang::catalog::VideoCodec::VP9(_) => (Codec::Vp9, TrackConvert::Passthrough),
			hang::catalog::VideoCodec::H264(_) => match config.description.as_ref().filter(|d| !d.is_empty()) {
				None => (Codec::H264, TrackConvert::Passthrough),
				Some(avcc) => {
					let parsed = moq_mux::codec::h264::Avcc::parse(avcc)
						.map_err(|err| crate::Error::Other(anyhow::anyhow!("avcc parse: {err}")))?;
					// Pull SPS+PPS from the avcC and prebuild the Annex-B prefix
					// to prepend ahead of every keyframe.
					let (sps, pps) = avcc_param_sets(avcc)?;
					let prefix = moq_mux::codec::annexb::build_prefix(sps.iter().chain(pps.iter()));
					(
						Codec::H264,
						TrackConvert::H264Avc1 {
							length_size: parsed.length_size,
							keyframe_prefix: prefix,
						},
					)
				}
			},
			other => return Err(crate::Error::UnsupportedCodec(format!("{other:?}"))),
		};

		Ok(Self {
			consumer,
			codec,
			convert,
		})
	}

	pub fn codec(&self) -> Codec {
		self.codec
	}

	/// Pull the next RTP-ready frame. Returns `None` when the track ends.
	pub async fn next(&mut self) -> Result<Option<PacketizedFrame>> {
		loop {
			let Some(frame) = self.consumer.read().await? else {
				return Ok(None);
			};
			let payload = match &self.convert {
				TrackConvert::Passthrough => frame.payload,
				TrackConvert::H264Avc1 {
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

/// Parse SPS+PPS NAL units out of an `AVCDecoderConfigurationRecord`.
///
/// Mirrors `moq_mux::codec::h264::parse_avcc_param_sets` which is `pub(crate)`
/// over there. Kept tiny on purpose; the catalog avcC is always small.
fn avcc_param_sets(avcc: &[u8]) -> Result<(Vec<Bytes>, Vec<Bytes>)> {
	if avcc.len() < 6 {
		return Err(crate::Error::Other(anyhow::anyhow!("avcC too short")));
	}
	let length_size_minus_one = avcc[4] & 0x03;
	if length_size_minus_one != 3 {
		// Other length sizes are technically legal, but real-world catalogs use 4.
		return Err(crate::Error::Other(anyhow::anyhow!(
			"unsupported avcC lengthSizeMinusOne = {length_size_minus_one}"
		)));
	}
	let mut sps = Vec::new();
	let mut pps = Vec::new();

	let mut pos = 5;
	let num_sps = (avcc[pos] & 0x1f) as usize;
	pos += 1;
	for _ in 0..num_sps {
		if pos + 2 > avcc.len() {
			return Err(crate::Error::Other(anyhow::anyhow!("avcC truncated in SPS table")));
		}
		let len = u16::from_be_bytes([avcc[pos], avcc[pos + 1]]) as usize;
		pos += 2;
		if pos + len > avcc.len() {
			return Err(crate::Error::Other(anyhow::anyhow!("avcC truncated in SPS NAL")));
		}
		sps.push(Bytes::copy_from_slice(&avcc[pos..pos + len]));
		pos += len;
	}

	if pos >= avcc.len() {
		return Ok((sps, pps));
	}
	let num_pps = avcc[pos] as usize;
	pos += 1;
	for _ in 0..num_pps {
		if pos + 2 > avcc.len() {
			return Err(crate::Error::Other(anyhow::anyhow!("avcC truncated in PPS table")));
		}
		let len = u16::from_be_bytes([avcc[pos], avcc[pos + 1]]) as usize;
		pos += 2;
		if pos + len > avcc.len() {
			return Err(crate::Error::Other(anyhow::anyhow!("avcC truncated in PPS NAL")));
		}
		pps.push(Bytes::copy_from_slice(&avcc[pos..pos + len]));
		pos += len;
	}

	Ok((sps, pps))
}
