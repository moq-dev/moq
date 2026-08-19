use std::collections::HashMap;

#[derive(Clone, uniffi::Record)]
pub struct MoqDimensions {
	pub width: u32,
	pub height: u32,
}

/// Catalog properties shared by every video rendition.
///
/// Passing an absent field clears it from the next catalog snapshot rather than preserving the previous value.
#[derive(Clone, Default, uniffi::Record)]
pub struct MoqVideoProperties {
	/// Final rendered size after rotation, or absent to clear the explicit display size.
	#[uniffi(default = None)]
	pub display: Option<MoqDimensions>,

	/// Clockwise rotation in degrees, or absent to clear the explicit rotation.
	#[uniffi(default = None)]
	pub rotation: Option<f64>,

	/// Whether to flip horizontally after rotation, or absent to clear the explicit value.
	#[uniffi(default = None)]
	pub flip: Option<bool>,
}

/// How a track's frames are packaged, as advertised in the catalog.
#[derive(Clone, uniffi::Enum)]
pub enum MoqContainer {
	/// The legacy hang container.
	Legacy,
	/// CMAF (fMP4), carrying the initialization segment.
	Cmaf { init: Vec<u8> },
	/// LOC, the low-overhead container.
	Loc,
}

impl MoqContainer {
	/// Convert a catalog container, or `None` if its `kind` is not recognized.
	///
	/// A rendition we can't parse is dropped from the catalog we hand to bindings, per the
	/// hang spec: a consumer must ignore a rendition whose container it doesn't recognize.
	fn from_catalog(container: &hang::catalog::Container) -> Option<Self> {
		match container {
			hang::catalog::Container::Legacy => Some(Self::Legacy),
			hang::catalog::Container::Cmaf { init, .. } => Some(Self::Cmaf { init: init.to_vec() }),
			hang::catalog::Container::Loc => Some(Self::Loc),
			hang::catalog::Container::Unknown(unknown) => {
				tracing::warn!(kind = unknown.kind(), "ignoring unknown container");
				None
			}
		}
	}
}

impl From<MoqContainer> for hang::catalog::Container {
	fn from(container: MoqContainer) -> Self {
		match container {
			MoqContainer::Legacy => Self::Legacy,
			MoqContainer::Cmaf { init } => Self::Cmaf { init: init.into() },
			MoqContainer::Loc => Self::Loc,
		}
	}
}

#[derive(uniffi::Record)]
pub struct MoqCatalog {
	pub video: HashMap<String, MoqVideo>,
	pub audio: HashMap<String, MoqAudio>,
	pub display: Option<MoqDimensions>,
	pub rotation: Option<f64>,
	pub flip: Option<bool>,
	/// Untyped application catalog sections, keyed by section name, each value a JSON string.
	/// These are the top-level catalog keys beyond `video`/`audio`, carried through verbatim
	/// (parse the JSON yourself). Set them on the publish side with
	/// [`set_catalog_section`](crate::producer::MoqBroadcastProducer::set_catalog_section).
	pub sections: HashMap<String, String>,
}

#[derive(Clone, uniffi::Record)]
pub struct MoqVideo {
	/// Human-readable rendition name for track pickers.
	#[uniffi(default = None)]
	pub label: Option<String>,
	/// The broadcast serving this rendition's track, relative to the catalog's own broadcast
	/// (e.g. `./source`). Absent or empty means the catalog's broadcast.
	#[uniffi(default = None)]
	pub broadcast: Option<String>,
	pub codec: String,
	pub description: Option<Vec<u8>>,
	pub coded: Option<MoqDimensions>,
	pub display_aspect: Option<MoqDimensions>,
	pub bitrate: Option<u64>,
	/// Whether the publisher recommends temporarily avoiding this rendition.
	#[uniffi(default = false)]
	pub stalled: bool,
	pub framerate: Option<f64>,
	pub container: MoqContainer,
}

#[derive(Clone, uniffi::Record)]
pub struct MoqAudio {
	/// Human-readable rendition name for track pickers.
	#[uniffi(default = None)]
	pub label: Option<String>,
	/// The broadcast serving this rendition's track, relative to the catalog's own broadcast
	/// (e.g. `./source`). Absent or empty means the catalog's broadcast.
	#[uniffi(default = None)]
	pub broadcast: Option<String>,
	pub codec: String,
	pub description: Option<Vec<u8>>,
	pub sample_rate: u32,
	pub channel_count: u32,
	pub bitrate: Option<u64>,
	pub container: MoqContainer,
}

