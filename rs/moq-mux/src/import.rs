//! Format dispatchers for callers who only have a format string.
//!
//! [`Framed`] is the entry point when the caller already has whole
//! frames (the typical case for files and reassembled network input).
//! [`Stream`] is for raw byte streams where frame boundaries have to
//! be inferred (piped Annex-B H.264, an fMP4 reader, …). Both pick a
//! concrete importer from a [`FramedFormat`] / [`StreamFormat`] string.
//! The concrete importers themselves live with their format under
//! [`crate::container`] or [`crate::codec`].

use std::{fmt, str::FromStr};

use bytes::Buf;

use crate::Result;

/// The supported framed formats (known frame boundaries).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum FramedFormat {
	/// H264 with AVCC framing (length-prefixed NALUs, out-of-band SPS/PPS).
	Avc1,
	/// H264 with Annex B framing (start code prefixed, inline SPS/PPS).
	Avc3,
	/// fMP4/CMAF container.
	Fmp4,
	/// aka H265 with inline SPS/PPS
	Hev1,
	/// AV1 with inline sequence headers
	Av01,
	/// Raw AAC frames (not ADTS).
	Aac,
	/// Raw Opus frames (not Ogg).
	Opus,
	/// Matroska / WebM container.
	Mkv,
	/// MPEG-TS (transport stream) container.
	Ts,
	// New variants go at the end: this enum has no repr, so inserting in the
	// middle would shift the implicit discriminants of everything after it.
	/// VP8 (one frame per buffer; not self-delimiting).
	Vp8,
	/// VP9 (one frame per buffer; not self-delimiting).
	Vp9,
	/// FLV (Flash Video / RTMP) container.
	Flv,
}

impl FromStr for FramedFormat {
	type Err = crate::Error;

	fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
		match s {
			"avc1" | "avcc" => Ok(FramedFormat::Avc1),
			"avc3" | "h264" => Ok(FramedFormat::Avc3),
			"hev1" => Ok(FramedFormat::Hev1),
			"fmp4" | "cmaf" => Ok(FramedFormat::Fmp4),
			"av01" | "av1" | "av1c" | "av1C" => Ok(FramedFormat::Av01),
			"aac" => Ok(FramedFormat::Aac),
			"opus" => Ok(FramedFormat::Opus),
			"mkv" | "webm" | "matroska" => Ok(FramedFormat::Mkv),
			"ts" | "mpegts" | "mpeg2ts" | "m2ts" => Ok(FramedFormat::Ts),
			"vp8" | "vp08" => Ok(FramedFormat::Vp8),
			"vp9" | "vp09" => Ok(FramedFormat::Vp9),
			"flv" => Ok(FramedFormat::Flv),
			_ => Err(crate::Error::UnknownFormat(s.to_string())),
		}
	}
}

impl fmt::Display for FramedFormat {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match *self {
			FramedFormat::Avc1 => write!(f, "avc1"),
			FramedFormat::Avc3 => write!(f, "avc3"),
			FramedFormat::Fmp4 => write!(f, "fmp4"),
			FramedFormat::Hev1 => write!(f, "hev1"),
			FramedFormat::Av01 => write!(f, "av01"),
			FramedFormat::Aac => write!(f, "aac"),
			FramedFormat::Opus => write!(f, "opus"),
			FramedFormat::Mkv => write!(f, "mkv"),
			FramedFormat::Ts => write!(f, "ts"),
			FramedFormat::Vp8 => write!(f, "vp8"),
			FramedFormat::Vp9 => write!(f, "vp9"),
			FramedFormat::Flv => write!(f, "flv"),
		}
	}
}

impl From<StreamFormat> for FramedFormat {
	fn from(format: StreamFormat) -> Self {
		match format {
			StreamFormat::Avc3 => FramedFormat::Avc3,
			StreamFormat::Fmp4 => FramedFormat::Fmp4,
			StreamFormat::Hev1 => FramedFormat::Hev1,
			StreamFormat::Av01 => FramedFormat::Av01,
			StreamFormat::Mkv => FramedFormat::Mkv,
			StreamFormat::Ts => FramedFormat::Ts,
			StreamFormat::Flv => FramedFormat::Flv,
		}
	}
}

enum FramedKind {
	/// H.264 avc3 (Annex-B, inline SPS/PPS). The split owns byte parsing; the
	/// import publishes.
	Avc3 {
		split: crate::codec::h264::Split,
		import: crate::publish::Published<crate::codec::h264::Import>,
	},
	/// H.264 avc1 (length-prefixed NALU, out-of-band avcC). No splitter: each
	/// access unit is wrapped directly. `length_size` is the NALU length prefix
	/// width read from the avcC.
	Avc1 {
		length_size: usize,
		import: crate::publish::Published<crate::codec::h264::Import>,
	},
	// Boxed because it's a large struct and clippy complains about the size.
	Fmp4(Box<crate::container::fmp4::Import>),
	Hev1 {
		split: crate::codec::h265::Split,
		import: crate::publish::Published<crate::codec::h265::Import>,
	},
	Av01 {
		split: crate::codec::av1::Split,
		import: crate::publish::Published<crate::codec::av1::Import>,
	},
	Vp8(crate::publish::Published<crate::codec::vp8::Import>),
	Vp9(crate::publish::Published<crate::codec::vp9::Import>),
	Aac(crate::publish::Published<crate::codec::aac::Import>),
	Opus(crate::publish::Published<crate::codec::opus::Import>),
	// Boxed for the same reason as Fmp4.
	Mkv(Box<crate::container::mkv::Import>),
	// Boxed for the same reason as Fmp4.
	Ts(Box<crate::container::ts::Import>),
	// Boxed for the same reason as Fmp4.
	Flv(Box<crate::container::flv::Import>),
}

