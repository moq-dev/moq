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

use anyhow::Context;
use bytes::Buf;
use hang::Error;

use crate::catalog::hang::Extra;

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
	type Err = Error;

	fn from_str(s: &str) -> Result<Self, Self::Err> {
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
			_ => Err(Error::UnknownFormat(s.to_string())),
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
	/// H.264 (both avc1 and avc3 wire shapes go through this importer; mode
	/// is pinned by the caller's FramedFormat choice).
	H264(crate::codec::h264::Import<Extra>),
	// Boxed because it's a large struct and clippy complains about the size.
	Fmp4(Box<crate::container::fmp4::Import>),
	Hev1(crate::codec::h265::Import<Extra>),
	Av01(crate::codec::av1::Import),
	Vp8(crate::codec::vp8::Import),
	Vp9(crate::codec::vp9::Import),
	Aac(crate::codec::aac::Import<Extra>),
	Opus(crate::codec::opus::Import),
	// Boxed for the same reason as Fmp4.
	Mkv(Box<crate::container::mkv::Import>),
	// Boxed for the same reason as Fmp4.
	Ts(Box<crate::container::ts::Import<Extra>>),
	// Boxed for the same reason as Fmp4.
	Flv(Box<crate::container::flv::Import>),
}

/// An importer for formats with known frame boundaries.
///
/// This supports all formats and should be used when the caller knows the frame boundaries.
pub struct Framed {
	decoder: FramedKind,
}

