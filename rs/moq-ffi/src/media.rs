use std::collections::HashMap;

#[derive(uniffi::Record)]
pub struct MoqDimensions {
	pub width: u32,
	pub height: u32,
}

#[derive(uniffi::Enum)]
pub enum Container {
	Legacy,
	Cmaf { init_data: String },
}

impl From<hang::catalog::Container> for Container {
	fn from(container: hang::catalog::Container) -> Self {
		match container {
			hang::catalog::Container::Legacy => Self::Legacy,
			hang::catalog::Container::Cmaf { init_data } => Self::Cmaf { init_data },
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
}

#[derive(uniffi::Record)]
pub struct MoqVideo {
	pub codec: String,
	pub description: Option<Vec<u8>>,
	pub coded: Option<MoqDimensions>,
	pub display_ratio: Option<MoqDimensions>,
	pub bitrate: Option<u64>,
	pub framerate: Option<f64>,
	pub container: Container,
}

#[derive(uniffi::Record)]
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

pub fn convert_catalog(catalog: &hang::catalog::Catalog) -> MoqCatalog {
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
					display_ratio: match (config.display_ratio_width, config.display_ratio_height) {
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

	MoqCatalog {
		video,
		audio,
		display,
		rotation: catalog.video.rotation,
		flip: catalog.video.flip,
	}
}