/// An importer for formats with known frame boundaries.
///
/// This supports all formats and should be used when the caller knows the frame boundaries.
pub struct Framed {
	decoder: FramedKind,
}

/// Build an H.264 avc3 split + import pair, resolving the config and consuming `buf`.
///
/// The import reads `buf` for the codec config without consuming it; the split
/// then consumes it, seeding its parameter-set cache.
fn build_h264_avc3<T: Buf + AsRef<[u8]>>(
	track: moq_net::TrackProducer,
	catalog: crate::catalog::Producer,
	buf: &mut T,
) -> Result<(
	crate::codec::h264::Split,
	crate::publish::Published<crate::codec::h264::Import>,
)> {
	let mut import = crate::codec::h264::Import::from_track(track);
	import.initialize(buf)?;
	let mut split = crate::codec::h264::Split::new();
	split.seed(buf)?;
	Ok((split, crate::publish::Published::new(catalog, import)))
}

/// Build an H.264 avc1 import, resolving the config and the NALU length size from
/// the avcC, and consuming `buf`. avc1 has no splitter: each access unit is
/// wrapped directly via [`crate::codec::h264::avc1_frame`].
fn build_h264_avc1<T: Buf + AsRef<[u8]>>(
	track: moq_net::TrackProducer,
	catalog: crate::catalog::Producer,
	buf: &mut T,
) -> Result<(usize, crate::publish::Published<crate::codec::h264::Import>)> {
	let mut import = crate::codec::h264::Import::from_track(track);
	import.initialize(buf)?;
	let length_size = crate::codec::h264::Avcc::parse(buf.as_ref())?.length_size;
	buf.advance(buf.remaining());
	Ok((length_size, crate::publish::Published::new(catalog, import)))
}

/// Build an H.265 split + import pair, resolving the config and consuming `buf`.
fn build_h265<T: Buf + AsRef<[u8]>>(
	track: moq_net::TrackProducer,
	catalog: crate::catalog::Producer,
	buf: &mut T,
) -> Result<(
	crate::codec::h265::Split,
	crate::publish::Published<crate::codec::h265::Import>,
)> {
	let mut import = crate::codec::h265::Import::from_track(track);
	import.initialize(buf)?;
	let mut split = crate::codec::h265::Split::new();
	split.seed(buf)?;
	Ok((split, crate::publish::Published::new(catalog, import)))
}

/// Build an AV1 split + import pair, resolving the config and consuming `buf`.
fn build_av1<T: Buf + AsRef<[u8]>>(
	track: moq_net::TrackProducer,
	catalog: crate::catalog::Producer,
	buf: &mut T,
) -> Result<(
	crate::codec::av1::Split,
	crate::publish::Published<crate::codec::av1::Import>,
)> {
	let mut import = crate::codec::av1::Import::from_track(track);
	import.initialize(buf)?;
	let mut split = crate::codec::av1::Split::new();
	// av1C (leading 0x81, ISO/IEC 14496-15) is config-only and not fed to the
	// splitter; raw OBUs seed it so the sequence header prefixes the first
	// keyframe. Mirror the importer's av1C detection exactly.
	let data = buf.as_ref();
	if data.len() >= 16 && data[0] == 0x81 {
		buf.advance(buf.remaining());
	} else {
		split.seed(buf)?;
	}
	Ok((split, crate::publish::Published::new(catalog, import)))
}