/// A payload and the time it should be presented.
///
/// The unit of both writing and raw reading: every producer write takes one of these, and a
/// raw (non-media) read returns one. Media reads return a [`MoqMediaFrame`] instead, which
/// adds the codec-derived keyframe flag.
#[derive(Clone, uniffi::Record)]
pub struct MoqFrame {
	/// The frame payload.
	pub payload: Vec<u8>,
	/// Presentation timestamp in microseconds.
	#[uniffi(default = 0)]
	pub timestamp_us: u64,
}

/// A [`MoqFrame`] plus the codec metadata a media track carries.
#[derive(Clone, uniffi::Record)]
pub struct MoqMediaFrame {
	/// The frame payload.
	pub payload: Vec<u8>,
	/// Presentation timestamp in microseconds.
	pub timestamp_us: u64,
	/// Whether this frame can be decoded without any earlier frame.
	pub keyframe: bool,
}

/// A best-effort raw track datagram, as received.
///
/// Send one with [`append_datagram`](crate::producer::MoqTrackProducer::append_datagram), which
/// takes a [`MoqFrame`] and assigns the sequence number for you.
#[derive(uniffi::Record)]
pub struct MoqDatagram {
	/// Per-track sequence number, shared with groups.
	#[uniffi(default = 0)]
	pub sequence: u64,
	/// Presentation timestamp in microseconds.
	#[uniffi(default = 0)]
	pub timestamp_us: u64,
	/// Datagram payload, capped at 1200 bytes.
	pub payload: Vec<u8>,
}

/// Caller-provided video catalog fields for [`MoqVideoInit`].
///
/// Every field is optional and fills only a gap the stream leaves; a value the stream detects wins.
/// Publishing the catalog before the first keyframe needs at least the codec, which comes from the
/// [`MoqVideoInit`] format. Audio has no equivalent: it resolves entirely from its init bytes.
#[derive(Clone, Default, uniffi::Record)]
pub struct MoqVideoHint {
	/// The encoded pixel dimensions.
	pub coded: Option<MoqDimensions>,
	/// The display aspect ratio.
	pub display_aspect: Option<MoqDimensions>,
	/// The maximum bitrate in bits per second.
	pub bitrate: Option<u64>,
	/// The frame rate in frames per second.
	pub framerate: Option<f64>,
	/// Whether the decoder should optimize for latency.
	pub optimize_for_latency: Option<bool>,
}

/// A single audio codec an importer can parse.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum MoqAudioFormat {
	/// Advanced Audio Coding, configured by an AudioSpecificConfig.
	Aac,
	/// Opus, configured by an OpusHead.
	Opus,
	/// FLAC, configured by the `fLaC` marker plus its STREAMINFO block.
	Flac,
	/// MPEG-1/2 Audio Layer III.
	Mp3,
}

/// A single video codec an importer can parse.
///
/// H.264 and H.265 appear twice each because the framing differs, not just the codec: `Avc1`/`Hvc1`
/// are length-prefixed with an out-of-band config record, while `Avc3`/`Hev1` are Annex-B with the
/// parameter sets inline.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum MoqVideoFormat {
	/// H.264, length-prefixed NALUs with an out-of-band avcC.
	Avc1,
	/// H.264, Annex-B with inline SPS/PPS.
	Avc3,
	/// H.265, length-prefixed NALUs with an out-of-band hvcC.
	Hvc1,
	/// H.265, Annex-B with inline parameter sets.
	Hev1,
	/// AV1.
	Av01,
	/// VP8.
	Vp8,
	/// VP9.
	Vp9,
}

/// A container that publishes its own tracks, which may be more than one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum MoqContainerFormat {
	/// Fragmented MP4 / CMAF.
	Fmp4,
	/// Matroska / WebM.
	Mkv,
	/// MPEG-2 transport stream.
	Ts,
	/// Flash Video, as used by RTMP.
	Flv,
}

/// What an audio publish needs: a format, its codec init bytes, and an optional label.
///
/// `data` is required: an audio importer cannot resolve its config from frames, so it needs the
/// OpusHead / AudioSpecificConfig / STREAMINFO up front.
#[derive(Clone, uniffi::Record)]
pub struct MoqAudioInit {
	/// The audio codec.
	pub format: MoqAudioFormat,
	/// Codec init bytes. Required: audio has no in-band config.
	pub data: Vec<u8>,
	/// Human-readable rendition name for a track picker.
	#[uniffi(default = None)]
	pub label: Option<String>,
}

