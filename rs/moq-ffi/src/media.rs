//! Publish and consume media catalogs and encoded frames.

use std::collections::HashMap;
use std::sync::{Arc, Weak};

use bytes::Buf;
use moq_mux::catalog::hang::Extra;

use crate::consumer::{
	MoqBroadcastConsumer, MoqFetchGroupOptions, MoqGroupConsumer, MoqSubscription, map_fetch_error, timestamp_us,
};
use crate::demand::MoqTrackDemand;
use crate::error::MoqError;
use crate::ffi::Task;
use crate::producer::{Child, Children, MoqBroadcastProducer, MoqTrackRequest};

/// A width and height in pixels.
#[derive(Clone, uniffi::Record)]
pub struct MoqDimensions {
	/// The width in pixels.
	pub width: u32,
	/// The height in pixels.
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
	Cmaf {
		/// The CMAF initialization segment.
		init: Vec<u8>,
	},
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

/// A catalog snapshot describing the available media renditions.
#[derive(uniffi::Record)]
pub struct MoqCatalog {
	/// Video renditions keyed by their catalog IDs.
	pub video: HashMap<String, MoqVideo>,
	/// Audio renditions keyed by their catalog IDs.
	pub audio: HashMap<String, MoqAudio>,
	/// The final rendered size after rotation, when specified.
	pub display: Option<MoqDimensions>,
	/// The clockwise rotation in degrees, when specified.
	pub rotation: Option<f64>,
	/// Whether to flip horizontally after rotation, when specified.
	pub flip: Option<bool>,
	/// Untyped application catalog sections, keyed by section name, each value a JSON string.
	/// These are the top-level catalog keys beyond `video`/`audio`, carried through verbatim
	/// (parse the JSON yourself). Set them on the publish side with
	/// [`set_section`](MoqMediaCatalogProducer::set_section).
	pub sections: HashMap<String, String>,
}

/// A video rendition and the configuration needed to decode it.
#[derive(Clone, uniffi::Record)]
pub struct MoqVideo {
	/// Human-readable rendition name for track pickers.
	#[uniffi(default = None)]
	pub label: Option<String>,
	/// The broadcast serving this rendition's track, relative to the catalog's own broadcast
	/// (e.g. `./source`). Absent or empty means the catalog's broadcast.
	#[uniffi(default = None)]
	pub broadcast: Option<String>,
	/// The publisher epoch of the instance [`broadcast`](Self::broadcast) names, when the
	/// catalog knows it. Resolving refuses a broadcast proven to serve another instance; absent
	/// names whichever instance resolves first.
	#[uniffi(default = None)]
	pub epoch: Option<String>,
	/// The track's name in its broadcast, when it differs from the rendition's ID. Absent means
	/// the ID is the track name.
	#[uniffi(default = None)]
	pub track: Option<String>,
	/// The codec string identifying this rendition's encoded format.
	pub codec: String,
	/// The codec initialization bytes, when required.
	pub description: Option<Vec<u8>>,
	/// The encoded pixel dimensions.
	pub coded: Option<MoqDimensions>,
	/// The aspect ratio the whole picture is displayed at; absent means the decoded shape.
	pub display_aspect: Option<MoqDimensions>,
	/// The maximum encoded bitrate in bits per second.
	pub bitrate: Option<u64>,
	/// Whether this rendition may be selected; when false, no frames are coming.
	#[uniffi(default = true)]
	pub enabled: bool,
	/// Higher values are preferred; compatibility transcodes use -1 and the default is 0.
	#[uniffi(default = 0)]
	pub preference: i32,
	/// The frame rate in frames per second, when specified.
	pub framerate: Option<f64>,
	/// The framing used to carry this rendition.
	pub container: MoqContainer,
}

/// An audio rendition and the configuration needed to decode it.
#[derive(Clone, uniffi::Record)]
pub struct MoqAudio {
	/// Human-readable rendition name for track pickers.
	#[uniffi(default = None)]
	pub label: Option<String>,
	/// The broadcast serving this rendition's track, relative to the catalog's own broadcast
	/// (e.g. `./source`). Absent or empty means the catalog's broadcast.
	#[uniffi(default = None)]
	pub broadcast: Option<String>,
	/// The publisher epoch of the instance [`broadcast`](Self::broadcast) names, when the
	/// catalog knows it. Resolving refuses a broadcast proven to serve another instance; absent
	/// names whichever instance resolves first.
	#[uniffi(default = None)]
	pub epoch: Option<String>,
	/// The track's name in its broadcast, when it differs from the rendition's ID. Absent means
	/// the ID is the track name.
	#[uniffi(default = None)]
	pub track: Option<String>,
	/// The codec string identifying this rendition's encoded format.
	pub codec: String,
	/// The codec initialization bytes, when required.
	pub description: Option<Vec<u8>>,
	/// The number of samples per second.
	pub sample_rate: u32,
	/// The number of audio channels.
	pub channel_count: u32,
	/// The maximum encoded bitrate in bits per second.
	pub bitrate: Option<u64>,
	/// Whether this rendition may be selected; when false, no frames are coming.
	#[uniffi(default = true)]
	pub enabled: bool,
	/// The framing used to carry this rendition.
	pub container: MoqContainer,
	/// The publisher's worst-case delay between a frame being ready and its flush, which
	/// floors an estimated playout delay. Absent means frames flush at once.
	#[uniffi(default = None)]
	pub jitter: Option<std::time::Duration>,
}

/// A payload and the time it should be presented.
///
/// The unit of both writing and raw reading: every producer write takes one of these, and a
/// raw (non-media) read returns one. Media reads carry a [`MoqMediaFrame`] in their frame
/// variant, which adds the codec-derived keyframe flag.
#[derive(Clone, uniffi::Record)]
pub struct MoqFrame {
	/// The frame payload.
	pub payload: Vec<u8>,
	/// Presentation timestamp in microseconds, or null on an untimed track, on both publish and receive.
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
	/// Presentation timestamp in microseconds, or null on an untimed track, on both publish and receive.
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
	/// The aspect ratio the whole picture is displayed at, not the sample aspect. Absent means the decoded shape.
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
	/// Fragmented MP4 / CMAF, split into one frame per sample.
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

/// Shares one source's timestamp offset across containers without keeping its broadcast open.
#[derive(uniffi::Object)]
pub struct MoqMediaTimebase {
	broadcast: Weak<MoqBroadcastProducer>,
	// Stable identity even when a Rust caller unwraps the outer Arc and moves the handle.
	key: Arc<()>,
}

#[uniffi::export]
impl MoqMediaTimebase {
	/// Create a source timebase owned by this broadcast.
	#[uniffi::constructor]
	pub fn new(broadcast: Arc<MoqBroadcastProducer>) -> Result<Arc<Self>, MoqError> {
		let handle = Arc::new(Self {
			broadcast: Arc::downgrade(&broadcast),
			key: Arc::new(()),
		});
		broadcast.with_state(|state| {
			state
				.timebases
				.insert(Arc::as_ptr(&handle.key) as usize, state.catalog.timebase());
			Ok(())
		})?;
		Ok(handle)
	}
}

impl MoqMediaTimebase {
	fn reserve(
		&self,
		broadcast: &MoqBroadcastProducer,
		state: &crate::producer::BroadcastProducer,
	) -> Result<moq_mux::catalog::Reserved<Extra>, MoqError> {
		if !std::ptr::eq(self.broadcast.as_ptr(), broadcast) {
			return Err(MoqError::Codec {
				reason: "timebase belongs to another broadcast".into(),
			});
		}
		let timebase = state
			.timebases
			.get(&(Arc::as_ptr(&self.key) as usize))
			.ok_or(MoqError::Closed)?;
		Ok(timebase.reserve())
	}
}

impl Drop for MoqMediaTimebase {
	fn drop(&mut self) {
		if let Some(broadcast) = self.broadcast.upgrade()
			&& let Some(state) = broadcast.state.lock().unwrap().as_mut()
		{
			state.timebases.remove(&(Arc::as_ptr(&self.key) as usize));
		}
	}
}

/// Reserve a container's catalog slot under the owner lock, from `timebase` when shared, so the
/// importer is constructed after the lock is released.
///
/// Also returns the broadcast's driver, which the importer checks so a driver panic reaches it.
fn reserve_container(
	broadcast: &MoqBroadcastProducer,
	timebase: Option<&MoqMediaTimebase>,
) -> Result<
	(
		moq_net::broadcast::Producer,
		moq_mux::catalog::Reserved<Extra>,
		crate::ffi::Background,
		crate::producer::Children,
	),
	MoqError,
> {
	broadcast.with_state(|state| {
		let reserved = match timebase {
			Some(timebase) => timebase.reserve(broadcast, state)?,
			None => state.catalog.reserve(),
		};
		Ok((
			state.broadcast.clone(),
			reserved,
			state.driver.clone(),
			state.children.clone(),
		))
	})
}

/// A container format, its leading bytes, and an optional shared source timebase.
///
/// A container publishes and describes its own tracks, so there is no label or hint here: a
/// rendition field would have no single track to land on.
#[derive(Clone, uniffi::Record)]
pub struct MoqContainerInit {
	/// The container format.
	pub format: MoqContainerFormat,
	/// The leading chunk of the container, decoded immediately. May be empty.
	pub data: Vec<u8>,
	/// A shared source timebase, or absent for an independent offset.
	#[uniffi(default = None)]
	pub timebase: Option<Arc<MoqMediaTimebase>>,
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
					epoch: config.epoch.as_ref().map(ToString::to_string),
					track: config.track.clone(),
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
					preference: config.preference,
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
					epoch: config.epoch.as_ref().map(ToString::to_string),
					track: config.track.clone(),
					codec: config.codec.to_string(),
					description: config.description.as_ref().map(|d| d.to_vec()),
					sample_rate: config.sample_rate,
					channel_count: config.channel_count,
					bitrate: config.bitrate,
					enabled: config.enabled,
					container: MoqContainer::from_catalog(&config.container)?,
					jitter: config.jitter.map(crate::ffi::duration),
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
	fn catalog_preserves_video_preference() {
		let mut catalog = moq_mux::catalog::hang::Catalog::default();
		for preference in [i32::MIN, -1, 0, 1, i32::MAX] {
			let mut video = hang::catalog::VideoConfig::new(hang::catalog::VideoCodec::VP8);
			video.preference = preference;
			catalog.video.renditions.insert(preference.to_string(), video);
		}
		let converted = convert_catalog(&catalog);
		for (id, video) in &catalog.video.renditions {
			assert_eq!(converted.video[id].preference, video.preference);
		}
	}

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

