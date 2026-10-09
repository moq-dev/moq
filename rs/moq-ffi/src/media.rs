use std::collections::HashMap;
use std::sync::{Arc, Weak};

use bytes::Buf;
use moq_mux::catalog::hang::Extra;

use crate::consumer::{MoqBroadcastConsumer, MoqFetchGroupOptions, MoqSubscription, map_fetch_error, timestamp_us};
use crate::demand::MoqTrackDemand;
use crate::error::MoqError;
use crate::ffi::Task;
use crate::producer::{MoqBroadcastProducer, MoqTrackRequest};

/// A width and height in pixels.
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
	/// [`set_section`](MoqMediaCatalogProducer::set_section).
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
	/// Whether this rendition may be selected; when false, no frames are coming.
	#[uniffi(default = true)]
	pub enabled: bool,
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
	/// Whether this rendition may be selected; when false, no frames are coming.
	#[uniffi(default = true)]
	pub enabled: bool,
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
	/// Presentation timestamp in microseconds, or null on a frame read from an untimed
	/// track. A raw track published here is timed, so writing one needs it.
	#[uniffi(default = None)]
	pub timestamp_us: Option<u64>,
}

/// A [`MoqFrame`] plus the codec metadata a media track carries.
#[derive(Clone, uniffi::Record)]
pub struct MoqMediaFrame {
	/// The frame payload.
	pub payload: Vec<u8>,
	/// Presentation timestamp in microseconds.
	pub timestamp_us: u64,
	/// Whether this frame opens a group or is a video keyframe; audio is true only at a group start.
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
	/// Presentation timestamp in microseconds, or null on a datagram read from an untimed
	/// track. A raw track published here is timed, so appending one needs it.
	#[uniffi(default = None)]
	pub timestamp_us: Option<u64>,
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
	#[uniffi(default = None)]
	pub coded: Option<MoqDimensions>,
	/// The display aspect ratio.
	#[uniffi(default = None)]
	pub display_aspect: Option<MoqDimensions>,
	/// The maximum bitrate in bits per second.
	#[uniffi(default = None)]
	pub bitrate: Option<u64>,
	/// The frame rate in frames per second.
	#[uniffi(default = None)]
	pub framerate: Option<f64>,
	/// Whether the decoder should optimize for latency.
	#[uniffi(default = None)]
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
					enabled: config.enabled,
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
					enabled: config.enabled,
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
		.ext
		.iter()
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
	fn catalog_exposes_disabled_renditions() {
		let mut catalog = moq_mux::catalog::hang::Catalog::default();
		let active = hang::catalog::VideoConfig::new(hang::catalog::VideoCodec::VP8);
		catalog.video.renditions.insert("active".to_string(), active);
		let mut video = hang::catalog::VideoConfig::new(hang::catalog::VideoCodec::VP8);
		video.enabled = false;
		catalog.video.renditions.insert("video".to_string(), video);
		let mut audio = hang::catalog::AudioConfig::new(hang::catalog::AudioCodec::Opus, 48_000, 2);
		audio.enabled = false;
		catalog.audio.renditions.insert("audio".to_string(), audio);

		let converted = convert_catalog(&catalog);
		assert!(converted.video["active"].enabled);
		assert!(!converted.video["video"].enabled);
		assert!(!converted.audio["audio"].enabled);
	}