/// What a video publish needs: a format, optional init bytes, a label, and hints.
///
/// `data` may be empty for a format that resolves in band. A [`hint`](Self::hint) pins catalog
/// fields the stream never reveals (bitrate) or publishes the catalog before the first keyframe.
#[derive(Clone, uniffi::Record)]
pub struct MoqVideoInit {
	/// The video codec.
	pub format: MoqVideoFormat,
	/// Codec init bytes (an avcC, an hvcC, ...). May be empty for a format that resolves in band.
	pub data: Vec<u8>,
	/// Human-readable rendition name for a track picker.
	#[uniffi(default = None)]
	pub label: Option<String>,
	/// Catalog fields the stream cannot reveal itself.
	#[uniffi(default = None)]
	pub hint: Option<MoqVideoHint>,
}

/// What a container publish needs: a format and its leading bytes.
///
/// A container publishes and describes its own tracks, so there is no label or hint here: a
/// rendition field would have no single track to land on.
#[derive(Clone, uniffi::Record)]
pub struct MoqContainerInit {
	/// The container format.
	pub format: MoqContainerFormat,
	/// The leading chunk of the container, decoded immediately. May be empty.
	pub data: Vec<u8>,
}

impl From<MoqVideoHint> for moq_mux::catalog::VideoHint {
	fn from(hint: MoqVideoHint) -> Self {
		let mut out = moq_mux::catalog::VideoHint::default();
		out.coded_width = hint.coded.as_ref().map(|d| d.width);
		out.coded_height = hint.coded.as_ref().map(|d| d.height);
		out.display_aspect_width = hint.display_aspect.as_ref().map(|d| d.width);
		out.display_aspect_height = hint.display_aspect.as_ref().map(|d| d.height);
		out.bitrate = hint.bitrate;
		out.framerate = hint.framerate;
		out.optimize_for_latency = hint.optimize_for_latency;
		out
	}
}

impl From<MoqAudioFormat> for moq_mux::import::AudioFormat {
	fn from(format: MoqAudioFormat) -> Self {
		match format {
			MoqAudioFormat::Aac => Self::Aac,
			MoqAudioFormat::Opus => Self::Opus,
			MoqAudioFormat::Flac => Self::Flac,
			MoqAudioFormat::Mp3 => Self::Mp3,
		}
	}
}

impl From<MoqVideoFormat> for moq_mux::import::VideoFormat {
	fn from(format: MoqVideoFormat) -> Self {
		match format {
			MoqVideoFormat::Avc1 => Self::Avc1,
			MoqVideoFormat::Avc3 => Self::Avc3,
			MoqVideoFormat::Hvc1 => Self::Hvc1,
			MoqVideoFormat::Hev1 => Self::Hev1,
			MoqVideoFormat::Av01 => Self::Av01,
			MoqVideoFormat::Vp8 => Self::Vp8,
			MoqVideoFormat::Vp9 => Self::Vp9,
		}
	}
}

impl From<MoqContainerFormat> for moq_mux::import::ContainerFormat {
	fn from(format: MoqContainerFormat) -> Self {
		match format {
			MoqContainerFormat::Fmp4 => Self::Fmp4,
			MoqContainerFormat::Mkv => Self::Mkv,
			MoqContainerFormat::Ts => Self::Ts,
			MoqContainerFormat::Flv => Self::Flv,
		}
	}
}

impl From<MoqAudioInit> for moq_mux::import::AudioInit {
	fn from(init: MoqAudioInit) -> Self {
		let mut out = moq_mux::import::AudioInit::new(init.format.into(), init.data);
		out.label = init.label;
		out
	}
}

impl From<MoqVideoInit> for moq_mux::import::VideoInit {
	fn from(init: MoqVideoInit) -> Self {
		let mut out = moq_mux::import::VideoInit::new(init.format.into(), init.data);
		out.label = init.label;
		if let Some(hint) = init.hint {
			out.hint = hint.into();
		}
		out
	}
}

impl From<MoqContainerInit> for moq_mux::import::ContainerInit {
	fn from(init: MoqContainerInit) -> Self {
		moq_mux::import::ContainerInit::new(init.format.into(), init.data)
	}
}