		let epoch: moq_net::Epoch = crate::origin::moq_mint_epoch().parse().unwrap();
		let mut video = hang::catalog::VideoConfig::new(hang::catalog::VideoCodec::VP8);
		video.broadcast = Some(moq_net::path::Relative::new("./source").unwrap());
		video.epoch = Some(epoch.clone());
		catalog.video.renditions.insert("video".to_string(), video);
		let local = hang::catalog::VideoConfig::new(hang::catalog::VideoCodec::VP8);
		catalog.video.renditions.insert("local".to_string(), local);

		let mut audio = hang::catalog::AudioConfig::new(hang::catalog::AudioCodec::Opus, 48_000, 2);
		audio.broadcast = Some(moq_net::path::Relative::new("../elsewhere").unwrap());
		catalog.audio.renditions.insert("audio".to_string(), audio);

		let converted = convert_catalog(&catalog);
		// `path::Relative` strips redundant `.` segments on creation, so the reference crosses as
		// the normalized form the catalog itself holds. It round-trips: rebuilding a
		// `path::Relative` from it is a no-op.
		assert_eq!(converted.video["video"].broadcast.as_deref(), Some("source"));
		assert_eq!(converted.video["local"].broadcast, None);
		assert_eq!(converted.audio["audio"].broadcast.as_deref(), Some("../elsewhere"));
		// The epoch crosses in the same canonical form `resolve` parses back.
		assert_eq!(converted.video["video"].epoch, Some(epoch.to_string()));
		assert_eq!(converted.video["local"].epoch, None);
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
	Named {
		/// The explicit track name, or absent to allocate a unique name from its format.
		name: Option<String>,
	},
	/// A requested track whose name and ownership are already fixed.
	Requested {
		/// The pending track request to accept.
		request: Arc<MoqTrackRequest>,
	},
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

impl MoqMediaCatalogProducer {
	#[cfg(all(any(feature = "audio", feature = "video"), not(target_arch = "wasm32")))]
	pub(crate) fn broadcast(&self) -> Result<Arc<MoqBroadcastProducer>, MoqError> {
		self.broadcast.upgrade().ok_or(MoqError::Closed)
	}
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