	/// A rendition may name a sibling broadcast, and the track then lives there. Dropping the
	/// reference here loses it for every binding, which then opens the track on the wrong
	/// broadcast: `NotFound`, or a same-named local track with mismatched metadata.
	#[test]
	fn catalog_exposes_rendition_broadcast_references() {
		let mut catalog = moq_mux::catalog::hang::Catalog::default();

		let mut video = hang::catalog::VideoConfig::new(hang::catalog::VideoCodec::VP8);
		video.broadcast = Some(moq_net::path::Relative::new("./source").into_owned());
		catalog.video.renditions.insert("video".to_string(), video);
		let local = hang::catalog::VideoConfig::new(hang::catalog::VideoCodec::VP8);
		catalog.video.renditions.insert("local".to_string(), local);

		let mut audio = hang::catalog::AudioConfig::new(hang::catalog::AudioCodec::Opus, 48_000, 2);
		audio.broadcast = Some(moq_net::path::Relative::new("../elsewhere").into_owned());
		catalog.audio.renditions.insert("audio".to_string(), audio);

		let converted = convert_catalog(&catalog);
		// `path::Relative` strips redundant `.` segments on creation, so the reference crosses as
		// the normalized form the catalog itself holds. It round-trips: rebuilding a
		// `path::Relative` from it is a no-op.
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

/// Where an importer publishes its single track.
#[derive(Clone, uniffi::Enum)]
pub enum MoqMediaTarget {
	/// A new track with an explicit name, or a unique name derived from its format.
	Named { name: Option<String> },
	/// A requested track whose name and ownership are already fixed.
	Requested { request: Arc<MoqTrackRequest> },
}

impl MoqMediaTarget {
	fn resolve(
		self,
		broadcast: &moq_net::broadcast::Producer,
		format: &impl std::fmt::Display,
	) -> Result<moq_net::track::Request, MoqError> {
		match self {
			Self::Named { name } => reserve_track(broadcast, name, format),
			Self::Requested { request } => request.take(),
		}
	}
}

/// Updates a broadcast's catalog without keeping the broadcast open.
#[derive(uniffi::Object)]
pub struct MoqMediaCatalogProducer {
	broadcast: Weak<MoqBroadcastProducer>,
}

#[uniffi::export]
impl MoqMediaCatalogProducer {
	/// Construct a weak catalog handle for this broadcast.
	#[uniffi::constructor]
	pub fn new(broadcast: Arc<MoqBroadcastProducer>) -> Result<Arc<Self>, MoqError> {
		let _guard = crate::ffi::enter();
		broadcast.with_state(|_| Ok(()))?;
		Ok(Arc::new(Self {
			broadcast: Arc::downgrade(&broadcast),
		}))
	}

	/// Replace shared video properties, clearing fields that are absent.
	///
	/// Rotation is clockwise and normalized to the nearest quarter turn.
	pub fn set_video_properties(&self, properties: MoqVideoProperties) -> Result<(), MoqError> {
		let _guard = crate::ffi::enter();
		let mut value = hang::catalog::VideoProperties::default();
		value.display = properties.display.map(|display| hang::catalog::Display {
			width: display.width,
			height: display.height,
		});
		value.rotation = properties.rotation;
		value.flip = properties.flip;

		self.broadcast.upgrade().ok_or(MoqError::Closed)?.with_state(|state| {
			let mut catalog = state.catalog.modify()?;
			catalog.video.set_properties(value)?;
			catalog.commit()?;
			Ok(())
		})
	}

	/// Set an application catalog section from JSON, refusing reserved section names.
	///
	/// `json` is any JSON document. Errors with [`MoqError::Json`] if it doesn't parse, or with
	/// the reserved-section error if `name` is a HANG root (`video`, `audio`, `text`, `archive`, `clock`, `json`, `binary`, or
	/// retired `timeline`) or an MSF root (`version`, `generatedAt`, `isComplete`, `tracks`, or
	/// `initDataList`).
	pub fn set_section(&self, name: String, json: String) -> Result<(), MoqError> {
		let _guard = crate::ffi::enter();
		let value: serde_json::Value = serde_json::from_str(&json)?;
		self.broadcast.upgrade().ok_or(MoqError::Closed)?.with_state(|state| {
			let mut guard = state.catalog.modify()?;
			guard.set_section(name, value)?;
			guard.commit()?;
			Ok(())
		})
	}

	/// Remove an application catalog section if present.
	pub fn remove_section(&self, name: String) -> Result<(), MoqError> {
		let _guard = crate::ffi::enter();
		self.broadcast.upgrade().ok_or(MoqError::Closed)?.with_state(|state| {
			let mut guard = state.catalog.modify()?;
			guard.remove_section(&name);
			guard.commit()?;
			Ok(())
		})
	}
}

/// A whole-frame importer for one codec track.
///
/// Separate from [`ContainerProducer`] because a container publishes several tracks and takes
/// chunks rather than timestamped frames. Sharing one type meant a `write_frame` timestamp that one
/// arm silently dropped.
struct MediaProducer {
	// Boxed because the codec splitters/imports make this much larger than the container one.
	import: Box<moq_mux::import::Track>,
	/// Subscriber demand (name/used/unused) for the one track this publishes.
	demand: moq_net::track::Demand,
}

/// A whole-chunk importer for a container, which may publish several tracks.
struct ContainerProducer {
	import: moq_mux::import::Container<Extra>,
}

/// A byte-stream importer for one codec track, where frame boundaries are inferred.
struct MediaStreamProducer {
	import: Box<moq_mux::import::TrackStream>,
}

/// A byte-stream importer for a container, which recovers its own framing.
struct ContainerStreamProducer {
	import: moq_mux::import::ContainerStream<Extra>,
}

/// Imports complete container chunks into a broadcast.
#[derive(uniffi::Object)]
pub struct MoqMediaContainerProducer {
	inner: std::sync::Mutex<Option<ContainerProducer>>,
}

/// Imports a container byte stream into a broadcast.
#[derive(uniffi::Object)]
pub struct MoqMediaContainerStreamProducer {
	inner: std::sync::Mutex<Option<ContainerStreamProducer>>,
}

/// Imports complete encoded frames onto one track.
#[derive(uniffi::Object)]
pub struct MoqMediaTrackProducer {
	inner: std::sync::Mutex<Option<MediaProducer>>,
}

/// Imports a video byte stream onto one track.
#[derive(uniffi::Object)]
pub struct MoqMediaTrackStreamProducer {
	inner: std::sync::Mutex<Option<MediaStreamProducer>>,
}

#[uniffi::export]
impl MoqMediaTrackProducer {
	/// Import complete encoded audio frames on a named or requested track.
	///
	/// [`MoqAudioInit::data`] is required: audio resolves its rendition entirely from those bytes.
	#[uniffi::constructor]
	pub fn audio(
		broadcast: &MoqBroadcastProducer,
		target: MoqMediaTarget,
		init: MoqAudioInit,
	) -> Result<Arc<MoqMediaTrackProducer>, MoqError> {
		let _guard = crate::ffi::enter();
		let guard = broadcast.state.lock().unwrap();
		let state = guard.as_ref().ok_or(MoqError::Closed)?;

		let init: moq_mux::import::AudioInit = init.into();
		let request = target.resolve(&state.broadcast, &init.format)?;

		let import = moq_mux::import::Track::audio(request, state.catalog.reserve(), init)
			.map_err(|err| MoqError::Codec(format!("init failed: {err}")))?;
		Ok(MoqMediaTrackProducer::from_import(import))
	}
}

#[uniffi::export]
impl MoqMediaTrackProducer {
	/// Import complete encoded video frames on a named or requested track.
	///
	/// [`MoqVideoInit::data`] may be empty for a format that resolves in band; a hint carrying the
	/// codec publishes the catalog before the first keyframe.
	#[uniffi::constructor]
	pub fn video(
		broadcast: &MoqBroadcastProducer,
		target: MoqMediaTarget,
		init: MoqVideoInit,
	) -> Result<Arc<MoqMediaTrackProducer>, MoqError> {
		let _guard = crate::ffi::enter();
		let guard = broadcast.state.lock().unwrap();
		let state = guard.as_ref().ok_or(MoqError::Closed)?;

		let init: moq_mux::import::VideoInit = init.into();
		let request = target.resolve(&state.broadcast, &init.format)?;

		let import = moq_mux::import::Track::video(request, state.catalog.reserve(), init)
			.map_err(|err| MoqError::Codec(format!("init failed: {err}")))?;
		Ok(MoqMediaTrackProducer::from_import(import))
	}
}

#[uniffi::export]
impl MoqMediaContainerProducer {
	/// Construct an importer from this broadcast.
	#[uniffi::constructor]
	pub fn new(
		broadcast: &MoqBroadcastProducer,
		init: MoqContainerInit,
	) -> Result<Arc<MoqMediaContainerProducer>, MoqError> {
		let _guard = crate::ffi::enter();
		let guard = broadcast.state.lock().unwrap();
		let state = guard.as_ref().ok_or(MoqError::Closed)?;

		let init: moq_mux::import::ContainerInit = init.into();
		let import = moq_mux::import::Container::new(state.broadcast.clone(), state.catalog.reserve(), &init)
			.map_err(|err| MoqError::Codec(format!("init failed: {err}")))?;

		Ok(Arc::new(MoqMediaContainerProducer {
			inner: std::sync::Mutex::new(Some(ContainerProducer { import })),
		}))
	}
}

#[uniffi::export]
impl MoqMediaTrackStreamProducer {
	/// Import a video byte stream, inferring frame boundaries.
	///
	/// Only the self-delimiting formats work here (`Avc3`, `Hev1`, `Av01`); the rest need length
	/// prefixes or an out-of-band config record. There is no audio counterpart for the same reason.
	#[uniffi::constructor]
	pub fn video(
		broadcast: &MoqBroadcastProducer,
		target: MoqMediaTarget,
		init: MoqVideoInit,
	) -> Result<Arc<MoqMediaTrackStreamProducer>, MoqError> {
		let _guard = crate::ffi::enter();
		let guard = broadcast.state.lock().unwrap();
		let state = guard.as_ref().ok_or(MoqError::Closed)?;

		let init: moq_mux::import::VideoInit = init.into();
		let request = target.resolve(&state.broadcast, &init.format)?;

		let import = moq_mux::import::TrackStream::video(request, state.catalog.reserve(), init)
			.map_err(|err| MoqError::Codec(format!("init failed: {err}")))?;

		Ok(Arc::new(MoqMediaTrackStreamProducer {
			inner: std::sync::Mutex::new(Some(MediaStreamProducer {
				import: Box::new(import),
			})),
		}))
	}
}

#[uniffi::export]
impl MoqMediaContainerStreamProducer {
	/// Construct an importer from this broadcast.
	#[uniffi::constructor]
	pub fn new(
		broadcast: &MoqBroadcastProducer,
		format: MoqContainerFormat,
	) -> Result<Arc<MoqMediaContainerStreamProducer>, MoqError> {
		let _guard = crate::ffi::enter();
		let guard = broadcast.state.lock().unwrap();
		let state = guard.as_ref().ok_or(MoqError::Closed)?;

		let import =
			moq_mux::import::ContainerStream::new(state.broadcast.clone(), state.catalog.reserve(), format.into())
				.map_err(|err| MoqError::Codec(format!("init failed: {err}")))?;

		Ok(Arc::new(MoqMediaContainerStreamProducer {
			inner: std::sync::Mutex::new(Some(ContainerStreamProducer { import })),
		}))
	}
}

impl MoqMediaTrackProducer {
	/// Wrap a single-codec importer, capturing the demand handle its track exposes.
	fn from_import(import: moq_mux::import::Track) -> Arc<Self> {
		let demand = import.demand();
		Arc::new(Self {
			inner: std::sync::Mutex::new(Some(MediaProducer {
				import: Box::new(import),
				demand,
			})),
		})
	}
}

#[uniffi::export]
impl MoqMediaTrackProducer {
	/// A watch-only handle to whether this track has subscribers.
	pub fn demand(&self) -> Result<Arc<MoqTrackDemand>, MoqError> {
		let guard = self.inner.lock().unwrap();
		Ok(MoqTrackDemand::new(
			guard.as_ref().ok_or(MoqError::Closed)?.demand.clone(),
		))
	}

	/// Write `frame` to this track.
	///
	/// The importer derives keyframe status from the bitstream, so a [`MoqFrame`] carries only the
	/// payload and its timestamp.
	pub fn write_frame(&self, frame: MoqFrame) -> Result<(), MoqError> {
		let _guard = crate::ffi::enter();
		let mut guard = self.inner.lock().unwrap();
		let media = guard.as_mut().ok_or(MoqError::Closed)?;

		let timestamp = frame
			.timestamp_us
			.map(hang::container::Timestamp::from_micros)
			.transpose()?;
		media
			.import
			.decode(&frame.payload, timestamp)
			.map_err(|err| MoqError::Codec(format!("decode failed: {err}")))?;

		Ok(())
	}

	/// Record a locally encoded frame's handoff for catalog jitter measurement.
	///
	/// `timestamp_us` is on the broadcast media clock. Call this after `write_frame` only for
	/// encoder output; imported files, pipes, and network media stay clock-free.
	pub fn flush(&self, timestamp_us: u64) -> Result<(), MoqError> {
		let _guard = crate::ffi::enter();
		let timestamp = moq_net::Timestamp::from_micros(timestamp_us)?;
		let mut guard = self.inner.lock().unwrap();
		let media = guard.as_mut().ok_or(MoqError::Closed)?;
		media.import.flush(timestamp, std::time::Instant::now())?;
		Ok(())
	}

	/// Mark a timeline break and restart handoff measurement without lowering advertised jitter.
	///
	/// Publishes a discontinuity marker; resumed frames must continue the broadcast media clock,
	/// and video must resume on a keyframe.
	pub fn discontinuity(&self) -> Result<(), MoqError> {
		let _guard = crate::ffi::enter();
		let mut guard = self.inner.lock().unwrap();
		let media = guard.as_mut().ok_or(MoqError::Closed)?;
		media.import.discontinuity()?;
		Ok(())
	}

	/// Draw a group boundary here.
	///
	/// Audio has no boundary of its own (every packet is independently decodable), so this is the
	/// only thing that gives it groups: call it after every frame for one group (one QUIC stream)
	/// the relay forwards without waiting, or at a segment cadence to align with video for
	/// HLS/DASH. Video groups at its own keyframes and needs this only to override that.
	pub fn cut(&self) -> Result<(), MoqError> {
		let _guard = crate::ffi::enter();
		let mut guard = self.inner.lock().unwrap();
		let media = guard.as_mut().ok_or(MoqError::Closed)?;
		media
			.import
			.cut(None)
			.map_err(|err| MoqError::Codec(format!("cut failed: {err}")))?;
		Ok(())
	}

	/// Draw a group boundary and number the next group `sequence`.
	///
	/// [`cut`](Self::cut) with an explicit sequence, for a publisher whose group numbers have to
	/// be deterministic: two encoders aligning per GOP so a consumer can fail over between them.
	pub fn seek(&self, sequence: u64) -> Result<(), MoqError> {
		let _guard = crate::ffi::enter();
		let mut guard = self.inner.lock().unwrap();
		let media = guard.as_mut().ok_or(MoqError::Closed)?;
		media
			.import
			.seek(sequence)
			.map_err(|err| MoqError::Codec(format!("seek failed: {err}")))?;
		Ok(())
	}

	/// Finish this track and finalize encoding.
	pub fn finish(&self) -> Result<(), MoqError> {
		let _guard = crate::ffi::enter();
		let mut guard = self.inner.lock().unwrap();
		let mut media = guard.take().ok_or(MoqError::Closed)?;
		media
			.import
			.finish()
			.map_err(|err| MoqError::Codec(format!("finish failed: {err}")))?;
		Ok(())
	}
}

#[uniffi::export]
impl MoqMediaContainerProducer {
	/// Write a whole chunk of the container.
	///
	/// No timestamp: a container carries its tracks' timing itself, and the importer reads it out
	/// rather than taking the caller's word for it.
	pub fn write(&self, payload: Vec<u8>) -> Result<(), MoqError> {
		let _guard = crate::ffi::enter();
		let mut guard = self.inner.lock().unwrap();
		let container = guard.as_mut().ok_or(MoqError::Closed)?;
		container
			.import
			.decode(&payload)
			.map_err(|err| MoqError::Codec(format!("decode failed: {err}")))?;
		Ok(())
	}

	/// Declare that the next chunk starts a new segment, rolling a group on every track this
	/// publishes.
	///
	/// For a caller that knows its source's segmentation out of band. An fMP4 source carrying
	/// `styp` atoms declares its own, so this is only needed when it doesn't, and formats with no
	/// segment concept (MKV, TS, FLV) ignore it.
	pub fn cut(&self) -> Result<(), MoqError> {
		let _guard = crate::ffi::enter();
		let mut guard = self.inner.lock().unwrap();
		let container = guard.as_mut().ok_or(MoqError::Closed)?;
		container.import.cut();
		Ok(())
	}

	/// Start a new segment and number its groups `sequence`.
	pub fn seek(&self, sequence: u64) -> Result<(), MoqError> {
		let _guard = crate::ffi::enter();
		let mut guard = self.inner.lock().unwrap();
		let container = guard.as_mut().ok_or(MoqError::Closed)?;
		container
			.import
			.seek(sequence)
			.map_err(|err| MoqError::Codec(format!("seek failed: {err}")))?;
		Ok(())
	}

	/// Finish every track this container publishes.
	pub fn finish(&self) -> Result<(), MoqError> {
		let _guard = crate::ffi::enter();
		let mut guard = self.inner.lock().unwrap();
		let mut container = guard.take().ok_or(MoqError::Closed)?;
		container
			.import
			.finish()
			.map_err(|err| MoqError::Codec(format!("finish failed: {err}")))?;
		Ok(())
	}
}

#[uniffi::export]
impl MoqMediaTrackStreamProducer {
	/// A watch-only handle to whether this track has subscribers.
	pub fn demand(&self) -> Result<Arc<MoqTrackDemand>, MoqError> {
		let guard = self.inner.lock().unwrap();
		Ok(MoqTrackDemand::new(
			guard.as_ref().ok_or(MoqError::Closed)?.import.demand(),
		))
	}

	/// Push raw stream bytes (e.g. Annex-B H.264 from an encoder). The importer frames whole access
	/// units and keeps any partial trailing frame for the next call, so callers can write arbitrary
	/// chunks.
	pub fn write(&self, payload: Vec<u8>) -> Result<(), MoqError> {
		let _guard = crate::ffi::enter();
		let mut guard = self.inner.lock().unwrap();
		let media = guard.as_mut().ok_or(MoqError::Closed)?;
		media
			.import
			.decode(&payload)
			.map_err(|err| MoqError::Codec(format!("decode failed: {err}")))?;
		Ok(())
	}

	/// Finalize the track.
	///
	/// The importer emits each access unit when the *next* one's start code arrives, so a trailing
	/// access unit with no following delimiter (e.g. the last frame at EOF) is not emitted. This
	/// matches moq-cli's stdin path.
	pub fn finish(&self) -> Result<(), MoqError> {
		let _guard = crate::ffi::enter();
		let mut guard = self.inner.lock().unwrap();
		let mut media = guard.take().ok_or(MoqError::Closed)?;
		media
			.import
			.finish()
			.map_err(|err| MoqError::Codec(format!("finish failed: {err}")))?;
		Ok(())
	}
}

#[uniffi::export]
impl MoqMediaContainerStreamProducer {
	/// Push raw container bytes. The importer recovers its own framing, so callers can write
	/// arbitrary chunks.
	pub fn write(&self, payload: Vec<u8>) -> Result<(), MoqError> {
		let _guard = crate::ffi::enter();
		let mut guard = self.inner.lock().unwrap();
		let container = guard.as_mut().ok_or(MoqError::Closed)?;
		container
			.import
			.decode(&payload)
			.map_err(|err| MoqError::Codec(format!("decode failed: {err}")))?;
		Ok(())
	}

	/// Finish every track this container publishes.
	pub fn finish(&self) -> Result<(), MoqError> {
		let _guard = crate::ffi::enter();
		let mut guard = self.inner.lock().unwrap();
		let mut container = guard.take().ok_or(MoqError::Closed)?;
		container
			.import
			.finish()
			.map_err(|err| MoqError::Codec(format!("finish failed: {err}")))?;
		Ok(())
	}
}

/// Reserve the named track, or a unique one named after the format.
fn reserve_track(
	broadcast: &moq_net::broadcast::Producer,
	track: Option<String>,
	format: &impl std::fmt::Display,
) -> Result<moq_net::track::Request, MoqError> {
	let name = track.unwrap_or_else(|| broadcast.unique_name(&format!(".{format}")));
	broadcast
		.reserve_track(name)
		.map_err(|err| MoqError::Codec(format!("init failed: {err}")))
}

/// A media track's name, container, and live delivery options.
#[derive(Clone, uniffi::Record)]
pub struct MoqMediaContainerConfig {
	/// The track name.
	pub name: String,
	/// How to decode the track's frames.
	pub container: MoqContainer,
	/// Live delivery options, or defaults when absent.
	#[uniffi(default = None)]
	pub subscription: Option<MoqSubscription>,
}

/// A fetched media group's name, sequence, container, and delivery options.
#[derive(Clone, uniffi::Record)]
pub struct MoqMediaContainerGroupConfig {
	/// The track name.
	pub name: String,
	/// The group sequence.
	pub sequence: u64,
	/// How to decode the group's frames.
	pub container: MoqContainer,
	/// Fetch delivery options, or defaults when absent.
	#[uniffi(default = None)]
	pub options: Option<MoqFetchGroupOptions>,
}

fn media_frame(mut frame: moq_mux::container::Frame) -> Result<MoqMediaFrame, MoqError> {
	let timestamp_us = timestamp_us(frame.timestamp)?;
	let payload = frame.payload.copy_to_bytes(frame.payload.remaining()).to_vec();

	Ok(MoqMediaFrame {
		payload,
		timestamp_us,
		keyframe: frame.keyframe,
	})
}

fn media_container(container: MoqContainer) -> Result<moq_mux::catalog::hang::Container, MoqError> {
	let container: hang::catalog::Container = container.into();
	// This byte-oriented API has no media-kind configuration.
	moq_mux::catalog::hang::Container::new(&container, moq_mux::container::Kind::Data)
		.map_err(|e| MoqError::Codec(format!("invalid container: {e}")))
}

/// Reads the latest catalog snapshots from a broadcast.
#[derive(uniffi::Object)]
pub struct MoqMediaCatalogConsumer {
	task: Task<Catalog>,
}

struct Catalog {
	// Consume with the untyped `Extra` extension so application sections survive into
	// `MoqCatalog.sections` instead of being dropped.
	inner: moq_mux::catalog::hang::Consumer<moq_mux::catalog::hang::Extra>,
}

impl Catalog {
	async fn next(&mut self) -> Result<Option<MoqCatalog>, MoqError> {
		match self.inner.next().await {
			Ok(Some(catalog)) => Ok(Some(convert_catalog(&catalog))),
			Ok(None) => Ok(None),
			Err(e) => Err(e.into()),
		}
	}
}

/// Reads container-decoded frames from a live media track.
#[derive(uniffi::Object)]
pub struct MoqMediaContainerConsumer {
	task: Task<Media>,
}

struct Media {
	inner: moq_mux::container::Consumer<moq_mux::catalog::hang::Container>,
}

impl Media {
	async fn next(&mut self) -> Result<Option<MoqMediaFrame>, MoqError> {
		self.inner.read().await?.map(media_frame).transpose()
	}
}

struct MediaGroupInner {
	inner: moq_mux::container::GroupConsumer<moq_mux::catalog::hang::Container>,
}

impl MediaGroupInner {
	async fn next(&mut self) -> Result<Option<MoqMediaFrame>, MoqError> {
		self.inner.read().await?.map(media_frame).transpose()
	}
}

/// A finite, container-decoded media group returned by
/// [`MoqMediaContainerGroupConsumer::fetch`].
#[derive(uniffi::Object)]
pub struct MoqMediaContainerGroupConsumer {
	sequence: u64,
	task: Task<MediaGroupInner>,
}

impl MoqMediaContainerGroupConsumer {
	fn from_inner(group: moq_net::group::Consumer, container: moq_mux::catalog::hang::Container) -> Self {
		let inner = moq_mux::container::GroupConsumer::new(group, container);
		Self {
			sequence: inner.sequence(),
			task: Task::new(MediaGroupInner { inner }),
		}
	}
}

#[uniffi::export]
impl MoqMediaContainerGroupConsumer {
	/// The sequence number of this group within the track.
	pub fn sequence(&self) -> u64 {
		self.sequence
	}

	/// Read the next decoded media frame, or `None` when the group ends.
	pub async fn next(&self) -> Result<Option<MoqMediaFrame>, MoqError> {
		self.task.run(|mut state| async move { state.next().await }).await
	}

	/// Cancel all current and future `next()` calls.
	///
	/// Terminal: the subscription is released here, not when the handle is.
	pub fn cancel(&self) {
		self.task.cancel();
	}
}

// ---- Catalog Consumer ----

#[uniffi::export]
impl MoqMediaCatalogConsumer {
	/// Get the next catalog update. Returns `None` when the track ends or is closed.
	pub async fn next(&self) -> Result<Option<MoqCatalog>, MoqError> {
		self.task.run(|mut state| async move { state.next().await }).await
	}

	/// Cancel all current and future `next()` calls.
	///
	/// Terminal: the subscription is released here, not when the handle is.
	pub fn cancel(&self) {
		self.task.cancel();
	}
}

// ---- Media Consumer ----

#[uniffi::export]
impl MoqMediaContainerConsumer {
	/// Get the next frame. Returns `None` when the track ends or is closed.
	pub async fn next(&self) -> Result<Option<MoqMediaFrame>, MoqError> {
		self.task.run(|mut state| async move { state.next().await }).await
	}

	/// Cancel all current and future `next()` calls.
	///
	/// Terminal: the subscription is released here, not when the handle is.
	pub fn cancel(&self) {
		self.task.cancel();
	}
}

#[uniffi::export]
impl MoqMediaCatalogConsumer {
	/// Subscribe to this broadcast's catalog snapshots.
	#[uniffi::constructor]
	pub async fn subscribe(broadcast: &MoqBroadcastConsumer) -> Result<Arc<MoqMediaCatalogConsumer>, MoqError> {
		let track = broadcast
			.inner
			.track(hang::catalog::Catalog::DEFAULT_NAME)?
			.subscribe(hang::catalog::Catalog::default_subscription())
			.await?;
		let consumer = moq_mux::catalog::hang::Consumer::from(track);
		Ok(Arc::new(MoqMediaCatalogConsumer {
			task: Task::new(Catalog { inner: consumer }),
		}))
	}
}

#[uniffi::export]
impl MoqMediaContainerConsumer {
	/// Subscribe to a media track and decode its container.
	#[uniffi::constructor]
	pub async fn subscribe(
		broadcast: &MoqBroadcastConsumer,
		config: MoqMediaContainerConfig,
	) -> Result<Arc<MoqMediaContainerConsumer>, MoqError> {
		let MoqMediaContainerConfig {
			name,
			container,
			subscription,
		} = config;
		// Parse the container before subscribing so we don't leave a dangling
		// subscription if init parsing fails.
		let media = media_container(container)?;
		let subscription = subscription.map(moq_net::track::Subscription::from).unwrap_or_default();
		let track = broadcast.inner.track(&name)?.subscribe(subscription).await?;
		let consumer = moq_mux::container::Consumer::new(track, media);
		Ok(Arc::new(MoqMediaContainerConsumer {
			task: Task::new(Media { inner: consumer }),
		}))
	}
}

#[uniffi::export]
impl MoqMediaContainerGroupConsumer {
	/// Fetch and decode exactly one media group.
	#[uniffi::constructor]
	pub async fn fetch(
		broadcast: &MoqBroadcastConsumer,
		config: MoqMediaContainerGroupConfig,
	) -> Result<Arc<MoqMediaContainerGroupConsumer>, MoqError> {
		let MoqMediaContainerGroupConfig {
			name,
			sequence,
			container,
			options,
		} = config;
		// Parse the container before fetching so invalid CMAF init data does not leave a
		// dynamic group request waiting for a consumer that can never read it.
		let media = media_container(container)?;
		let options = options.map(moq_net::group::Fetch::from);
		let track = broadcast.inner.track(&name).map_err(map_fetch_error)?;
		let group = track.fetch_group(sequence, options).await.map_err(map_fetch_error)?;
		Ok(Arc::new(MoqMediaContainerGroupConsumer::from_inner(group, media)))
	}
}

#[cfg(test)]
mod catalog_tests {
	use super::*;

	#[test]
	fn catalog_handle_keeps_no_broadcast_open() {
		let broadcast = MoqBroadcastProducer::new().unwrap();
		let catalog = MoqMediaCatalogProducer::new(broadcast.clone()).unwrap();
		drop(broadcast);
		assert!(matches!(
			catalog.set_section("app".into(), "{}".into()),
			Err(MoqError::Closed)
		));
		assert!(matches!(catalog.remove_section("app".into()), Err(MoqError::Closed)));
	}

	#[test]
	fn catalog_refuses_writes_after_broadcast_close() {
		let broadcast = MoqBroadcastProducer::new().unwrap();
		let catalog = MoqMediaCatalogProducer::new(broadcast.clone()).unwrap();
		broadcast.close().unwrap();
		assert!(matches!(
			catalog.set_video_properties(MoqVideoProperties::default()),
			Err(MoqError::Closed)
		));
		assert!(matches!(MoqMediaCatalogProducer::new(broadcast), Err(MoqError::Closed)));
	}

	#[tokio::test]
	async fn catalog_handle_updates_sections() {
		let broadcast = MoqBroadcastProducer::new().unwrap();
		let catalog = MoqMediaCatalogProducer::new(broadcast.clone()).unwrap();
		let consumer = MoqMediaCatalogConsumer::subscribe(&broadcast.consume().unwrap())
			.await
			.unwrap();
		catalog.set_section("app".into(), "{\"value\":42}".into()).unwrap();
		let snapshot = consumer.next().await.unwrap().unwrap();
		assert_eq!(snapshot.sections["app"], "{\"value\":42}");
		catalog.remove_section("app".into()).unwrap();
		assert!(!consumer.next().await.unwrap().unwrap().sections.contains_key("app"));
		assert!(catalog.set_section("video".into(), "{}".into()).is_err());
		assert!(catalog.set_section("app".into(), "{".into()).is_err());
		consumer.cancel();
	}
}