impl Framed {
	/// Create a new framed importer with the given format and initialization data.
	///
	/// The buffer will be fully consumed, or an error will be returned.
	pub fn new<T: Buf + AsRef<[u8]>>(
		mut broadcast: moq_net::BroadcastProducer,
		catalog: crate::catalog::Producer,
		format: FramedFormat,
		buf: &mut T,
	) -> Result<Self> {
		let decoder = match format {
			FramedFormat::Avc1 => {
				let track = crate::publish::unique_track(&mut broadcast, ".avc1")?;
				let (length_size, import) = build_h264_avc1(track, catalog, buf)?;
				FramedKind::Avc1 { length_size, import }
			}
			FramedFormat::Avc3 => {
				let track = crate::publish::unique_track(&mut broadcast, ".avc3")?;
				let (split, import) = build_h264_avc3(track, catalog, buf)?;
				FramedKind::Avc3 { split, import }
			}
			FramedFormat::Fmp4 => {
				let mut decoder = Box::new(crate::container::fmp4::Import::new(broadcast, catalog));
				decoder.decode(buf)?;
				FramedKind::Fmp4(decoder)
			}
			FramedFormat::Hev1 => {
				let track = crate::publish::unique_track(&mut broadcast, ".hev1")?;
				let (split, import) = build_h265(track, catalog, buf)?;
				FramedKind::Hev1 { split, import }
			}
			FramedFormat::Av01 => {
				let track = crate::publish::unique_track(&mut broadcast, ".av01")?;
				let (split, import) = build_av1(track, catalog, buf)?;
				FramedKind::Av01 { split, import }
			}
			FramedFormat::Vp8 => {
				let track = crate::publish::unique_track(&mut broadcast, ".vp8")?;
				let mut decoder = crate::codec::vp8::Import::from_track(track);
				decoder.initialize(buf)?;
				FramedKind::Vp8(crate::publish::Published::new(catalog, decoder))
			}
			FramedFormat::Vp9 => {
				let track = crate::publish::unique_track(&mut broadcast, ".vp09")?;
				let mut decoder = crate::codec::vp9::Import::from_track(track);
				decoder.initialize(buf)?;
				FramedKind::Vp9(crate::publish::Published::new(catalog, decoder))
			}
			FramedFormat::Aac => {
				let config = crate::codec::aac::Config::parse(buf)?;
				let track = crate::publish::unique_track(&mut broadcast, ".aac")?;
				let import = crate::codec::aac::Import::from_track(track, config)?;
				FramedKind::Aac(crate::publish::Published::new(catalog, import))
			}
			FramedFormat::Opus => {
				let config = crate::codec::opus::Config::parse(buf)?;
				let track = crate::publish::unique_track(&mut broadcast, ".opus")?;
				let import = crate::codec::opus::Import::from_track(track, config)?;
				FramedKind::Opus(crate::publish::Published::new(catalog, import))
			}
			FramedFormat::Mkv => {
				let mut decoder = Box::new(crate::container::mkv::Import::new(broadcast, catalog));
				decoder.decode(buf)?;
				FramedKind::Mkv(decoder)
			}
			FramedFormat::Ts => {
				let mut decoder = Box::new(crate::container::ts::Import::new(broadcast, catalog));
				decoder.decode(buf)?;
				FramedKind::Ts(decoder)
			}
			FramedFormat::Flv => {
				let mut decoder = Box::new(crate::container::flv::Import::new(broadcast, catalog));
				decoder.decode(buf)?;
				FramedKind::Flv(decoder)
			}
		};

		if buf.has_remaining() {
			return Err(crate::Error::BufferNotConsumed);
		}

		Ok(Self { decoder })
	}

	/// Create a new framed importer that publishes on an existing track.
	///
	/// Only single-track formats are supported. Container formats that may
	/// create multiple MoQ tracks need an explicit track mapping API.
	pub fn new_with_track<T: Buf + AsRef<[u8]>>(
		track: moq_net::TrackProducer,
		catalog: crate::catalog::Producer,
		format: FramedFormat,
		buf: &mut T,
	) -> anyhow::Result<Self> {
		let decoder = match format {
			FramedFormat::Avc1 => {
				let (length_size, import) = build_h264_avc1(track, catalog, buf)?;
				FramedKind::Avc1 { length_size, import }
			}
			FramedFormat::Avc3 => {
				let (split, import) = build_h264_avc3(track, catalog, buf)?;
				FramedKind::Avc3 { split, import }
			}
			FramedFormat::Hev1 => {
				let (split, import) = build_h265(track, catalog, buf)?;
				FramedKind::Hev1 { split, import }
			}
			FramedFormat::Av01 => {
				let (split, import) = build_av1(track, catalog, buf)?;
				FramedKind::Av01 { split, import }
			}
			FramedFormat::Vp8 => {
				let mut decoder = crate::codec::vp8::Import::from_track(track);
				decoder.initialize(buf)?;
				FramedKind::Vp8(crate::publish::Published::new(catalog, decoder))
			}
			FramedFormat::Vp9 => {
				let mut decoder = crate::codec::vp9::Import::from_track(track);
				decoder.initialize(buf)?;
				FramedKind::Vp9(crate::publish::Published::new(catalog, decoder))
			}
			FramedFormat::Aac => {
				let config = crate::codec::aac::Config::parse(buf)?;
				let import = crate::codec::aac::Import::from_track(track, config)?;
				FramedKind::Aac(crate::publish::Published::new(catalog, import))
			}
			FramedFormat::Opus => {
				let config = crate::codec::opus::Config::parse(buf)?;
				let import = crate::codec::opus::Import::from_track(track, config)?;
				FramedKind::Opus(crate::publish::Published::new(catalog, import))
			}
			FramedFormat::Fmp4 | FramedFormat::Mkv | FramedFormat::Ts | FramedFormat::Flv => {
				anyhow::bail!("{format} can publish multiple tracks")
			}
		};

		anyhow::ensure!(!buf.has_remaining(), "buffer was not fully consumed");

		Ok(Self { decoder })
	}

