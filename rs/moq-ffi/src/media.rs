use std::collections::HashMap;

#[derive(Clone, uniffi::Record)]
pub struct MoqDimensions {
	pub width: u32,
	pub height: u32,
}

#[derive(Clone, uniffi::Enum)]
pub enum Container {
	Legacy,
	Cmaf { init: Vec<u8> },
	Loc,
}

impl From<hang::catalog::Container> for Container {
	fn from(container: hang::catalog::Container) -> Self {
		match container {
			hang::catalog::Container::Legacy => Self::Legacy,
			hang::catalog::Container::Cmaf { init, .. } => Self::Cmaf { init: init.to_vec() },
			hang::catalog::Container::Loc => Self::Loc,
		}
	}
}

impl From<Container> for hang::catalog::Container {
	fn from(container: Container) -> Self {
		match container {
			Container::Legacy => Self::Legacy,
			Container::Cmaf { init } => Self::Cmaf { init: init.into() },
			Container::Loc => Self::Loc,
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
	pub container: Container,
}

#[derive(Clone, uniffi::Record)]
pub struct MoqAudio {
	pub codec: String,
	pub description: Option<Vec<u8>>,
	pub sample_rate: u32,
	pub channel_count: u32,
	pub bitrate: Option<u64>,
	pub container: Container,
}

/// A media frame.
#[derive(uniffi::Record)]
pub struct MoqFrame {
	pub payload: Vec<u8>,
	pub timestamp_us: u64,
	pub keyframe: bool,
}

/// Caller-provided audio catalog fields for [`MoqInit`].
///
/// Every field is optional and authoritative: the importer fills the rest from the encoded stream
/// and errors if a value it detects contradicts one set here. The codec comes from the [`MoqInit`]
/// format (and the init bytes); providing the sample rate and channel count lets the catalog publish
/// before the first frame.
#[derive(Clone, Default, uniffi::Record)]
pub struct MoqAudioHint {
	pub sample_rate: Option<u32>,
	pub channel_count: Option<u32>,
	pub bitrate: Option<u64>,
}

/// Caller-provided video catalog fields for [`MoqInit`].
///
/// The video counterpart of [`MoqAudioHint`]; see it for the fill-and-validate semantics.
#[derive(Clone, Default, uniffi::Record)]
pub struct MoqVideoHint {
	pub coded: Option<MoqDimensions>,
	pub display_aspect: Option<MoqDimensions>,
	pub bitrate: Option<u64>,
	pub framerate: Option<f64>,
	pub optimize_for_latency: Option<bool>,
}

/// What a single-track media publish needs: a format, its init bytes, and optional catalog fields.
///
/// `format` selects the codec (e.g. `"opus"`, `"avc3"`); `data` carries the codec init bytes (an
/// OpusHead, an avcC, ...) or is empty. The [`audio`](Self::audio) / [`video`](Self::video) hints
/// pin catalog fields the stream can't reveal (bitrate) or publish the catalog before the first
/// frame. See [`MoqBroadcastProducer::publish_media`](crate::producer::MoqBroadcastProducer::publish_media).
#[derive(Clone, uniffi::Record)]
pub struct MoqInit {
	pub format: String,
	pub data: Vec<u8>,
	pub audio: Option<MoqAudioHint>,
	pub video: Option<MoqVideoHint>,
}

impl From<MoqAudioHint> for moq_mux::catalog::AudioHint {
	fn from(hint: MoqAudioHint) -> Self {
		let mut out = moq_mux::catalog::AudioHint::default();
		out.sample_rate = hint.sample_rate;
		out.channel_count = hint.channel_count;
		out.bitrate = hint.bitrate;
		out
	}
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
		out.audio = init.audio.map(Into::into);
		out.video = init.video.map(Into::into);
		out
	}
}

pub(crate) fn convert_catalog(catalog: &moq_mux::catalog::hang::Catalog<moq_mux::catalog::hang::Extra>) -> MoqCatalog {
	let video = catalog
		.video
		.renditions
		.iter()
		.map(|(name, config)| {
			(
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
					container: config.container.clone().into(),
				},
			)
		})
		.collect();

	let audio = catalog
		.audio
		.renditions
		.iter()
		.map(|(name, config)| {
			(
				name.clone(),
				MoqAudio {
					codec: config.codec.to_string(),
					description: config.description.as_ref().map(|d| d.to_vec()),
					sample_rate: config.sample_rate,
					channel_count: config.channel_count,
					bitrate: config.bitrate,
					container: config.container.clone().into(),
				},
			)
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