	/// Read the broadcast clock in microseconds, fixing it for subsequent media producers.
	pub fn now(&self) -> Result<u64, MoqError> {
		let _guard = crate::ffi::enter();
		self.broadcast.upgrade().ok_or(MoqError::Closed)?.with_state(|state| {
			u64::try_from(state.catalog.clock().now().as_micros()).map_err(|_| MoqError::Codec {
				reason: "broadcast clock exceeds microsecond timestamp range".into(),
			})
		})
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

	/// Finish the catalog, which ends the broadcast's content.
	///
	/// Finish every track first. The broadcast stays announced and keeps serving its finished
	/// tracks, so a subscriber that arrives late can still read them, until the broadcast's
	/// `close` takes it offline. How long to keep it up is yours to decide.
	pub fn finish(&self) -> Result<(), MoqError> {
		let _guard = crate::ffi::enter();
		self.broadcast.upgrade().ok_or(MoqError::Closed)?.with_state(|state| {
			state.catalog.finish()?;
			state.ended = true;
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
}

/// An importer for a container, which may publish several tracks.
struct ContainerProducer {
	import: moq_mux::import::Container<Extra>,
}

/// A byte-stream importer for one codec track, where frame boundaries are inferred.
struct MediaStreamProducer {
	import: Box<moq_mux::import::TrackStream>,
}

/// Imports a container into a broadcast, from chunks split anywhere such as a raw byte stream.
///
/// Released before its finish, or still open when its broadcast closes, it ends its tracks with
/// `Unroutable`, which readers take as the publisher leaving.
#[derive(uniffi::Object)]
pub struct MoqMediaContainerProducer {
	driver: crate::ffi::Background,
	inner: std::sync::Mutex<Option<ContainerProducer>>,
	children: Children,
}

/// Imports complete encoded frames onto one track.
///
/// Released before its finish, or still open when its broadcast closes, it ends its track with
/// `Unroutable`, which readers take as the publisher leaving.
#[derive(uniffi::Object)]
pub struct MoqMediaTrackProducer {
	inner: std::sync::Mutex<Option<MediaProducer>>,
	children: Children,
}

/// Imports a video byte stream onto one track.
///
/// Released before its finish, or still open when its broadcast closes, it ends its track with
/// `Unroutable`, which readers take as the publisher leaving.
#[derive(uniffi::Object)]
pub struct MoqMediaTrackStreamProducer {
	inner: std::sync::Mutex<Option<MediaStreamProducer>>,
	children: Children,
}

// A media handle dropped or closed before its finish ends its tracks the way a publisher
// leaving does, never as a cause-less drop.
impl Child for MoqMediaContainerProducer {
	fn abort(&self, err: moq_net::Error) {
		let _guard = crate::ffi::enter();
		if let Some(container) = self.inner.lock().unwrap().take() {
			container.import.abort(err);
		}
	}
}

impl Drop for MoqMediaContainerProducer {
	fn drop(&mut self) {
		self.abort(moq_net::Error::Unroutable);
	}
}

impl Child for MoqMediaTrackProducer {
	fn abort(&self, err: moq_net::Error) {
		let _guard = crate::ffi::enter();
		if let Some(media) = self.inner.lock().unwrap().take() {
			media.import.abort(err);
		}
	}
}

impl Drop for MoqMediaTrackProducer {
	fn drop(&mut self) {
		self.abort(moq_net::Error::Unroutable);
	}
}

impl Child for MoqMediaTrackStreamProducer {
	fn abort(&self, err: moq_net::Error) {
		let _guard = crate::ffi::enter();
		if let Some(media) = self.inner.lock().unwrap().take() {
			media.import.abort(err);
		}
	}
}

impl Drop for MoqMediaTrackStreamProducer {
	fn drop(&mut self) {
		self.abort(moq_net::Error::Unroutable);
	}
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
		broadcast.with_state(|state| {
			let init: moq_mux::import::AudioInit = init.into();
			let request = target.resolve(&state.broadcast, &init.format)?;

			let import = moq_mux::import::Track::audio(request, state.catalog.reserve(), init).map_err(|err| {
				MoqError::Codec {
					reason: format!("init failed: {err}"),
				}
			})?;
			MoqMediaTrackProducer::from_import(import, &state.children)
		})
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
		broadcast.with_state(|state| {
			let init: moq_mux::import::VideoInit = init.into();
			let request = target.resolve(&state.broadcast, &init.format)?;

			let import = moq_mux::import::Track::video(request, state.catalog.reserve(), init).map_err(|err| {
				MoqError::Codec {
					reason: format!("init failed: {err}"),
				}
			})?;
			MoqMediaTrackProducer::from_import(import, &state.children)
		})
	}
}

#[uniffi::export]
impl MoqMediaContainerProducer {
	/// Construct an importer from this broadcast.
	///
	/// A supplied timebase must belong to this broadcast; absence gives it an independent offset.
	#[uniffi::constructor]
	pub fn new(
		broadcast: &MoqBroadcastProducer,
		init: MoqContainerInit,
	) -> Result<Arc<MoqMediaContainerProducer>, MoqError> {
		let _guard = crate::ffi::enter();
		let native = moq_mux::import::ContainerInit::new(init.format.into(), init.data);
		let (target, reserved, driver, children) = reserve_container(broadcast, init.timebase.as_deref())?;
		let import = moq_mux::import::Container::new(target, reserved, &native).map_err(|err| MoqError::Codec {
			reason: format!("init failed: {err}"),
		})?;

		children.adopt(Arc::new(MoqMediaContainerProducer {
			driver,
			inner: std::sync::Mutex::new(Some(ContainerProducer { import })),
			children: children.clone(),
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
		broadcast.with_state(|state| {
			let init: moq_mux::import::VideoInit = init.into();
			let request = target.resolve(&state.broadcast, &init.format)?;

			let import =
				moq_mux::import::TrackStream::video(request, state.catalog.reserve(), init).map_err(|err| {
					MoqError::Codec {
						reason: format!("init failed: {err}"),
					}
				})?;

			state.children.adopt(Arc::new(MoqMediaTrackStreamProducer {
				inner: std::sync::Mutex::new(Some(MediaStreamProducer {
					import: Box::new(import),
				})),
				children: state.children.clone(),
			}))
		})
	}
}

impl MoqMediaTrackProducer {
	/// Wrap a single-codec importer.
	fn from_import(import: moq_mux::import::Track, children: &Children) -> Result<Arc<Self>, MoqError> {
		children.adopt(Arc::new(Self {
			inner: std::sync::Mutex::new(Some(MediaProducer {
				import: Box::new(import),
			})),
			children: children.clone(),
		}))
	}
}

#[uniffi::export]
impl MoqMediaTrackProducer {
	/// A watch-only handle to whether this track has subscribers.
	///
	/// A frame that changes the decoder identity publishes under a new track name, so read this
	/// again after [`write_frame`](Self::write_frame) rather than holding the first handle.
	pub fn demand(&self) -> Result<Arc<MoqTrackDemand>, MoqError> {
		let guard = self.inner.lock().unwrap();
		Ok(MoqTrackDemand::new(
			guard.as_ref().ok_or(MoqError::Closed)?.import.demand(),
		))
	}

	/// Write `frame` to this track.
	///
	/// The importer derives keyframe status from the bitstream, so a [`MoqFrame`] carries only the
	/// payload and its caller-supplied timestamp. A missing timestamp is refused.
	pub fn write_frame(&self, frame: MoqFrame) -> Result<(), MoqError> {
		let _guard = crate::ffi::enter();
		let mut guard = self.inner.lock().unwrap();
		let media = guard.as_mut().ok_or(MoqError::Closed)?;

		let timestamp =
			hang::container::Timestamp::from_micros(frame.timestamp_us.ok_or_else(|| MoqError::Codec {
				reason: "whole media frames require a timestamp".into(),
			})?)?;
		media
			.import
			.decode(&frame.payload, Some(timestamp))
			.map_err(|err| MoqError::Codec {
				reason: format!("decode failed: {err}"),
			})?;

		Ok(())
	}

	/// Mark a timeline break without lowering the advertised jitter or delay.
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
		media.import.cut(None).map_err(|err| MoqError::Codec {
			reason: format!("cut failed: {err}"),
		})?;
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
		media.import.seek(sequence).map_err(|err| MoqError::Codec {
			reason: format!("seek failed: {err}"),
		})?;
		Ok(())
	}

	/// Finish this track and finalize encoding.
	///
	/// The broadcast keeps the finished track until it ends, so its finish still holds it. A
	/// failed finish ends the track with its error instead.
	pub fn finish(&self) -> Result<(), MoqError> {
		let _guard = crate::ffi::enter();
		let mut guard = self.inner.lock().unwrap();
		let mut media = guard.take().ok_or(MoqError::Closed)?;
		if let Err(err) = media.import.finish() {
			media.import.abort(cause(&err));
			return Err(MoqError::Codec {
				reason: format!("finish failed: {err}"),
			});
		}
		self.children.keep(media);
		Ok(())
	}
}

#[uniffi::export]
impl MoqMediaContainerProducer {
	/// Write the next container bytes. The container recovers its own framing, so a chunk may split
	/// anywhere.
	///
	/// No timestamp: a container carries its tracks' timing itself, and the importer reads it out
	/// rather than taking the caller's word for it.
	pub fn write(&self, payload: Vec<u8>) -> Result<(), MoqError> {
		self.driver.check()?;
		let result = (|| {
			let _guard = crate::ffi::enter();
			let mut guard = self.inner.lock().unwrap();
			let container = guard.as_mut().ok_or(MoqError::Closed)?;
			container.import.decode(&payload).map_err(|err| MoqError::Codec {
				reason: format!("decode failed: {err}"),
			})?;
			Ok(())
		})();
		self.driver.check()?;
		result
	}

	/// Declare that the next chunk starts a new segment, rolling a group on every track this
	/// publishes.
	///
	/// For a caller that knows its source's segmentation out of band. An fMP4 source carrying
	/// `styp` atoms declares its own, so this is only needed when it doesn't, and formats with no
	/// segment concept (MKV, TS, FLV) ignore it.
	pub fn cut(&self) -> Result<(), MoqError> {
		self.driver.check()?;
		let result = (|| {
			let _guard = crate::ffi::enter();
			let mut guard = self.inner.lock().unwrap();
			let container = guard.as_mut().ok_or(MoqError::Closed)?;
			container.import.cut().map_err(|err| MoqError::Codec {
				reason: format!("cut failed: {err}"),
			})?;
			Ok(())
		})();
		self.driver.check()?;
		result
	}

	/// Start a new segment and number its groups `sequence`.
	pub fn seek(&self, sequence: u64) -> Result<(), MoqError> {
		self.driver.check()?;
		let result = (|| {
			let _guard = crate::ffi::enter();
			let mut guard = self.inner.lock().unwrap();
			let container = guard.as_mut().ok_or(MoqError::Closed)?;
			container.import.seek(sequence).map_err(|err| MoqError::Codec {
				reason: format!("seek failed: {err}"),
			})?;
			Ok(())
		})();
		self.driver.check()?;
		result
	}

	/// Finish every track this container publishes.
	///
	/// The broadcast keeps the finished tracks until it ends, so its finish still holds them. A
	/// failed finish ends the tracks with its error instead.
	pub fn finish(&self) -> Result<(), MoqError> {
		let result = (|| {
			let _guard = crate::ffi::enter();
			let mut guard = self.inner.lock().unwrap();
			let mut container = guard.take().ok_or(MoqError::Closed)?;
			if let Err(err) = container.import.finish() {
				container.import.abort(cause(&err));
				return Err(MoqError::Codec {
					reason: format!("finish failed: {err}"),
				});
			}
			self.children.keep(container);
			Ok(())
		})();
		self.driver.check()?;
		result
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
		media.import.decode(&payload).map_err(|err| MoqError::Codec {
			reason: format!("decode failed: {err}"),
		})?;
		Ok(())
	}

	/// Finalize the track, publishing the last access unit still waiting for a following start code.
	///
	/// The broadcast keeps the finished track until it ends, so its finish still holds it. A
	/// failed finish ends the track with its error instead.
	pub fn finish(&self) -> Result<(), MoqError> {
		let _guard = crate::ffi::enter();
		let mut guard = self.inner.lock().unwrap();
		let mut media = guard.take().ok_or(MoqError::Closed)?;
		if let Err(err) = media.import.finish() {
			media.import.abort(cause(&err));
			return Err(MoqError::Codec {
				reason: format!("finish failed: {err}"),
			});
		}
		self.children.keep(media);
		Ok(())
	}
}

/// The cause a failed import ends its tracks with, so a reader sees the publisher fail rather than
/// `Dropped`. Only a moq-net error has a wire code; any other reaches readers as `Internal`.
fn cause(err: &moq_mux::Error) -> moq_net::Error {
	match err {
		moq_mux::Error::Moq(err) => err.clone(),
		_ => moq_net::StreamError::Internal.into(),
	}
}

/// Reserve the named track, or a unique one named after the format.
fn reserve_track(
	broadcast: &moq_net::broadcast::Producer,
	track: Option<String>,
	format: &impl std::fmt::Display,
) -> Result<moq_net::track::Request, MoqError> {
	let name = track.unwrap_or_else(|| broadcast.unique_name(&format!(".{format}")));
	broadcast.reserve_track(name).map_err(|err| MoqError::Codec {
		reason: format!("init failed: {err}"),
	})
}

/// A catalog rendition: its ID (its key in the catalog) and its config.
///
/// The consumers that take one resolve the rendition's broadcast reference and track name
/// themselves, so pass the ID exactly as the catalog lists it.
#[derive(Clone, uniffi::Enum)]
pub enum MoqRendition {
	/// A video rendition.
	Video {
		/// The rendition ID in the catalog.
		id: String,
		/// The rendition configuration.
		config: MoqVideo,
	},
	/// An audio rendition.
	Audio {
		/// The rendition ID in the catalog.
		id: String,
		/// The rendition configuration.
		config: MoqAudio,
	},
}

impl MoqRendition {
	/// Split into the broadcast reference and its epoch, the track name, and the container.
	pub(crate) fn into_parts(self) -> (Option<String>, Option<String>, String, MoqContainer) {
		let (id, broadcast, epoch, track, container) = match self {
			Self::Video { id, config } => (id, config.broadcast, config.epoch, config.track, config.container),
			Self::Audio { id, config } => (id, config.broadcast, config.epoch, config.track, config.container),
		};
		(broadcast, epoch, track.unwrap_or(id), container)
	}
}

/// A media rendition to subscribe to, and its live delivery options.
#[derive(Clone, uniffi::Record)]
pub struct MoqMediaTrackConfig {
	/// The catalog rendition to subscribe to.
	pub rendition: MoqRendition,
	/// Live delivery options, or defaults when absent.
	#[uniffi(default = None)]
	pub subscription: Option<MoqSubscription>,
}

/// A media rendition's group to fetch, and its delivery options.
#[derive(Clone, uniffi::Record)]
pub struct MoqMediaGroupConfig {
	/// The catalog rendition to fetch from.
	pub rendition: MoqRendition,
	/// The group sequence.
	pub sequence: u64,
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

fn media_container(
	container: MoqContainer,
	kind: moq_mux::container::Kind,
) -> Result<moq_mux::catalog::hang::Container, MoqError> {
	let container: hang::catalog::Container = container.into();
	moq_mux::catalog::hang::Container::new(&container, kind).map_err(|e| MoqError::Codec {
		reason: format!("invalid container: {e}"),
	})
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

/// One encoded media read: a container-decoded frame, a gap, or an encoder end.
#[derive(uniffi::Enum)]
pub enum MoqMediaRead {
	/// An encoded frame with its container metadata.
	Frame {
		/// The encoded payload, timestamp, and keyframe flag.
		frame: MoqMediaFrame,
	},
	/// Media after what was already read was lost; what follows does not continue it.
	Gap {
		/// The furthest presentation point already read, in microseconds.
		timestamp_us: u64,
		/// An upper bound on how long until the next frame resumes.
		duration: std::time::Duration,
	},
	/// The publisher stopped its encoder: it paused, switched source, or finished.
	/// Frames starting before `end_us` are its remaining media; the first at or past it
	/// comes from a fresh encoder.
	End {
		/// Where the stopped encoder's media stops, in microseconds.
		end_us: u64,
	},
}

impl TryFrom<moq_mux::container::Read> for MoqMediaRead {
	type Error = MoqError;

	fn try_from(read: moq_mux::container::Read) -> Result<Self, MoqError> {
		use crate::consumer::timestamp_us;
		Ok(match read {
			moq_mux::container::Read::Frame { frame, .. } => Self::Frame {
				frame: media_frame(frame)?,
			},
			moq_mux::container::Read::Gap { timestamp, duration } => Self::Gap {
				timestamp_us: timestamp_us(timestamp)?,
				duration: crate::ffi::duration(duration.into()),
			},
			moq_mux::container::Read::End { end, .. } => Self::End {
				end_us: timestamp_us(end)?,
			},
		})
	}
}

/// Reads encoded frames and the breaks between them from a live media track.
#[derive(uniffi::Object)]
pub struct MoqMediaTrackConsumer {
	task: Task<Media>,
}

struct Media {
	inner: moq_mux::container::Consumer<moq_mux::catalog::hang::Container>,
}

impl Media {
	async fn next(&mut self) -> Result<Option<MoqMediaRead>, MoqError> {
		self.inner.read().await?.map(MoqMediaRead::try_from).transpose()
	}
}

#[cfg(test)]
mod read_tests {
	use super::*;
	use moq_mux::container::Container as _;
	use std::time::Duration;

	#[tokio::test(start_paused = true)]
	async fn encoded_media_preserves_breaks() {
		let broadcast = moq_net::broadcast::Info::new().produce();
		let track = broadcast
			.create_track("encoded", hang::container::track_info(0))
			.unwrap();
		let format = || moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Audio);
		let reader = MoqMediaTrackConsumer::subscribe(
			&MoqBroadcastConsumer::new(broadcast.consume()),
			MoqMediaTrackConfig {
				rendition: MoqRendition::Audio {
					id: "encoded".into(),
					config: MoqAudio {
						label: None,
						broadcast: None,
						epoch: None,
						track: None,
						codec: "opus".into(),
						description: None,
						sample_rate: 48_000,
						channel_count: 2,
						bitrate: None,
						enabled: true,
						container: MoqContainer::Legacy,
						jitter: None,
					},
				},
				subscription: None,
			},
		)
		.await
		.unwrap();
		let timestamp = |us| moq_net::Timestamp::from_micros(us).unwrap();
		let mut first = track.create_group(0u64.into()).unwrap();
		format()
			.write(
				&mut first,
				&[moq_mux::container::Frame {
					timestamp: timestamp(10_000),
					payload: bytes::Bytes::from_static(b"encoded"),
					keyframe: true,
					duration: None,
				}],
			)
			.unwrap();
		first.finish().unwrap();
		let Some(MoqMediaRead::Frame { frame }) = reader.next().await.unwrap() else {
			panic!("expected frame")
		};
		assert_eq!(frame.payload, b"encoded");
		assert_eq!(frame.timestamp_us, 10_000);
		assert!(frame.keyframe);
		let mut marker = track.create_group(2u64.into()).unwrap();
		format()
			.write(
				&mut marker,
				&[moq_mux::container::Frame {
					timestamp: timestamp(20_000),
					payload: bytes::Bytes::new(),
					keyframe: false,
					duration: None,
				}],
			)
			.unwrap();
		marker.finish().unwrap();
		assert!(
			matches!(reader.next().await.unwrap(), Some(MoqMediaRead::Gap { timestamp_us: 10_000, duration }) if duration == Duration::from_millis(10))
		);
		assert!(matches!(
			reader.next().await.unwrap(),
			Some(MoqMediaRead::End { end_us: 20_000 })
		));
		let mut resumed = track.create_group(3u64.into()).unwrap();
		format()
			.write(
				&mut resumed,
				&[moq_mux::container::Frame {
					timestamp: timestamp(20_000),
					payload: bytes::Bytes::from_static(b"resumed"),
					keyframe: true,
					duration: None,
				}],
			)
			.unwrap();
		resumed.finish().unwrap();
		let Some(MoqMediaRead::Frame { frame }) = reader.next().await.unwrap() else {
			panic!("end is not EOF")
		};
		assert_eq!(frame.payload, b"resumed");
		track.finish().unwrap();
		assert!(reader.next().await.unwrap().is_none());
		reader.cancel();
		assert!(matches!(reader.next().await, Err(MoqError::Cancelled)));
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

/// A finite, container-decoded media group, fetched or decoded from a raw group.
#[derive(uniffi::Object)]
pub struct MoqMediaGroupConsumer {
	sequence: u64,
	task: Task<MediaGroupInner>,
}

impl MoqMediaGroupConsumer {
	fn from_inner(
		group: moq_net::group::Consumer,
		container: moq_mux::catalog::hang::Container,
		driver: crate::ffi::Background,
	) -> Self {
		let inner = moq_mux::container::GroupConsumer::new(group, container);
		Self {
			sequence: inner.sequence(),
			task: Task::new(MediaGroupInner { inner }).supervised(driver),
		}
	}
}

#[uniffi::export]
impl MoqMediaGroupConsumer {
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
impl MoqMediaTrackConsumer {
	/// Get the next encoded frame, gap, or end; `None` once the track ends or is closed.
	pub async fn next(&self) -> Result<Option<MoqMediaRead>, MoqError> {
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
			.driver
			.watch(async {
				Ok(broadcast
					.inner
					.track(hang::catalog::Catalog::DEFAULT_NAME)?
					.subscribe(hang::catalog::Catalog::default_subscription())
					.await?)
			})
			.await?;
		let consumer = moq_mux::catalog::hang::Consumer::from(track);
		Ok(Arc::new(MoqMediaCatalogConsumer {
			task: Task::new(Catalog { inner: consumer }).supervised(broadcast.driver.clone()),
		}))
	}
}

#[uniffi::export]
impl MoqMediaTrackConsumer {
	/// Subscribe to a catalog rendition and decode its container.
	///
	/// Follows the rendition's broadcast reference and track name, so a rendition served by a
	/// sibling broadcast or under another track name subscribes to the right track, and fails
	/// one whose broadcast is proven to serve another instance than its epoch.
	#[uniffi::constructor]
	pub async fn subscribe(
		broadcast: &MoqBroadcastConsumer,
		config: MoqMediaTrackConfig,
	) -> Result<Arc<MoqMediaTrackConsumer>, MoqError> {
		let MoqMediaTrackConfig {
			rendition,
			subscription,
		} = config;
		let kind = match &rendition {
			MoqRendition::Audio { .. } => moq_mux::container::Kind::Audio,
			MoqRendition::Video { .. } => moq_mux::container::Kind::Video,
		};
		let (reference, epoch, name, container) = rendition.into_parts();
		// Parse the container before resolving or subscribing so we don't leave a dangling
		// subscription if init parsing fails.
		let media = media_container(container, kind)?;
		let resolved = broadcast.resolve_inner(reference.as_deref(), epoch.as_deref()).await?;
		let subscription = subscription
			.map(moq_net::track::Subscription::try_from)
			.transpose()?
			.unwrap_or_default();
		let track = broadcast
			.driver
			.watch(async { Ok(resolved.track(&name)?.subscribe(subscription).await?) })
			.await?;
		let consumer = moq_mux::container::Consumer::new(track, media);
		Ok(Arc::new(MoqMediaTrackConsumer {
			task: Task::new(Media { inner: consumer }).supervised(broadcast.driver.clone()),
		}))
	}
}

#[uniffi::export]
impl MoqMediaGroupConsumer {
	/// Decode `group` with the track's container, as each frame lands.
	///
	/// Pair it with [`MoqTrackConsumer::recv_group`](crate::consumer::MoqTrackConsumer::recv_group)
	/// to read media in arrival order: frame timestamps are container media time, not the
	/// transport's. Which groups arrive, and how long a stalled one stays readable, is the
	/// subscription's `max_delay`. Takes over `group`, whose handle is closed afterward; errors
	/// if it has already read a frame.
	#[uniffi::constructor]
	pub fn new(group: &MoqGroupConsumer, container: MoqContainer) -> Result<Arc<Self>, MoqError> {
		// Parse the container first, so a bad init leaves the raw group readable.
		let media = media_container(container, moq_mux::container::Kind::Data)?;
		let (group, driver) = group.take()?;
		Ok(Arc::new(Self::from_inner(group, media, driver)))
	}

	/// Fetch and decode exactly one group of a catalog rendition.
	///
	/// Follows the rendition's broadcast reference and track name, like
	/// [`MoqMediaTrackConsumer::subscribe`].
	#[uniffi::constructor]
	pub async fn fetch(
		broadcast: &MoqBroadcastConsumer,
		config: MoqMediaGroupConfig,
	) -> Result<Arc<MoqMediaGroupConsumer>, MoqError> {
		let MoqMediaGroupConfig {
			rendition,
			sequence,
			options,
		} = config;
		let (reference, epoch, name, container) = rendition.into_parts();
		// Parse the container before fetching so invalid CMAF init data does not leave a
		// dynamic group request waiting for a consumer that can never read it.
		let media = media_container(container, moq_mux::container::Kind::Data)?;
		let resolved = broadcast.resolve_inner(reference.as_deref(), epoch.as_deref()).await?;
		let options = options.map(moq_net::group::Fetch::from);
		let group = broadcast
			.driver
			.watch(async {
				let track = resolved.track(&name).map_err(map_fetch_error)?;
				track.fetch_group(sequence, options).await.map_err(map_fetch_error)
			})
			.await?;
		Ok(Arc::new(MoqMediaGroupConsumer::from_inner(
			group,
			media,
			broadcast.driver.clone(),
		)))
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
		assert!(matches!(catalog.now(), Err(MoqError::Closed)));
	}

	#[test]
	fn catalog_refuses_writes_after_broadcast_close() {
		let broadcast = MoqBroadcastProducer::new().unwrap();
		let catalog = MoqMediaCatalogProducer::new(broadcast.clone()).unwrap();
		broadcast.close().unwrap();
		assert!(matches!(catalog.now(), Err(MoqError::Closed)));
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

#[cfg(all(test, not(target_arch = "wasm32")))]
mod timebase_tests {
	use super::*;
	use std::time::Duration;

	fn source() -> moq_mux::container::fmp4::Muxer {
		let mut config = hang::catalog::VideoConfig::new(hang::catalog::VideoCodec::VP8);
		config.coded_width = Some(16);
		config.coded_height = Some(16);
		moq_mux::container::fmp4::Muxer::video(&config).unwrap()
	}

	fn fragment(source: &moq_mux::container::fmp4::Muxer, seconds: u64, tag: u8) -> bytes::Bytes {
		source
			.fragment(
				0,
				&[moq_mux::container::Frame {
					timestamp: moq_net::Timestamp::from_scale(seconds, 1).unwrap(),
					payload: bytes::Bytes::from(vec![tag]),
					keyframe: true,
					duration: Some(moq_net::Timestamp::from_millis(20).unwrap()),
				}],
				None,
			)
			.unwrap()
	}

	#[tokio::test(start_paused = true)]
	async fn containers_of_one_source_keep_their_relative_media_time() {
		for shared in [false, true] {
			// Construct under the paused runtime, before exported entry points enter their worker.
			let broadcast = Arc::new(
				MoqBroadcastProducer::from_inner(
					moq_net::broadcast::Info::new().produce(),
					crate::ffi::Background::default(),
				)
				.unwrap(),
			);
			let catalog = broadcast.with_state(|state| Ok(state.catalog.clone())).unwrap();
			let clock = catalog.clock();
			let consumer = broadcast.consume_inner().unwrap();
			let source = source();
			let init = source.init().unwrap().unwrap();
			let audio = moq_mux::container::fmp4::Muxer::audio(&hang::catalog::AudioConfig::new(
				hang::catalog::AudioCodec::Opus,
				48_000,
				2,
			))
			.unwrap();
			let audio_init = audio.init().unwrap().unwrap();
			let timebase = shared.then(|| MoqMediaTimebase::new(broadcast.clone()).unwrap());
			let chunks = MoqMediaContainerProducer::new(
				&broadcast,
				MoqContainerInit {
					format: MoqContainerFormat::Fmp4,
					data: init.to_vec(),
					timebase: timebase.clone(),
				},
			)
			.unwrap();
			let audio_chunks = MoqMediaContainerProducer::new(
				&broadcast,
				MoqContainerInit {
					format: MoqContainerFormat::Fmp4,
					data: audio_init.to_vec(),
					timebase,
				},
			)
			.unwrap();
			tokio::time::advance(Duration::from_secs(1)).await;
			let first = clock.now().as_micros();
			// Drive the private importers on the paused clock, without entering the unpaused FFI worker.
			chunks
				.inner
				.lock()
				.unwrap()
				.as_mut()
				.unwrap()
				.import
				.decode(&fragment(&source, 3600, 1))
				.unwrap();
			tokio::time::advance(Duration::from_secs(5)).await;
			audio_chunks
				.inner
				.lock()
				.unwrap()
				.as_mut()
				.unwrap()
				.import
				.decode(&fragment(&audio, 3602, 2))
				.unwrap();
			let mut readers = Vec::new();
			let snapshot = catalog.snapshot();
			let containers =
				snapshot
					.video
					.renditions
					.into_iter()
					.map(|(name, config)| (name, moq_mux::catalog::hang::Container::try_from(&config).unwrap()))
					.chain(
						snapshot.audio.renditions.into_iter().map(|(name, config)| {
							(name, moq_mux::catalog::hang::Container::try_from(&config).unwrap())
						}),
					);
			for (name, container) in containers {
				let track = consumer
					.track(&name)
					.unwrap()
					.subscribe(moq_net::track::Subscription::default().with_max_delay(Duration::from_secs(30)))
					.await
					.unwrap();
				readers.push(moq_mux::container::Consumer::new(track, container));
			}
			chunks.finish().unwrap();
			audio_chunks.finish().unwrap();
			let mut times = HashMap::new();
			for mut media in readers {
				while let Some(read) = media.read().await.unwrap() {
					if let moq_mux::container::Read::Frame { frame, .. } = read {
						times.insert(frame.payload[0], frame.timestamp.as_micros());
					}
				}
			}
			assert_eq!(times[&1], first);
			assert_eq!(
				times[&2],
				first + if shared { 2_000_000 } else { 5_000_000 },
				"shared={shared}"
			);
		}
	}

	#[test]
	fn a_timebase_refuses_foreign_or_closed_broadcasts() {
		let owner = MoqBroadcastProducer::new().unwrap();
		let other = MoqBroadcastProducer::new().unwrap();
		let timebase = MoqMediaTimebase::new(owner.clone()).unwrap();
		assert!(matches!(MoqMediaContainerProducer::new(&other, MoqContainerInit {
			format: MoqContainerFormat::Fmp4, data: Vec::new(), timebase: Some(timebase.clone()),
		}), Err(MoqError::Codec { reason }) if reason.contains("another broadcast")));
		owner.close().unwrap();
		assert!(matches!(MoqMediaTimebase::new(owner.clone()), Err(MoqError::Closed)));
		assert!(matches!(
			MoqMediaContainerProducer::new(
				&owner,
				MoqContainerInit {
					format: MoqContainerFormat::Fmp4,
					data: Vec::new(),
					timebase: Some(timebase),
				}
			),
			Err(MoqError::Closed)
		));
	}

	#[test]
	fn moving_a_rust_handle_still_releases_its_owner_entry() {
		let owner = MoqBroadcastProducer::new().unwrap();
		let handle = MoqMediaTimebase::new(owner.clone()).unwrap();
		let Ok(handle) = Arc::try_unwrap(handle) else {
			panic!("timebase acquired a strong owner")
		};
		drop(handle);
		assert!(owner.with_state(|state| Ok(state.timebases.is_empty())).unwrap());
	}

	#[test]
	fn temporary_timebases_leave_only_the_importers_reservation() {
		let broadcast = MoqBroadcastProducer::new().unwrap();
		let chunks = MoqMediaContainerProducer::new(
			&broadcast,
			MoqContainerInit {
				format: MoqContainerFormat::Fmp4,
				data: Vec::new(),
				timebase: Some(MoqMediaTimebase::new(broadcast.clone()).unwrap()),
			},
		)
		.unwrap();
		assert!(broadcast.with_state(|state| Ok(state.timebases.is_empty())).unwrap());
		chunks.finish().unwrap();
	}

	#[tokio::test(start_paused = true)]
	async fn a_timebase_never_holds_the_publication_open() {
		for close in [false, true] {
			let broadcast = MoqBroadcastProducer::new().unwrap();
			let timebase = MoqMediaTimebase::new(broadcast.clone()).unwrap();
			let track = broadcast
				.consume_inner()
				.unwrap()
				.track(hang::catalog::Catalog::DEFAULT_NAME)
				.unwrap()
				.subscribe(moq_net::track::Subscription::default())
				.await
				.unwrap();
			let mut catalog = moq_mux::catalog::hang::Consumer::<Extra>::new(track);
			assert!(catalog.poll_next(&kio::Waiter::noop()).is_pending());
			let weak = Arc::downgrade(&broadcast);
			if close {
				broadcast.close().unwrap();
			}
			drop(broadcast);
			assert!(weak.upgrade().is_none(), "timebase retained its broadcast handle");
			assert!(
				catalog.poll_next(&kio::Waiter::noop()).is_ready(),
				"timebase retained catalog outputs"
			);
			drop(timebase);
		}
	}

	#[tokio::test(start_paused = true)]
	async fn an_unused_timebase_does_not_withhold_the_catalog() {
		let owner = MoqBroadcastProducer::new().unwrap();
		let _timebase = MoqMediaTimebase::new(owner.clone()).unwrap();
		let track = owner
			.consume_inner()
			.unwrap()
			.track(hang::catalog::Catalog::DEFAULT_NAME)
			.unwrap()
			.subscribe(moq_net::track::Subscription::default())
			.await
			.unwrap();
		let mut reader = moq_mux::catalog::hang::Consumer::<Extra>::new(track);
		let catalog = MoqMediaCatalogProducer::new(owner.clone()).unwrap();
		catalog.set_section("app".into(), "{}".into()).unwrap();
		assert!(matches!(
			reader.poll_next(&kio::Waiter::noop()),
			std::task::Poll::Ready(Ok(Some(_)))
		));
	}

	/// Sweep the owner table and the touched importer count independently.
	#[test]
	#[ignore = "benchmark, runs in Nightly"]
	fn benchmark_timebase_reservation_fanout() {
		eprintln!("timebases,containers,ns_per_container");
		for entries in [1, 32, 1024] {
			let owner = MoqBroadcastProducer::new().unwrap();
			let timebases: Vec<_> = (0..entries)
				.map(|_| MoqMediaTimebase::new(owner.clone()).unwrap())
				.collect();
			let reserve = || {
				let importer = MoqMediaContainerProducer::new(
					&owner,
					MoqContainerInit {
						format: MoqContainerFormat::Fmp4,
						data: Vec::new(),
						timebase: Some(std::hint::black_box(timebases.last().unwrap()).clone()),
					},
				)
				.unwrap();
				importer.finish().unwrap();
			};
			for _ in 0..100 {
				reserve();
			}
			for touched in [1, 16, 256] {
				let start = moq_clock::Instant::now();
				for _ in 0..1000 {
					for _ in 0..touched {
						reserve();
					}
				}
				let nanos = start.elapsed().as_nanos() as f64 / (1000 * touched) as f64;
				eprintln!("{entries},{touched},{nanos:.1}");
			}
			owner.close().unwrap();
		}
	}
}