	/// Finish the decoder, flushing any buffered data.
	pub fn finish(&mut self) -> Result<()> {
		match self.decoder {
			FramedKind::Avc3 { ref mut import, .. } => import.finish(),
			FramedKind::Avc1 { ref mut import, .. } => import.finish(),
			FramedKind::Fmp4(ref mut decoder) => decoder.finish(),
			FramedKind::Hev1 { ref mut import, .. } => import.finish(),
			FramedKind::Av01 { ref mut import, .. } => import.finish(),
			FramedKind::Vp8(ref mut decoder) => decoder.finish(),
			FramedKind::Vp9(ref mut decoder) => decoder.finish(),
			FramedKind::Aac(ref mut decoder) => decoder.finish(),
			FramedKind::Opus(ref mut decoder) => decoder.finish(),
			FramedKind::Mkv(ref mut decoder) => decoder.finish(),
			FramedKind::Ts(ref mut decoder) => decoder.finish().map_err(Into::into),
			FramedKind::Flv(ref mut decoder) => decoder.finish().map_err(Into::into),
		}
	}

	/// Close the current group and open the next one at `sequence`.
	pub fn seek(&mut self, sequence: u64) -> Result<()> {
		match self.decoder {
			FramedKind::Avc3 {
				ref mut split,
				ref mut import,
			} => {
				split.reset();
				import.seek(sequence)
			}
			FramedKind::Avc1 { ref mut import, .. } => import.seek(sequence),
			FramedKind::Fmp4(ref mut decoder) => decoder.seek(sequence),
			FramedKind::Hev1 {
				ref mut split,
				ref mut import,
			} => {
				split.reset();
				import.seek(sequence)
			}
			FramedKind::Av01 {
				ref mut split,
				ref mut import,
			} => {
				split.reset();
				import.seek(sequence)
			}
			FramedKind::Vp8(ref mut decoder) => decoder.seek(sequence),
			FramedKind::Vp9(ref mut decoder) => decoder.seek(sequence),
			FramedKind::Aac(ref mut decoder) => decoder.seek(sequence),
			FramedKind::Opus(ref mut decoder) => decoder.seek(sequence),
			FramedKind::Mkv(ref mut decoder) => decoder.seek(sequence),
			FramedKind::Ts(ref mut decoder) => decoder.seek(sequence).map_err(Into::into),
			FramedKind::Flv(ref mut decoder) => decoder.seek(sequence).map_err(Into::into),
		}
	}

	/// Return the single track produced by this importer.
	pub fn track(&self) -> Result<&moq_net::TrackProducer> {
		match self.decoder {
			FramedKind::Avc3 { ref import, .. } => Ok(import.track()),
			FramedKind::Avc1 { ref import, .. } => Ok(import.track()),
			FramedKind::Fmp4(_) => Err(crate::Error::MultipleTracks("fmp4")),
			FramedKind::Hev1 { ref import, .. } => Ok(import.track()),
			FramedKind::Av01 { ref import, .. } => Ok(import.track()),
			FramedKind::Vp8(ref decoder) => Ok(decoder.track()),
			FramedKind::Vp9(ref decoder) => Ok(decoder.track()),
			FramedKind::Aac(ref decoder) => Ok(decoder.track()),
			FramedKind::Opus(ref decoder) => Ok(decoder.track()),
			FramedKind::Mkv(_) => Err(crate::Error::MultipleTracks("mkv")),
			FramedKind::Ts(_) => Err(crate::Error::MultipleTracks("ts")),
			FramedKind::Flv(_) => Err(crate::Error::MultipleTracks("flv")),
		}
	}

	/// Decode a frame from the given buffer.
	pub fn decode_frame<T: Buf + AsRef<[u8]>>(&mut self, buf: &mut T, pts: Option<moq_net::Timestamp>) -> Result<()> {
		match self.decoder {
			FramedKind::Avc3 {
				ref mut split,
				ref mut import,
			} => {
				// Framed hands over one whole access unit per call, so flush to
				// emit it rather than waiting for the next start code.
				let mut frames = split.decode(buf, pts)?;
				frames.extend(split.flush(pts)?);
				import.decode(frames)?;
			}
			FramedKind::Avc1 {
				length_size,
				ref mut import,
			} => {
				let pts = pts.ok_or(crate::codec::h264::Error::MissingTimestamp)?;
				let frame = crate::codec::h264::avc1_frame(buf.as_ref(), length_size, pts)?;
				import.decode([frame])?;
				buf.advance(buf.remaining());
			}
			FramedKind::Fmp4(ref mut decoder) => decoder.decode(buf)?,
			FramedKind::Hev1 {
				ref mut split,
				ref mut import,
			} => {
				let mut frames = split.decode(buf, pts)?;
				frames.extend(split.flush(pts)?);
				import.decode(frames)?;
			}
			FramedKind::Av01 {
				ref mut split,
				ref mut import,
			} => {
				let mut frames = split.decode(buf, pts)?;
				frames.extend(split.flush(pts)?);
				import.decode(frames)?;
			}
			FramedKind::Vp8(ref mut decoder) => decoder.decoding(|d| d.decode_frame(buf, pts))?,
			FramedKind::Vp9(ref mut decoder) => decoder.decoding(|d| d.decode_frame(buf, pts))?,
			FramedKind::Aac(ref mut decoder) => decoder.decode(buf, pts)?,
			FramedKind::Opus(ref mut decoder) => decoder.decode_buf(buf, pts)?,
			FramedKind::Mkv(ref mut decoder) => {
				let _ = pts;
				decoder.decode(buf)?;
			}
			FramedKind::Ts(ref mut decoder) => {
				let _ = pts;
				decoder.decode(buf)?;
			}
			FramedKind::Flv(ref mut decoder) => {
				let _ = pts;
				decoder.decode(buf)?;
			}
		}

		if buf.has_remaining() {
			return Err(crate::Error::BufferNotConsumed);
		}

		Ok(())
	}
}