impl Framed {
	/// Create a new framed importer with the given format and initialization data.
	///
	/// The buffer will be fully consumed, or an error will be returned.
	pub fn new<T: Buf + AsRef<[u8]>>(
		broadcast: moq_net::BroadcastProducer,
		catalog: crate::catalog::Producer,
		format: FramedFormat,
		buf: &mut T,
	) -> anyhow::Result<Self> {
		use crate::codec::h264::Mode as H264Mode;
		let decoder = match format {
			FramedFormat::Avc1 => {
				let mut decoder = crate::codec::h264::Import::new(broadcast, catalog).with_mode(H264Mode::Avc1)?;
				decoder.initialize(buf)?;
				FramedKind::H264(decoder)
			}
			FramedFormat::Avc3 => {
				let mut decoder = crate::codec::h264::Import::new(broadcast, catalog).with_mode(H264Mode::Avc3)?;
				decoder.initialize(buf)?;
				FramedKind::H264(decoder)
			}
			FramedFormat::Fmp4 => {
				let mut decoder = Box::new(crate::container::fmp4::Import::new(broadcast, catalog));
				decoder.decode(buf)?;
				FramedKind::Fmp4(decoder)
			}
			FramedFormat::Hev1 => {
				let mut decoder = crate::codec::h265::Import::new(broadcast, catalog);
				decoder.initialize(buf)?;
				FramedKind::Hev1(decoder)
			}
			FramedFormat::Av01 => {
				let mut decoder = crate::codec::av1::Import::new(broadcast, catalog);
				decoder.initialize(buf)?;
				FramedKind::Av01(decoder)
			}
			FramedFormat::Vp8 => {
				let mut decoder = crate::codec::vp8::Import::new(broadcast, catalog);
				decoder.initialize(buf)?;
				FramedKind::Vp8(decoder)
			}
			FramedFormat::Vp9 => {
				let mut decoder = crate::codec::vp9::Import::new(broadcast, catalog);
				decoder.initialize(buf)?;
				FramedKind::Vp9(decoder)
			}
			FramedFormat::Aac => {
				let config = crate::codec::aac::Config::parse(buf)?;
				FramedKind::Aac(crate::codec::aac::Import::new(broadcast, catalog, config)?)
			}
			FramedFormat::Opus => {
				let config = crate::codec::opus::Config::parse(buf)?;
				FramedKind::Opus(crate::codec::opus::Import::new(broadcast, catalog, config)?)
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

		anyhow::ensure!(!buf.has_remaining(), "buffer was not fully consumed");

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
		use crate::codec::h264::Mode as H264Mode;
		let decoder = match format {
			FramedFormat::Avc1 => {
				let mut decoder =
					crate::codec::h264::Import::new_with_track(track, catalog).with_mode(H264Mode::Avc1)?;
				decoder.initialize(buf)?;
				FramedKind::H264(decoder)
			}
			FramedFormat::Avc3 => {
				let mut decoder =
					crate::codec::h264::Import::new_with_track(track, catalog).with_mode(H264Mode::Avc3)?;
				decoder.initialize(buf)?;
				FramedKind::H264(decoder)
			}
			FramedFormat::Hev1 => {
				let mut decoder = crate::codec::h265::Import::new_with_track(track, catalog);
				decoder.initialize(buf)?;
				FramedKind::Hev1(decoder)
			}
			FramedFormat::Av01 => {
				let mut decoder = crate::codec::av1::Import::new_with_track(track, catalog);
				decoder.initialize(buf)?;
				FramedKind::Av01(decoder)
			}
			FramedFormat::Vp8 => {
				let mut decoder = crate::codec::vp8::Import::new_with_track(track, catalog);
				decoder.initialize(buf)?;
				FramedKind::Vp8(decoder)
			}
			FramedFormat::Vp9 => {
				let mut decoder = crate::codec::vp9::Import::new_with_track(track, catalog);
				decoder.initialize(buf)?;
				FramedKind::Vp9(decoder)
			}
			FramedFormat::Aac => {
				let config = crate::codec::aac::Config::parse(buf)?;
				FramedKind::Aac(crate::codec::aac::Import::new_with_track(track, catalog, config)?)
			}
			FramedFormat::Opus => {
				let config = crate::codec::opus::Config::parse(buf)?;
				FramedKind::Opus(crate::codec::opus::Import::new_with_track(track, catalog, config)?)
			}
			FramedFormat::Fmp4 | FramedFormat::Mkv | FramedFormat::Ts | FramedFormat::Flv => {
				anyhow::bail!("{format} can publish multiple tracks")
			}
		};

		anyhow::ensure!(!buf.has_remaining(), "buffer was not fully consumed");

		Ok(Self { decoder })
	}

	/// Finish the decoder, flushing any buffered data.
	pub fn finish(&mut self) -> anyhow::Result<()> {
		match self.decoder {
			FramedKind::H264(ref mut decoder) => decoder.finish(),
			FramedKind::Fmp4(ref mut decoder) => decoder.finish(),
			FramedKind::Hev1(ref mut decoder) => decoder.finish(),
			FramedKind::Av01(ref mut decoder) => decoder.finish(),
			FramedKind::Vp8(ref mut decoder) => decoder.finish(),
			FramedKind::Vp9(ref mut decoder) => decoder.finish(),
			FramedKind::Aac(ref mut decoder) => decoder.finish(),
			FramedKind::Opus(ref mut decoder) => decoder.finish(),
			FramedKind::Mkv(ref mut decoder) => decoder.finish(),
			FramedKind::Ts(ref mut decoder) => decoder.finish(),
			FramedKind::Flv(ref mut decoder) => decoder.finish(),
		}
	}

	/// Close the current group and open the next one at `sequence`.
	pub fn seek(&mut self, sequence: u64) -> anyhow::Result<()> {
		match self.decoder {
			FramedKind::H264(ref mut decoder) => decoder.seek(sequence),
			FramedKind::Fmp4(ref mut decoder) => decoder.seek(sequence),
			FramedKind::Hev1(ref mut decoder) => decoder.seek(sequence),
			FramedKind::Av01(ref mut decoder) => decoder.seek(sequence),
			FramedKind::Vp8(ref mut decoder) => decoder.seek(sequence),
			FramedKind::Vp9(ref mut decoder) => decoder.seek(sequence),
			FramedKind::Aac(ref mut decoder) => decoder.seek(sequence),
			FramedKind::Opus(ref mut decoder) => decoder.seek(sequence),
			FramedKind::Mkv(ref mut decoder) => decoder.seek(sequence),
			FramedKind::Ts(ref mut decoder) => decoder.seek(sequence),
			FramedKind::Flv(ref mut decoder) => decoder.seek(sequence),
		}
	}

	/// Return the single track produced by this importer.
	pub fn track(&self) -> anyhow::Result<&moq_net::TrackProducer> {
		match self.decoder {
			FramedKind::H264(ref decoder) => decoder.track().context("H.264 track not yet created"),
			FramedKind::Fmp4(_) => anyhow::bail!("fmp4 can contain multiple tracks"),
			FramedKind::Hev1(ref decoder) => decoder.track(),
			FramedKind::Av01(ref decoder) => decoder.track(),
			FramedKind::Vp8(ref decoder) => decoder.track(),
			FramedKind::Vp9(ref decoder) => decoder.track(),
			FramedKind::Aac(ref decoder) => Ok(decoder.track()),
			FramedKind::Opus(ref decoder) => Ok(decoder.track()),
			FramedKind::Mkv(_) => anyhow::bail!("mkv can contain multiple tracks"),
			FramedKind::Ts(_) => anyhow::bail!("ts can contain multiple tracks"),
			FramedKind::Flv(_) => anyhow::bail!("flv can contain multiple tracks"),
		}
	}

	/// Decode a frame from the given buffer.
	pub fn decode_frame<T: Buf + AsRef<[u8]>>(
		&mut self,
		buf: &mut T,
		pts: Option<crate::container::Timestamp>,
	) -> anyhow::Result<()> {
		match self.decoder {
			FramedKind::H264(ref mut decoder) => decoder.decode_frame(buf, pts)?,
			FramedKind::Fmp4(ref mut decoder) => decoder.decode(buf)?,
			FramedKind::Hev1(ref mut decoder) => decoder.decode_frame(buf, pts)?,
			FramedKind::Av01(ref mut decoder) => decoder.decode_frame(buf, pts)?,
			FramedKind::Vp8(ref mut decoder) => decoder.decode_frame(buf, pts)?,
			FramedKind::Vp9(ref mut decoder) => decoder.decode_frame(buf, pts)?,
			FramedKind::Aac(ref mut decoder) => decoder.decode(buf, pts)?,
			FramedKind::Opus(ref mut decoder) => decoder.decode(buf, pts)?,
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

		anyhow::ensure!(!buf.has_remaining(), "buffer was not fully consumed");

		Ok(())
	}
}

// Lift an already-built codec importer into a `Framed` so callers that build
// their config out-of-band (e.g. moq-gst, which constructs `opus::Config` from
// gstreamer caps instead of an OpusHead buffer) can keep using `.into()`.
impl From<crate::codec::opus::Import> for Framed {
	fn from(opus: crate::codec::opus::Import) -> Self {
		Self {
			decoder: FramedKind::Opus(opus),
		}
	}
}

impl From<crate::codec::aac::Import<Extra>> for Framed {
	fn from(aac: crate::codec::aac::Import<Extra>) -> Self {
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
	use crate::container::Timestamp;

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
		let mut broadcast = moq_net::Broadcast::new().produce();
		let catalog = crate::catalog::Producer::new(&mut broadcast).unwrap();
		(broadcast, catalog)
	}

	#[tokio::test(start_paused = true)]
	async fn fixed_track_opus_uses_existing_name_and_delivers_frames() {
		let (mut broadcast, catalog) = new_broadcast();
		let track = broadcast.create_track(moq_net::Track::new("requested-audio")).unwrap();
		let consumer = track.consume();
		let init = opus_head();
		let mut init = init.as_slice();

		let mut framed = Framed::new_with_track(track, catalog.clone(), FramedFormat::Opus, &mut init).unwrap();

		assert_eq!(framed.track().unwrap().name, "requested-audio");
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
	async fn fixed_track_h264_uses_existing_name_in_catalog() {
		let (mut broadcast, catalog) = new_broadcast();
		let track = broadcast.create_track(moq_net::Track::new("camera")).unwrap();
		let init = h264_init();
		let mut init = init.as_slice();

		let framed = Framed::new_with_track(track, catalog.clone(), FramedFormat::Avc3, &mut init).unwrap();

		assert_eq!(framed.track().unwrap().name, "camera");
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
			let track = broadcast.create_track(moq_net::Track::new("media")).unwrap();
			let mut init = Bytes::new();

			let err = match Framed::new_with_track(track, catalog, format, &mut init) {
				Ok(_) => panic!("multi-track format should be rejected"),
				Err(err) => err,
			};
			assert!(err.to_string().contains("multiple tracks"));
		}
	}

	#[tokio::test(start_paused = true)]
	async fn fixed_track_reconfiguration_errors() {
		let (mut broadcast, catalog) = new_broadcast();
		let track = broadcast.create_track(moq_net::Track::new("video")).unwrap();
		let mut init = Bytes::new();
		let mut framed = Framed::new_with_track(track, catalog, FramedFormat::Vp8, &mut init).unwrap();

		let mut first = Bytes::from_static(&[0x10, 0x00, 0x00, 0x9d, 0x01, 0x2a, 0x40, 0x01, 0xf0, 0x00]);
		framed
			.decode_frame(&mut first, Some(Timestamp::from_micros(0).unwrap()))
			.unwrap();

		let mut second = Bytes::from_static(&[0x10, 0x00, 0x00, 0x9d, 0x01, 0x2a, 0x80, 0x02, 0xe0, 0x01]);
		let err = framed
			.decode_frame(&mut second, Some(Timestamp::from_micros(33_000).unwrap()))
			.unwrap_err();
		assert!(err.to_string().contains("fixed track cannot be reconfigured"));
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
	type Err = Error;

	fn from_str(s: &str) -> Result<Self, Self::Err> {
		match s {
			"avc3" | "h264" => Ok(StreamFormat::Avc3),
			"hev1" => Ok(StreamFormat::Hev1),
			"fmp4" | "cmaf" => Ok(StreamFormat::Fmp4),
			"av01" | "av1" | "av1c" | "av1C" => Ok(StreamFormat::Av01),
			"mkv" | "webm" | "matroska" => Ok(StreamFormat::Mkv),
			"ts" | "mpegts" | "mpeg2ts" | "m2ts" => Ok(StreamFormat::Ts),
			"flv" => Ok(StreamFormat::Flv),
			_ => Err(Error::UnknownFormat(s.to_string())),
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
	/// H.264 in avc3 wire shape (Annex-B with inline SPS/PPS).
	Avc3(crate::codec::h264::Import<Extra>),
	// Boxed because it's a large struct and clippy complains about the size.
	Fmp4(Box<crate::container::fmp4::Import>),
	Hev1(crate::codec::h265::Import<Extra>),
	Av01(crate::codec::av1::Import),
	// Boxed for the same reason as Fmp4.
	Mkv(Box<crate::container::mkv::Import>),
	// Boxed for the same reason as Fmp4.
	Ts(Box<crate::container::ts::Import<Extra>>),
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
		broadcast: moq_net::BroadcastProducer,
		catalog: crate::catalog::Producer,
		format: StreamFormat,
	) -> anyhow::Result<Self> {
		use crate::codec::h264::Mode as H264Mode;
		let decoder = match format {
			StreamFormat::Avc3 => {
				StreamKind::Avc3(crate::codec::h264::Import::new(broadcast, catalog).with_mode(H264Mode::Avc3)?)
			}
			StreamFormat::Fmp4 => StreamKind::Fmp4(Box::new(crate::container::fmp4::Import::new(broadcast, catalog))),
			StreamFormat::Hev1 => StreamKind::Hev1(crate::codec::h265::Import::new(broadcast, catalog)),
			StreamFormat::Av01 => StreamKind::Av01(crate::codec::av1::Import::new(broadcast, catalog)),
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
	pub fn initialize<T: Buf + AsRef<[u8]>>(&mut self, buf: &mut T) -> anyhow::Result<()> {
		match self.decoder {
			StreamKind::Avc3(ref mut decoder) => decoder.initialize(buf)?,
			StreamKind::Fmp4(ref mut decoder) => decoder.decode(buf)?,
			StreamKind::Hev1(ref mut decoder) => decoder.initialize(buf)?,
			StreamKind::Av01(ref mut decoder) => decoder.initialize(buf)?,
			StreamKind::Mkv(ref mut decoder) => decoder.decode(buf)?,
			StreamKind::Ts(ref mut decoder) => decoder.decode(buf)?,
			StreamKind::Flv(ref mut decoder) => decoder.decode(buf)?,
		}

		anyhow::ensure!(!buf.has_remaining(), "buffer was not fully consumed");

		Ok(())
	}

	/// Decode a stream of data from the given buffer.
	pub fn decode_stream<T: Buf + AsRef<[u8]>>(&mut self, buf: &mut T) -> anyhow::Result<()> {
		match self.decoder {
			StreamKind::Avc3(ref mut decoder) => decoder.decode_stream(buf, None),
			StreamKind::Fmp4(ref mut decoder) => decoder.decode(buf),
			StreamKind::Hev1(ref mut decoder) => decoder.decode_stream(buf, None),
			StreamKind::Av01(ref mut decoder) => decoder.decode_stream(buf, None),
			StreamKind::Mkv(ref mut decoder) => decoder.decode(buf),
			StreamKind::Ts(ref mut decoder) => decoder.decode(buf),
			StreamKind::Flv(ref mut decoder) => decoder.decode(buf),
		}
	}

	/// Finish the decoder, flushing any buffered data.
	pub fn finish(&mut self) -> anyhow::Result<()> {
		match self.decoder {
			StreamKind::Avc3(ref mut decoder) => decoder.finish(),
			StreamKind::Fmp4(ref mut decoder) => decoder.finish(),
			StreamKind::Hev1(ref mut decoder) => decoder.finish(),
			StreamKind::Av01(ref mut decoder) => decoder.finish(),
			StreamKind::Mkv(ref mut decoder) => decoder.finish(),
			StreamKind::Ts(ref mut decoder) => decoder.finish(),
			StreamKind::Flv(ref mut decoder) => decoder.finish(),
		}
	}

	/// Close the current group and open the next one at `sequence`.
	pub fn seek(&mut self, sequence: u64) -> anyhow::Result<()> {
		match self.decoder {
			StreamKind::Avc3(ref mut decoder) => decoder.seek(sequence),
			StreamKind::Fmp4(ref mut decoder) => decoder.seek(sequence),
			StreamKind::Hev1(ref mut decoder) => decoder.seek(sequence),
			StreamKind::Av01(ref mut decoder) => decoder.seek(sequence),
			StreamKind::Mkv(ref mut decoder) => decoder.seek(sequence),
			StreamKind::Ts(ref mut decoder) => decoder.seek(sequence),
			StreamKind::Flv(ref mut decoder) => decoder.seek(sequence),
		}
	}

	/// Check if the decoder has read enough data to be initialized.
	pub fn is_initialized(&self) -> bool {
		match self.decoder {
			StreamKind::Avc3(ref decoder) => decoder.is_initialized(),
			StreamKind::Fmp4(ref decoder) => decoder.is_initialized(),
			StreamKind::Hev1(ref decoder) => decoder.is_initialized(),
			StreamKind::Av01(ref decoder) => decoder.is_initialized(),
			StreamKind::Mkv(ref decoder) => decoder.is_initialized(),
			StreamKind::Ts(ref decoder) => decoder.is_initialized(),
			StreamKind::Flv(ref decoder) => decoder.is_initialized(),
		}
	}
}
