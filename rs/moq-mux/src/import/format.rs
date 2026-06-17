//! Format strings for the import dispatchers.
//!
//! The formats are split along the same axis as the importers: [`TrackFormat`]
//! and [`TrackStreamFormat`] name single-codec tracks, while [`ContainerFormat`]
//! names containers that may publish more than one track. A format only appears
//! in the `*Stream` enum if that format can recover frame boundaries from a raw
//! byte stream.

use std::{fmt, str::FromStr};

/// A single-codec format with known frame boundaries (the typical case for files
/// and reassembled network input).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum TrackFormat {
	/// H264 with AVCC framing (length-prefixed NALUs, out-of-band SPS/PPS).
	Avc1,
	/// H264 with Annex B framing (start code prefixed, inline SPS/PPS).
	Avc3,
	/// aka H265 with inline SPS/PPS
	Hev1,
	/// AV1 with inline sequence headers
	Av01,
	/// Raw AAC frames (not ADTS).
	Aac,
	/// Raw Opus frames (not Ogg).
	Opus,
	/// VP8 (one frame per buffer; not self-delimiting).
	Vp8,
	/// VP9 (one frame per buffer; not self-delimiting).
	Vp9,
}

impl FromStr for TrackFormat {
	type Err = crate::Error;

	fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
		match s {
			"avc1" | "avcc" => Ok(TrackFormat::Avc1),
			"avc3" | "h264" => Ok(TrackFormat::Avc3),
			"hev1" => Ok(TrackFormat::Hev1),
			"av01" | "av1" | "av1c" | "av1C" => Ok(TrackFormat::Av01),
			"aac" => Ok(TrackFormat::Aac),
			"opus" => Ok(TrackFormat::Opus),
			"vp8" | "vp08" => Ok(TrackFormat::Vp8),
			"vp9" | "vp09" => Ok(TrackFormat::Vp9),
			_ => Err(crate::Error::UnknownFormat(s.to_string())),
		}
	}
}

impl fmt::Display for TrackFormat {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match *self {
			TrackFormat::Avc1 => write!(f, "avc1"),
			TrackFormat::Avc3 => write!(f, "avc3"),
			TrackFormat::Hev1 => write!(f, "hev1"),
			TrackFormat::Av01 => write!(f, "av01"),
			TrackFormat::Aac => write!(f, "aac"),
			TrackFormat::Opus => write!(f, "opus"),
			TrackFormat::Vp8 => write!(f, "vp8"),
			TrackFormat::Vp9 => write!(f, "vp9"),
		}
	}
}

/// A single-codec format whose frame boundaries can be inferred from a raw byte
/// stream (piped Annex-B H.264, an fMP4 reader, …).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum TrackStreamFormat {
	/// aka H264 with inline SPS/PPS
	Avc3,
	/// aka H265 with inline SPS/PPS
	Hev1,
	/// AV1 with inline sequence headers
	Av01,
}

impl FromStr for TrackStreamFormat {
	type Err = crate::Error;

	fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
		match s {
			"avc3" | "h264" => Ok(TrackStreamFormat::Avc3),
			"hev1" => Ok(TrackStreamFormat::Hev1),
			"av01" | "av1" | "av1c" | "av1C" => Ok(TrackStreamFormat::Av01),
			_ => Err(crate::Error::UnknownFormat(s.to_string())),
		}
	}
}

impl fmt::Display for TrackStreamFormat {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match *self {
			TrackStreamFormat::Avc3 => write!(f, "avc3"),
			TrackStreamFormat::Hev1 => write!(f, "hev1"),
			TrackStreamFormat::Av01 => write!(f, "av01"),
		}
	}
}

impl From<TrackStreamFormat> for TrackFormat {
	fn from(format: TrackStreamFormat) -> Self {
		match format {
			TrackStreamFormat::Avc3 => TrackFormat::Avc3,
			TrackStreamFormat::Hev1 => TrackFormat::Hev1,
			TrackStreamFormat::Av01 => TrackFormat::Av01,
		}
	}
}

/// A container that may publish more than one track.
///
/// Every container currently supports both framed ([`Container`](super::Container))
/// and stream ([`ContainerStream`](super::ContainerStream)) decoding, so there is
/// no separate stream-format enum; that will change once a non-streamable
/// container (e.g. RTP) is added.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ContainerFormat {
	/// fMP4/CMAF container.
	Fmp4,
	/// Matroska / WebM container.
	Mkv,
	/// MPEG-TS (transport stream) container.
	Ts,
	/// FLV (Flash Video / RTMP) container.
	Flv,
}

impl FromStr for ContainerFormat {
	type Err = crate::Error;

	fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
		match s {
			"fmp4" | "cmaf" => Ok(ContainerFormat::Fmp4),
			"mkv" | "webm" | "matroska" => Ok(ContainerFormat::Mkv),
			"ts" | "mpegts" | "mpeg2ts" | "m2ts" => Ok(ContainerFormat::Ts),
			"flv" => Ok(ContainerFormat::Flv),
			_ => Err(crate::Error::UnknownFormat(s.to_string())),
		}
	}
}

impl fmt::Display for ContainerFormat {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match *self {
			ContainerFormat::Fmp4 => write!(f, "fmp4"),
			ContainerFormat::Mkv => write!(f, "mkv"),
			ContainerFormat::Ts => write!(f, "ts"),
			ContainerFormat::Flv => write!(f, "flv"),
		}
	}
}