// Lift an already-built, catalog-attached opus importer into a `Framed` so callers
// that build their config out-of-band (e.g. moq-gst, which constructs `opus::Config`
// from gstreamer caps instead of an OpusHead buffer) can keep using `.into()`.
impl From<crate::publish::Published<crate::codec::opus::Import>> for Framed {
	fn from(opus: crate::publish::Published<crate::codec::opus::Import>) -> Self {
		Self {
			decoder: FramedKind::Opus(opus),
		}
	}
}

impl From<crate::publish::Published<crate::codec::aac::Import>> for Framed {
	fn from(aac: crate::publish::Published<crate::codec::aac::Import>) -> Self {
		Self {
			decoder: FramedKind::Aac(aac),
		}
	}
}

#[cfg(test)]
mod tests {
	use std::time::Duration;

	use bytes::Bytes;

	use super::*;
	use moq_net::Timestamp;

	fn opus_head() -> Vec<u8> {
		let mut head = Vec::with_capacity(19);
		head.extend_from_slice(b"OpusHead");
		head.push(1);
		head.push(2);
		head.extend_from_slice(&0u16.to_le_bytes());
		head.extend_from_slice(&48000u32.to_le_bytes());
		head.extend_from_slice(&0u16.to_le_bytes());
		head.push(0);
		head
	}

	fn h264_init() -> Vec<u8> {
		let mut init = Vec::new();
		init.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
		init.extend_from_slice(&[
			0x67, 0x64, 0x00, 0x1f, 0xac, 0x24, 0x84, 0x01, 0x40, 0x16, 0xec, 0x04, 0x40, 0x00, 0x00, 0x03, 0x00, 0x40,
			0x00, 0x00, 0x0c, 0x23, 0xc6, 0x0c, 0x92,
		]);
		init.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
		init.extend_from_slice(&[0x68, 0xee, 0x32, 0xc8, 0xb0]);
		init
	}

	fn new_broadcast() -> (moq_net::BroadcastProducer, crate::catalog::Producer) {
		let mut broadcast = moq_net::BroadcastInfo::new().produce();
		let catalog = crate::catalog::Producer::new(&mut broadcast).unwrap();
		(broadcast, catalog)
	}

	#[tokio::test(start_paused = true)]
	async fn fixed_track_opus_uses_existing_name_and_delivers_frames() {
		let (mut broadcast, catalog) = new_broadcast();
		// Legacy-container codecs write micro-timestamped frames, so a caller-supplied
		// fixed track must declare the matching timescale.
		let track = broadcast
			.create_track(
				"requested-audio",
				moq_net::TrackInfo::default().with_timescale(hang::container::TIMESCALE),
			)
			.unwrap();
		let consumer = track.subscribe(None);
		let init = opus_head();
		let mut init = init.as_slice();

		let mut framed = Framed::new_with_track(track, catalog.clone(), FramedFormat::Opus, &mut init).unwrap();

		assert_eq!(framed.track().unwrap().name(), "requested-audio");
		let snapshot = catalog.snapshot();
		assert!(snapshot.audio.renditions.contains_key("requested-audio"));
		assert!(!snapshot.audio.renditions.contains_key("0.opus"));

		let mut media = crate::container::Consumer::new(consumer, crate::catalog::hang::Container::Legacy);
		let payload = b"opus payload".to_vec();
		let mut frame = payload.as_slice();
		framed
			.decode_frame(&mut frame, Some(Timestamp::from_micros(1_000).unwrap()))
			.unwrap();

		let frame = tokio::time::timeout(Duration::from_secs(1), media.read())
			.await
			.unwrap()
			.unwrap()
			.unwrap();
		assert_eq!(frame.payload, payload);
		assert_eq!(frame.timestamp, Timestamp::from_micros(1_000).unwrap());

		framed.finish().unwrap();
	}