pub(crate) fn convert_catalog(catalog: &moq_mux::catalog::hang::Catalog<moq_mux::catalog::hang::Extra>) -> MoqCatalog {
	let video = catalog
		.video
		.renditions
		.iter()
		.filter_map(|(name, config)| {
			Some((
				name.clone(),
				MoqVideo {
					label: config.label.clone(),
					broadcast: config.broadcast.as_ref().map(|path| path.as_str().to_string()),
					codec: config.codec.to_string(),
					description: config.description.as_ref().map(|d| d.to_vec()),
					coded: match (config.coded_width, config.coded_height) {
						(Some(w), Some(h)) => Some(MoqDimensions { width: w, height: h }),
						_ => None,
					},
					display_aspect: match (config.display_aspect_width, config.display_aspect_height) {
						(Some(w), Some(h)) => Some(MoqDimensions { width: w, height: h }),
						_ => None,
					},
					bitrate: config.bitrate,
					stalled: config.stalled.unwrap_or(false),
					framerate: config.framerate,
					container: MoqContainer::from_catalog(&config.container)?,
				},
			))
		})
		.collect();

	let audio = catalog
		.audio
		.renditions
		.iter()
		.filter_map(|(name, config)| {
			Some((
				name.clone(),
				MoqAudio {
					label: config.label.clone(),
					broadcast: config.broadcast.as_ref().map(|path| path.as_str().to_string()),
					codec: config.codec.to_string(),
					description: config.description.as_ref().map(|d| d.to_vec()),
					sample_rate: config.sample_rate,
					channel_count: config.channel_count,
					bitrate: config.bitrate,
					container: MoqContainer::from_catalog(&config.container)?,
				},
			))
		})
		.collect();

	let display = catalog.video.display.as_ref().map(|d| MoqDimensions {
		width: d.width,
		height: d.height,
	});

	let sections = catalog
		.sections()
		.map(|(name, value)| (name.clone(), value.to_string()))
		.collect();

	MoqCatalog {
		video,
		audio,
		display,
		rotation: catalog.video.rotation,
		flip: catalog.video.flip,
		sections,
	}
}

#[cfg(test)]
mod test {
	use super::*;

	#[test]
	fn catalog_exposes_stalled_renditions() {
		let mut catalog = moq_mux::catalog::hang::Catalog::default();
		let active = hang::catalog::VideoConfig::new(hang::catalog::VideoCodec::VP8);
		catalog.video.renditions.insert("active".to_string(), active);
		let mut video = hang::catalog::VideoConfig::new(hang::catalog::VideoCodec::VP8);
		video.stalled = Some(true);
		catalog.video.renditions.insert("video".to_string(), video);

		let converted = convert_catalog(&catalog);
		assert!(!converted.video["active"].stalled);
		assert!(converted.video["video"].stalled);
	}

	/// A rendition may name a sibling broadcast, and the track then lives there. Dropping the
	/// reference here loses it for every binding, which then opens the track on the wrong
	/// broadcast: `NotFound`, or a same-named local track with mismatched metadata.
	#[test]
	fn catalog_exposes_rendition_broadcast_references() {
		let mut catalog = moq_mux::catalog::hang::Catalog::default();

		let mut video = hang::catalog::VideoConfig::new(hang::catalog::VideoCodec::VP8);
		video.broadcast = Some(moq_net::PathRelative::new("./source").into_owned());
		catalog.video.renditions.insert("video".to_string(), video);
		let local = hang::catalog::VideoConfig::new(hang::catalog::VideoCodec::VP8);
		catalog.video.renditions.insert("local".to_string(), local);

		let mut audio = hang::catalog::AudioConfig::new(hang::catalog::AudioCodec::Opus, 48_000, 2);
		audio.broadcast = Some(moq_net::PathRelative::new("../elsewhere").into_owned());
		catalog.audio.renditions.insert("audio".to_string(), audio);

		let converted = convert_catalog(&catalog);
		// `PathRelative` strips redundant `.` segments on creation, so the reference crosses as
		// the normalized form the catalog itself holds. It round-trips: rebuilding a
		// `PathRelative` from it is a no-op.
		assert_eq!(converted.video["video"].broadcast.as_deref(), Some("source"));
		assert_eq!(converted.video["local"].broadcast, None);
		assert_eq!(converted.audio["audio"].broadcast.as_deref(), Some("../elsewhere"));
	}

	#[test]
	fn catalog_exposes_rendition_labels() {
		let mut catalog = moq_mux::catalog::hang::Catalog::default();
		let mut video = hang::catalog::VideoConfig::new(hang::catalog::VideoCodec::VP8);
		video.label = Some("Main camera".to_string());
		catalog.video.renditions.insert("video".to_string(), video);

		let mut audio = hang::catalog::AudioConfig::new(hang::catalog::AudioCodec::Opus, 48_000, 2);
		audio.label = Some("English".to_string());
		catalog.audio.renditions.insert("audio".to_string(), audio);

		let converted = convert_catalog(&catalog);
		assert_eq!(converted.video["video"].label.as_deref(), Some("Main camera"));
		assert_eq!(converted.audio["audio"].label.as_deref(), Some("English"));
	}
}
