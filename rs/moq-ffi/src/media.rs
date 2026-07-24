use std::collections::HashMap;

#[derive(Clone, uniffi::Record)]
pub struct MoqDimensions {
	pub width: u32,
	pub height: u32,
}

/// Video presentation metadata applied to all video renditions in the catalog.
///
/// Every generated-language constructor requires all three fields.
/// Passing an absent field clears it from the next catalog snapshot rather than preserving the previous value.
#[derive(Clone, Default, uniffi::Record)]
pub struct MoqVideoPresentation {
	/// Final rendered size after rotation, or absent to clear the explicit display size.
	pub display: Option<MoqDimensions>,

	/// Clockwise rotation in degrees, or absent to clear the explicit rotation.
	pub rotation: Option<f64>,

	/// Whether to flip horizontally after rotation, or absent to clear the explicit value.
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
	pub codec: String,
	pub description: Option<Vec<u8>>,
	pub coded: Option<MoqDimensions>,
	pub display_aspect: Option<MoqDimensions>,
	pub bitrate: Option<u64>,
	pub framerate: Option<f64>,
	pub container: MoqContainer,
}

#[derive(Clone, uniffi::Record)]
pub struct MoqAudio {
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

/// Caller-provided video catalog fields for [`MoqInit`].
///
/// Every field is optional and fills only a gap the stream leaves; a value the stream detects wins.
/// Publishing the catalog before the first keyframe needs at least the codec, which comes from the
/// [`MoqInit`] format. Audio has no equivalent: an audio format resolves entirely from its init bytes.
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

/// What a single-track media publish needs: a format, its init bytes, and optional video fields.
///
/// `format` selects the codec (e.g. `"opus"`, `"avc3"`); `data` carries the codec init bytes (an
/// OpusHead, an avcC, an AudioSpecificConfig, ...). Audio formats need those bytes up front; video
/// formats may resolve in band, and a [`video`](Self::video) hint pins catalog fields the stream
/// can't reveal (bitrate) or publishes the catalog before the first keyframe. See
/// [`MoqBroadcastProducer::publish_media`](crate::producer::MoqBroadcastProducer::publish_media).
#[derive(Clone, uniffi::Record)]
pub struct MoqInit {
	/// The media format, e.g. `"opus"`, `"avc3"`, or `"aac"`.
	pub format: String,
	/// Codec init bytes. Required for audio; may be empty for a video format that resolves in band.
	pub data: Vec<u8>,
	/// Caller-provided fields for a video track.
	pub video: Option<MoqVideoHint>,
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

impl From<MoqInit> for moq_mux::import::Init {
	fn from(init: MoqInit) -> Self {
		let mut out = moq_mux::import::Init::new(init.format, init.data);
		out.video = init.video.map(Into::into);
		out
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