	#[tokio::test(start_paused = true)]
	async fn unique_track_opus_delivers_frames_via_broadcast() {
		let (broadcast, catalog) = new_broadcast();
		let init = opus_head();
		let mut init = init.as_slice();

		// The broadcast path mints a unique track and attaches its catalog rendition.
		let mut framed = Framed::new(broadcast, catalog.clone(), FramedFormat::Opus, &mut init).unwrap();

		assert_eq!(framed.track().unwrap().name(), "0.opus");
		assert!(catalog.snapshot().audio.renditions.contains_key("0.opus"));

		// Frames published through the minted producer are delivered.
		let subscriber = framed.track().unwrap().subscribe(None);
		let mut media = crate::container::Consumer::new(subscriber, crate::catalog::hang::Container::Legacy);

		let payload = b"opus payload".to_vec();
		let mut frame = payload.as_slice();
		framed
			.decode_frame(&mut frame, Some(Timestamp::from_micros(2_000).unwrap()))
			.unwrap();

		let frame = tokio::time::timeout(Duration::from_secs(1), media.read())
			.await
			.unwrap()
			.unwrap()
			.unwrap();
		assert_eq!(frame.payload, payload);
		assert_eq!(frame.timestamp, Timestamp::from_micros(2_000).unwrap());

		framed.finish().unwrap();

		// Dropping the importer retires its rendition from the shared catalog.
		drop(framed);
		assert!(!catalog.snapshot().audio.renditions.contains_key("0.opus"));
	}

	#[tokio::test(start_paused = true)]
	async fn opus_import_serves_track_request() {
		// The on-demand path: build straight from a TrackRequest, no broadcast/catalog.
		let request = moq_net::TrackRequest::new("audio");
		let config = crate::codec::opus::Config {
			sample_rate: 48_000,
			channel_count: 2,
		};
		let mut import = crate::codec::opus::Import::new(request, config).unwrap();

		assert_eq!(import.track().name(), "audio");
		assert!(import.catalog().audio.renditions.contains_key("audio"));

		// Accepting the request yields a working producer that delivers frames.
		let subscriber = import.track().subscribe(None);
		let mut media = crate::container::Consumer::new(subscriber, crate::catalog::hang::Container::Legacy);

		let payload = b"opus payload".to_vec();
		let mut buf = payload.as_slice();
		import
			.decode_buf(&mut buf, Some(Timestamp::from_micros(1_000).unwrap()))
			.unwrap();

		let frame = tokio::time::timeout(Duration::from_secs(1), media.read())
			.await
			.unwrap()
			.unwrap()
			.unwrap();
		assert_eq!(frame.payload, payload);

		import.finish().unwrap();
	}

	#[tokio::test(start_paused = true)]
	async fn fixed_track_h264_uses_existing_name_in_catalog() {
		let (mut broadcast, catalog) = new_broadcast();
		let track = broadcast.create_track("camera", None).unwrap();
		let init = h264_init();
		let mut init = init.as_slice();

		let framed = Framed::new_with_track(track, catalog.clone(), FramedFormat::Avc3, &mut init).unwrap();

		assert_eq!(framed.track().unwrap().name(), "camera");
		let snapshot = catalog.snapshot();
		let video = snapshot.video.renditions.get("camera").unwrap();
		assert_eq!(video.coded_width, Some(1280));
		assert_eq!(video.coded_height, Some(720));
		assert!(!snapshot.video.renditions.contains_key("0.avc3"));
	}

	#[test]
	fn fixed_track_rejects_multi_track_formats() {
		for format in [FramedFormat::Fmp4, FramedFormat::Mkv, FramedFormat::Ts] {
			let (mut broadcast, catalog) = new_broadcast();
			let track = broadcast.create_track("media", None).unwrap();
			let mut init = Bytes::new();

			let err = match Framed::new_with_track(track, catalog, format, &mut init) {
				Ok(_) => panic!("multi-track format should be rejected"),
				Err(err) => err,
			};
			assert!(err.to_string().contains("multiple tracks"));
		}
	}

	/// A changed key frame just updates the rendition in place; there are no fixed
	/// tracks to reject a reconfiguration, so the second key frame succeeds.
	#[tokio::test(start_paused = true)]
	async fn reconfiguration_updates_in_place() {
		let (mut broadcast, catalog) = new_broadcast();
		let track = broadcast
			.create_track(
				"video",
				moq_net::TrackInfo::default().with_timescale(hang::container::TIMESCALE),
			)
			.unwrap();
		let mut init = Bytes::new();
		let mut framed = Framed::new_with_track(track, catalog, FramedFormat::Vp8, &mut init).unwrap();

		let mut first = Bytes::from_static(&[0x10, 0x00, 0x00, 0x9d, 0x01, 0x2a, 0x40, 0x01, 0xf0, 0x00]);
		framed
			.decode_frame(&mut first, Some(Timestamp::from_micros(0).unwrap()))
			.unwrap();

		let mut second = Bytes::from_static(&[0x10, 0x00, 0x00, 0x9d, 0x01, 0x2a, 0x80, 0x02, 0xe0, 0x01]);
		framed
			.decode_frame(&mut second, Some(Timestamp::from_micros(33_000).unwrap()))
			.unwrap();
	}
}

// -- stream dispatcher --

/// Formats that support stream decoding (unknown frame boundaries).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum StreamFormat {
	/// aka H264 with inline SPS/PPS
	Avc3,
	/// fMP4/CMAF container.
	Fmp4,
	/// aka H265 with inline SPS/PPS
	Hev1,
	/// AV1 with inline sequence headers
	Av01,
	/// Matroska / WebM container.
	Mkv,
	/// MPEG-TS (transport stream) container.
	Ts,
	/// FLV (Flash Video / RTMP) container.
	Flv,
}

impl FromStr for StreamFormat {
	type Err = crate::Error;

	fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
		match s {
			"avc3" | "h264" => Ok(StreamFormat::Avc3),
			"hev1" => Ok(StreamFormat::Hev1),
			"fmp4" | "cmaf" => Ok(StreamFormat::Fmp4),
			"av01" | "av1" | "av1c" | "av1C" => Ok(StreamFormat::Av01),
			"mkv" | "webm" | "matroska" => Ok(StreamFormat::Mkv),
			"ts" | "mpegts" | "mpeg2ts" | "m2ts" => Ok(StreamFormat::Ts),
			"flv" => Ok(StreamFormat::Flv),
			_ => Err(crate::Error::UnknownFormat(s.to_string())),
		}
	}
}

impl fmt::Display for StreamFormat {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match *self {
			StreamFormat::Avc3 => write!(f, "avc3"),
			StreamFormat::Fmp4 => write!(f, "fmp4"),
			StreamFormat::Hev1 => write!(f, "hev1"),
			StreamFormat::Av01 => write!(f, "av01"),
			StreamFormat::Mkv => write!(f, "mkv"),
			StreamFormat::Ts => write!(f, "ts"),
			StreamFormat::Flv => write!(f, "flv"),
		}
	}
}

enum StreamKind {
	/// H.264 in avc3 wire shape (Annex-B with inline SPS/PPS). The split owns
	/// byte parsing; the import publishes.
	Avc3 {
		split: crate::codec::h264::Split,
		import: crate::publish::Published<crate::codec::h264::Import>,
	},
	// Boxed because it's a large struct and clippy complains about the size.
	Fmp4(Box<crate::container::fmp4::Import>),
	Hev1 {
		split: crate::codec::h265::Split,
		import: crate::publish::Published<crate::codec::h265::Import>,
	},
	Av01 {
		split: crate::codec::av1::Split,
		import: crate::publish::Published<crate::codec::av1::Import>,
	},
	// Boxed for the same reason as Fmp4.
	Mkv(Box<crate::container::mkv::Import>),
	// Boxed for the same reason as Fmp4.
	Ts(Box<crate::container::ts::Import>),
	// Boxed for the same reason as Fmp4.
	Flv(Box<crate::container::flv::Import>),
}

/// An importer for formats that support stream decoding (unknown frame boundaries).
///
/// This includes formats like H.264 (AVC3), H.265 (HEV1), and fMP4/CMAF.
/// Use this when the caller does not know the frame boundaries.
pub struct Stream {
	decoder: StreamKind,
}

impl Stream {
	/// Create a new stream importer with the given format.
	pub fn new(
		mut broadcast: moq_net::BroadcastProducer,
		catalog: crate::catalog::Producer,
		format: StreamFormat,
	) -> Result<Self> {
		let decoder = match format {
			StreamFormat::Avc3 => {
				let track = crate::publish::unique_track(&mut broadcast, ".avc3")?;
				let import = crate::codec::h264::Import::from_track(track);
				let split = crate::codec::h264::Split::new();
				StreamKind::Avc3 {
					split,
					import: crate::publish::Published::new(catalog, import),
				}
			}
			StreamFormat::Fmp4 => StreamKind::Fmp4(Box::new(crate::container::fmp4::Import::new(broadcast, catalog))),
			StreamFormat::Hev1 => {
				let track = crate::publish::unique_track(&mut broadcast, ".hev1")?;
				let import = crate::codec::h265::Import::from_track(track);
				StreamKind::Hev1 {
					split: crate::codec::h265::Split::new(),
					import: crate::publish::Published::new(catalog, import),
				}
			}
			StreamFormat::Av01 => {
				let track = crate::publish::unique_track(&mut broadcast, ".av01")?;
				let import = crate::codec::av1::Import::from_track(track);
				StreamKind::Av01 {
					split: crate::codec::av1::Split::new(),
					import: crate::publish::Published::new(catalog, import),
				}
			}
			StreamFormat::Mkv => StreamKind::Mkv(Box::new(crate::container::mkv::Import::new(broadcast, catalog))),
			StreamFormat::Ts => StreamKind::Ts(Box::new(crate::container::ts::Import::new(broadcast, catalog))),
			StreamFormat::Flv => StreamKind::Flv(Box::new(crate::container::flv::Import::new(broadcast, catalog))),
		};

		Ok(Self { decoder })
	}

	/// Initialize the decoder with the given buffer and populate the broadcast.
	///
	/// This is not required for self-describing formats like fMP4 or AVC3.
	///
	/// The buffer will be fully consumed, or an error will be returned.
	pub fn initialize<T: Buf + AsRef<[u8]>>(&mut self, buf: &mut T) -> Result<()> {
		match self.decoder {
			StreamKind::Avc3 {
				ref mut split,
				ref mut import,
			} => {
				import.decoding(|d| d.initialize(buf))?;
				split.seed(buf)?;
			}
			StreamKind::Fmp4(ref mut decoder) => decoder.decode(buf)?,
			StreamKind::Hev1 {
				ref mut split,
				ref mut import,
			} => {
				import.decoding(|d| d.initialize(buf))?;
				split.seed(buf)?;
			}
			StreamKind::Av01 {
				ref mut split,
				ref mut import,
			} => {
				import.decoding(|d| d.initialize(buf))?;
				let data = buf.as_ref();
				if data.len() >= 16 && data[0] == 0x81 {
					buf.advance(buf.remaining());
				} else {
					split.seed(buf)?;
				}
			}
			StreamKind::Mkv(ref mut decoder) => decoder.decode(buf)?,
			StreamKind::Ts(ref mut decoder) => decoder.decode(buf)?,
			StreamKind::Flv(ref mut decoder) => decoder.decode(buf)?,
		}

		if buf.has_remaining() {
			return Err(crate::Error::BufferNotConsumed);
		}

		Ok(())
	}

	/// Decode a stream of data from the given buffer.
	pub fn decode_stream<T: Buf + AsRef<[u8]>>(&mut self, buf: &mut T) -> Result<()> {
		match self.decoder {
			StreamKind::Avc3 {
				ref mut split,
				ref mut import,
			} => {
				let frames = split.decode(buf, None)?;
				import.decode(frames)
			}
			StreamKind::Fmp4(ref mut decoder) => decoder.decode(buf),
			StreamKind::Hev1 {
				ref mut split,
				ref mut import,
			} => {
				let frames = split.decode(buf, None)?;
				import.decode(frames)
			}
			StreamKind::Av01 {
				ref mut split,
				ref mut import,
			} => {
				let frames = split.decode(buf, None)?;
				import.decode(frames)
			}
			StreamKind::Mkv(ref mut decoder) => decoder.decode(buf),
			StreamKind::Ts(ref mut decoder) => decoder.decode(buf).map_err(Into::into),
			StreamKind::Flv(ref mut decoder) => decoder.decode(buf).map_err(Into::into),
		}
	}

	/// Finish the decoder, flushing any buffered data.
	pub fn finish(&mut self) -> Result<()> {
		match self.decoder {
			StreamKind::Avc3 {
				ref mut split,
				ref mut import,
			} => {
				let tail = split.flush(None)?;
				import.decode(tail)?;
				import.finish()
			}
			StreamKind::Fmp4(ref mut decoder) => decoder.finish(),
			StreamKind::Hev1 {
				ref mut split,
				ref mut import,
			} => {
				let tail = split.flush(None)?;
				import.decode(tail)?;
				import.finish()
			}
			StreamKind::Av01 {
				ref mut split,
				ref mut import,
			} => {
				let tail = split.flush(None)?;
				import.decode(tail)?;
				import.finish()
			}
			StreamKind::Mkv(ref mut decoder) => decoder.finish(),
			StreamKind::Ts(ref mut decoder) => decoder.finish().map_err(Into::into),
			StreamKind::Flv(ref mut decoder) => decoder.finish().map_err(Into::into),
		}
	}

	/// Close the current group and open the next one at `sequence`.
	pub fn seek(&mut self, sequence: u64) -> Result<()> {
		match self.decoder {
			StreamKind::Avc3 {
				ref mut split,
				ref mut import,
			} => {
				split.reset();
				import.seek(sequence)
			}
			StreamKind::Fmp4(ref mut decoder) => decoder.seek(sequence),
			StreamKind::Hev1 {
				ref mut split,
				ref mut import,
			} => {
				split.reset();
				import.seek(sequence)
			}
			StreamKind::Av01 {
				ref mut split,
				ref mut import,
			} => {
				split.reset();
				import.seek(sequence)
			}
			StreamKind::Mkv(ref mut decoder) => decoder.seek(sequence),
			StreamKind::Ts(ref mut decoder) => decoder.seek(sequence).map_err(Into::into),
			StreamKind::Flv(ref mut decoder) => decoder.seek(sequence).map_err(Into::into),
		}
	}

	/// Check if the decoder has read enough data to be initialized.
	pub fn is_initialized(&self) -> bool {
		match self.decoder {
			StreamKind::Avc3 { ref import, .. } => import.is_initialized(),
			StreamKind::Fmp4(ref decoder) => decoder.is_initialized(),
			StreamKind::Hev1 { ref import, .. } => import.is_initialized(),
			StreamKind::Av01 { ref import, .. } => import.is_initialized(),
			StreamKind::Mkv(ref decoder) => decoder.is_initialized(),
			StreamKind::Ts(ref decoder) => decoder.is_initialized(),
			StreamKind::Flv(ref decoder) => decoder.is_initialized(),
		}
	}
}
