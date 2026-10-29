mod av1;
mod codec;
mod color;
mod h264;
mod h265;
mod projection;
mod stereo;
mod vp9;

pub use av1::*;
pub use codec::*;
pub use color::*;
pub use h264::*;
pub use h265::*;
pub use projection::*;
pub use stereo::*;
pub use vp9::*;

use std::collections::{BTreeMap, btree_map};

use bytes::Bytes;
use serde::{Deserialize, Serialize};
use serde_with::DisplayFromStr;

use crate::catalog::Container;
use crate::catalog::hex::Hex;
use crate::catalog::millis::MillisCeil;

/// Information about a video track in the catalog.
///
/// This struct contains a map of renditions (different quality/codec options)
/// and optional metadata like detection, display settings, rotation, and flip.
///
/// Marked `#[non_exhaustive]` so additional optional fields can be added without
/// bumping the major version. External callers start from [`Video::default`] and
/// fill in what they need ([`insert`](Self::insert) for renditions); struct-literal
/// construction (with or without `..base`) is not available outside this crate.
#[serde_with::serde_as]
#[serde_with::skip_serializing_none]
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct Video {
	/// A map of track name to rendition configuration.
	/// This is not an array in order for it to work with JSON Merge Patch.
	/// We use a BTreeMap so keys are sorted alphabetically for *some* deterministic behavior.
	pub renditions: BTreeMap<String, VideoConfig>,

	/// Render the video at this size in pixels.
	/// This is separate from the display aspect ratio because it does not require reinitialization.
	#[serde(default)]
	pub display: Option<Display>,

	/// The clockwise rotation of the video in degrees.
	/// Default: 0
	#[serde(default)]
	pub rotation: Option<f64>,

	/// If true, the decoder will flip the video horizontally
	/// Default: false
	#[serde(default)]
	pub flip: Option<bool>,
}

/// Catalog properties shared by every video rendition.
#[derive(Debug, Clone, PartialEq, Default)]
#[non_exhaustive]
pub struct VideoProperties {
	/// Render the video at this final size after rotation, or clear the explicit size when absent.
	pub display: Option<Display>,

	/// Apply this clockwise rotation before rendering, or clear it when absent. Values are normalized to the nearest quarter turn.
	pub rotation: Option<f64>,

	/// Flip horizontally after rotation, or clear the explicit value when absent.
	pub flip: Option<bool>,
}

impl VideoProperties {
	fn normalized(mut self) -> crate::Result<Self> {
		self.rotation = self.rotation.map(normalize_video_rotation).transpose()?;
		Ok(self)
	}
}

const FULL_TURN_DEGREES: f64 = 360.0;
const QUARTER_TURN_DEGREES: f64 = 90.0;
const QUARTER_TURNS_PER_FULL_TURN: u16 = 4;

fn normalize_video_rotation(rotation: f64) -> crate::Result<f64> {
	if !rotation.is_finite() {
		return Err(crate::Error::InvalidVideoRotation);
	}

	let normalized = rotation.rem_euclid(FULL_TURN_DEGREES);
	let quarter_turns = (normalized / QUARTER_TURN_DEGREES).round() as u16 % QUARTER_TURNS_PER_FULL_TURN;
	Ok(quarter_turns as f64 * QUARTER_TURN_DEGREES)
}

impl Video {
	/// Insert a track config, returning an error if the name already exists.
	pub fn insert(&mut self, name: &str, config: VideoConfig) -> crate::Result<()> {
		let btree_map::Entry::Vacant(entry) = self.renditions.entry(name.to_string()) else {
			return Err(crate::Error::Duplicate(name.to_string()));
		};
		entry.insert(config);
		Ok(())
	}

	/// Remove the track from the catalog and return the configuration if found.
	pub fn remove(&mut self, name: &str) -> Option<VideoConfig> {
		self.renditions.remove(name)
	}

	/// True when there are no renditions, so the section can be omitted from the catalog.
	///
	/// Display, rotation, and flip ride this section, so they are omitted too when nothing is published.
	pub fn is_empty(&self) -> bool {
		self.renditions.is_empty()
	}

	/// Iterate enabled renditions in quality order.
	pub fn enabled(&self) -> impl Iterator<Item = (&String, &VideoConfig)> {
		self.ranked().filter(|(_, config)| config.enabled)
	}

	/// Iterate the renditions best first: enabled, then highest [preference](VideoConfig::preference),
	/// then largest picture, then highest bitrate.
	///
	/// A consumer that carries one rendition takes the first it supports, so the
	/// picture doesn't depend on how the tracks are named, and a lower preference
	/// is taken only when nothing preferred is supported. Unknown dimensions or
	/// bitrate rank below known ones, and exact ties keep name order.
	pub fn ranked(&self) -> impl Iterator<Item = (&String, &VideoConfig)> {
		let mut ranked: Vec<_> = self.renditions.iter().collect();
		ranked.sort_by_key(|(_, config)| {
			let area = u64::from(config.coded_width.unwrap_or(0)) * u64::from(config.coded_height.unwrap_or(0));
			std::cmp::Reverse((config.enabled, config.preference, area, config.bitrate))
		});
		ranked.into_iter()
	}

	/// Normalize and replace the properties shared by every video rendition.
	pub fn set_properties(&mut self, properties: VideoProperties) -> crate::Result<()> {
		let properties = properties.normalized()?;
		self.display = properties.display;
		self.rotation = properties.rotation;
		self.flip = properties.flip;
		Ok(())
	}
}

/// Display size for rendering video
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Display {
	/// Width in pixels.
	pub width: u32,
	/// Height in pixels.
	pub height: u32,
}

/// Video decoder configuration based on WebCodecs VideoDecoderConfig.
///
/// This struct contains all the information needed to initialize a video decoder,
/// including codec-specific parameters, resolution, and optional metadata.
///
/// Reference: <https://www.w3.org/TR/webcodecs/#video-decoder-config>
///
/// Marked `#[non_exhaustive]` so additional optional fields can be added
/// without bumping the major version. External callers build a config with
/// [`VideoConfig::new`] and then assign whichever optional fields they need;
/// struct-literal construction (with or without `..base`) is not available
/// outside this crate.
#[serde_with::serde_as]
#[serde_with::skip_serializing_none]
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct VideoConfig {
	/// Optional reference to another broadcast that publishes this track, expressed
	/// relative to the broadcast that served this catalog (e.g. `./source`). If unset,
	/// the track lives in the same broadcast as the catalog.
	///
	/// This allows a transcoder to author a downstream catalog that points unchanged
	/// renditions at the source broadcast without re-publishing the bytes.
	///
	/// Resolve it with [`Path::resolve`](moq_net::Path::resolve): a reference that walks
	/// above the root names no broadcast, so the catalog is rejected.
	#[serde(default)]
	pub broadcast: Option<moq_net::path::Relative>,

	/// The track's name in its broadcast, when it differs from this entry's ID: its key in the
	/// catalog. Absent means the ID is the track name, as in every catalog before this field.
	///
	/// Set it only when the ID can't be the name, such as two renditions of same-named tracks
	/// from different broadcasts. Readers resolve the name with
	/// [`Reference::track`](crate::catalog::Reference::track); one that predates the
	/// field subscribes to the ID instead.
	#[serde(default)]
	pub track: Option<String>,

	/// The publisher instance the reference names, when the catalog knows it. Absent names
	/// whichever instance the reader resolves first, as in every catalog before this field.
	///
	/// A reader on an epoch-aware route refuses another instance rather than fall back to it;
	/// on an epochless route it is best effort, since the route cannot prove the instance.
	/// Without [`broadcast`](Self::broadcast), it names the catalog's own instance.
	#[serde(default)]
	#[serde_as(as = "Option<serde_with::DisplayFromStr>")]
	pub epoch: Option<moq_net::Epoch>,

	/// Human-readable rendition name for track pickers.
	#[serde(default)]
	pub label: Option<String>,

	/// The codec, see the registry for details:
	/// <https://w3c.github.io/webcodecs/codec_registry.html>
	#[serde_as(as = "DisplayFromStr")]
	pub codec: VideoCodec,

	/// Information used to initialize the decoder on a per-codec basis.
	///
	/// One of the best examples is H264, which needs the sps/pps to function.
	/// If not provided, this information is (automatically) inserted before each key-frame (marginally higher overhead).
	#[serde(default)]
	#[serde_as(as = "Option<Hex>")]
	pub description: Option<Bytes>,

	/// The largest encoded width/height the track carries, fixed for its name.
	///
	/// The picture resizes in band at a keyframe below these ceilings, unless the
	/// `description` pins it. Absent means unknown; a publisher may fill it once.
	pub coded_width: Option<u32>,
	/// The height counterpart of `coded_width`.
	pub coded_height: Option<u32>,

	/// The aspect ratio the whole picture is displayed at, as WebCodecs'
	/// `VideoDecoderConfig.displayAspectWidth` / `displayAspectHeight`.
	///
	/// Absent means the decoded picture's own shape. A publisher with non-square
	/// samples sets the picture's aspect here, never the sample aspect.
	///
	/// The `displayRatio*` aliases decode catalogs from publishers predating the
	/// rename to `displayAspect*`; the current name is what we emit.
	#[serde(alias = "displayRatioWidth")]
	pub display_aspect_width: Option<u32>,
	/// The height counterpart of `display_aspect_width`.
	#[serde(alias = "displayRatioHeight")]
	pub display_aspect_height: Option<u32>,

	/// The colour the decoded pixels carry, so a renderer need not guess or reparse the bitstream.
	///
	/// Live state, not identity: a publisher may fill it in once the bitstream reveals it.
	#[serde(default)]
	pub color: Option<Color>,

	/// How the picture maps onto the viewer's field of view. Absent means rectilinear.
	///
	/// Live state, like [`color`](Self::color).
	#[serde(default)]
	pub projection: Option<Projection>,

	/// How each eye's view is packed into the frame. Absent means mono.
	///
	/// Live state, like [`color`](Self::color).
	#[serde(default)]
	#[serde_as(as = "Option<DisplayFromStr>")]
	pub stereo: Option<Stereo>,

	/// Each picture weaves two fields, so a renderer deinterlaces it. Only written when true.
	///
	/// Decides decoder configuration, since some hardware decoders refuse interlaced streams.
	#[serde(default, skip_serializing_if = "std::ops::Not::not")]
	pub interlaced: bool,

	/// The maximum bitrate of the video track, if known.
	#[serde(default)]
	pub bitrate: Option<u64>,

	/// Whether this rendition may be selected. When false, no frames are coming and a consumer
	/// must not select it. Only written when false.
	#[serde(
		default = "crate::catalog::enabled_default",
		skip_serializing_if = "crate::catalog::enabled_skip"
	)]
	pub enabled: bool,

	/// Higher is preferred: a consumer selects among the highest preference it can decode,
	/// then by picture and bitrate within it. A compatibility transcode is `-1` so only a
	/// consumer that can't decode the source pulls it. Only written when nonzero.
	#[serde(default, skip_serializing_if = "is_zero")]
	pub preference: i32,

	/// The maximum frame rate of the video track, if known.
	#[serde(default)]
	pub framerate: Option<f64>,

	/// The most frames that can precede a frame in decode order and follow it in presentation
	/// order, as H.264 `max_num_reorder_frames` and HEVC `sps_max_num_reorder_pics` count them.
	///
	/// `0` declares that presentation order is decode order. Absent declares nothing: a consumer
	/// that authors decode times infers them from the timestamps instead. A publisher that
	/// declares it keeps it for the track's life, and never reorders deeper.
	#[serde(default)]
	pub reorder: Option<u8>,

	/// If true, the decoder will optimize for latency.
	///
	/// Default: true
	#[serde(default)]
	pub optimize_for_latency: Option<bool>,

	/// Container format for frame encoding.
	/// Defaults to "legacy" for backward compatibility.
	#[serde(default)]
	pub container: Container,

	/// The maximum delay between a frame being ready and the publisher flushing it.
	/// The player's jitter buffer should be larger than this value.
	/// If not provided, the player should assume each frame is flushed immediately.
	///
	/// This is measured at the publisher (encoder latency, segment size, B-frame
	/// reordering), never on the network a consumer sees. It only ever grows over the life of a
	/// stream.
	///
	/// Serialized as a whole number of milliseconds, rounded up, so an upper bound never
	/// rounds down into a promise the publisher can't keep.
	///
	/// ex:
	/// - If each frame is flushed immediately, this would be 1000/fps.
	/// - If there can be up to 3 b-frames in a row, this would be 3 * 1000/fps.
	/// - If frames are buffered into 2s segments, this would be 2s.
	#[serde_as(as = "MillisCeil")]
	#[serde(default)]
	pub jitter: Option<std::time::Duration>,

	/// After a non-continuous join, decode from the group start, present at start plus warmup, and join that much earlier.
	/// Serialized as whole milliseconds, rounded up so presentation never starts early.
	#[serde_as(as = "Option<MillisCeil>")]
	#[serde(default)]
	pub warmup: Option<std::time::Duration>,

	/// How far this rendition's frames reach the transport behind the broadcast's earliest
	/// rendition, measured at the publisher from each rendition's minimum flush lateness.
	/// Absent on the earliest rendition and on any rendition the publisher did not measure.
	///
	/// A consumer holds `delay + jitter` for this rendition and MUST NOT subtract one rendition's
	/// `delay` from another's: each is a lifetime maximum, so two need not share an origin. It only
	/// ever grows over the life of a stream, and is serialized like [`jitter`](Self::jitter).
	#[serde_as(as = "MillisCeil")]
	#[serde(default)]
	pub delay: Option<std::time::Duration>,
	/// This rendition's HLS segment records, when the publisher opted in. Without it the rendition
	/// is not HLS-renderable. An undecodable value is absent, so it can never reject the media.
	#[serde(default)]
	#[serde_as(as = "serde_with::DefaultOnError<_>")]
	pub hls: Option<crate::catalog::Hls>,
}

impl VideoConfig {
	/// Construct a config with the required codec set and every optional
	/// field cleared. `container` defaults to [`Container::default`]. Fields
	/// are `pub`, so callers set whatever they need by assignment afterwards.
	///
	/// This is the only path external crates have to build a `VideoConfig`
	/// since the type is `#[non_exhaustive]`.
	pub fn new(codec: impl Into<VideoCodec>) -> Self {
		Self {
			broadcast: None,
			track: None,
			epoch: None,
			label: None,
			codec: codec.into(),
			description: None,
			coded_width: None,
			coded_height: None,
			display_aspect_width: None,
			display_aspect_height: None,
			color: None,
			projection: None,
			stereo: None,
			interlaced: false,
			bitrate: None,
			enabled: true,
			preference: 0,
			framerate: None,
			reorder: None,
			optimize_for_latency: None,
			container: Container::default(),
			jitter: None,
			warmup: None,
			delay: None,
			hls: None,
		}
	}

	/// Whether `other` keeps this config's track identity: the fields a consumer cannot change
	/// without reinitializing its decoder or demuxer (`codec`, `description`, `container`,
	/// `display_aspect_width`, `display_aspect_height`, and `interlaced`).
	///
	/// A track name always means the same content, so a publisher that changes any of them
	/// publishes under a new name; every other field may update in place. See the draft's
	/// [Track Identity](https://github.com/moq-dev/moq/blob/main/drafts/draft-lcurley-moq-hang.md#identity).
	pub fn same_identity(&self, other: &Self) -> bool {
		self.codec == other.codec
			&& self.description == other.description
			&& self.container == other.container
			&& self.display_aspect_width == other.display_aspect_width
			&& self.display_aspect_height == other.display_aspect_height
			&& self.interlaced == other.interlaced
	}
}

fn is_zero(value: &i32) -> bool {
	*value == 0
}

#[cfg(test)]
mod test {
	use crate::catalog::{Container, H264};

	use super::*;

	#[test]
	fn warmup_round_trips() {
		let mut config = VideoConfig::new(VideoCodec::VP8);
		assert!(serde_json::to_value(&config).unwrap().get("warmup").is_none());
		for millis in [0, 80, 1_000] {
			config.warmup = Some(std::time::Duration::from_millis(millis));
			let encoded = serde_json::to_value(&config).unwrap();
			assert_eq!(encoded["warmup"], millis);
			let decoded: VideoConfig = serde_json::from_value(encoded).unwrap();
			assert_eq!(decoded.warmup, config.warmup);
		}
	}

	#[test]
	fn reorder_keeps_zero_distinct_from_absent() {
		let mut config = VideoConfig::new(VideoCodec::VP8);
		assert!(serde_json::to_value(&config).unwrap().get("reorder").is_none());
		for depth in [0, 2] {
			config.reorder = Some(depth);
			let encoded = serde_json::to_value(&config).unwrap();
			assert_eq!(encoded["reorder"], depth);
			let decoded: VideoConfig = serde_json::from_value(encoded).unwrap();
			assert_eq!(decoded.reorder, Some(depth));
		}
	}

	#[test]
	fn warmup_rounds_up() {
		let mut config = VideoConfig::new(VideoCodec::VP8);
		for (nanos, millis) in [(1, 1), (499_999, 1), (333_200_000, 334)] {
			config.warmup = Some(std::time::Duration::from_nanos(nanos));
			let encoded = serde_json::to_value(&config).unwrap();
			assert_eq!(encoded["warmup"], millis);
			let decoded: VideoConfig = serde_json::from_value(encoded).unwrap();
			assert_eq!(decoded.warmup, Some(std::time::Duration::from_millis(millis)));
		}
	}

	#[test]
	fn ranked_orders_by_picture_then_bitrate() {
		fn rendition(size: Option<(u32, u32)>, bitrate: Option<u64>) -> VideoConfig {
			let mut config = VideoConfig::new(VideoCodec::VP8);
			config.coded_width = size.map(|(w, _)| w);
			config.coded_height = size.map(|(_, h)| h);
			config.bitrate = bitrate;
			config
		}

		let mut video = Video::default();
		// Names sort worst first, so name order alone would pick the wrong one.
		video.insert("a", rendition(None, Some(9_000_000))).unwrap();
		video.insert("b", rendition(Some((640, 360)), Some(1_000_000))).unwrap();
		video.insert("c", rendition(Some((1280, 720)), None)).unwrap();
		video
			.insert("d", rendition(Some((1280, 720)), Some(3_000_000)))
			.unwrap();
		video
			.insert("e", rendition(Some((1280, 720)), Some(3_000_000)))
			.unwrap();
		video
			.insert("f", rendition(Some((1920, 1080)), Some(6_000_000)))
			.unwrap();

		let names: Vec<_> = video.ranked().map(|(name, _)| name.as_str()).collect();
		assert_eq!(names, ["f", "d", "e", "c", "b", "a"]);

		// Disabled renditions rank last, so catalog transforms can retain their metadata.
		video.renditions.get_mut("f").unwrap().enabled = false;
		let names: Vec<_> = video.ranked().map(|(name, _)| name.as_str()).collect();
		assert_eq!(names, ["d", "e", "c", "b", "a", "f"]);
		let names: Vec<_> = video.enabled().map(|(name, _)| name.as_str()).collect();
		assert_eq!(names, ["d", "e", "c", "b", "a"]);
	}

	#[test]
	fn disabled_renditions_are_ineligible_until_reenabled() {
		let mut catalog = Video::default();
		let config = VideoConfig::new(VideoCodec::VP8);
		catalog.renditions.insert("track".into(), config);
		assert_eq!(catalog.enabled().count(), 1);
		catalog.renditions.get_mut("track").unwrap().enabled = false;
		assert_eq!(catalog.enabled().count(), 0);
		assert_eq!(catalog.ranked().count(), 1);
		catalog.renditions.get_mut("track").unwrap().enabled = true;
		assert_eq!(catalog.enabled().count(), 1);
	}

	#[test]
	fn display_aspect_uses_canonical_json_names() {
		let mut config = VideoConfig::new(H264 {
			profile: 0x64,
			constraints: 0,
			level: 0x1f,
			inline: false,
		});
		config.display_aspect_width = Some(4);
		config.display_aspect_height = Some(3);
		config.container = Container::Legacy;

		let encoded = serde_json::to_value(config).expect("failed to encode");
		assert_eq!(encoded["displayAspectWidth"], 4);
		assert_eq!(encoded["displayAspectHeight"], 3);
		assert!(encoded.get("displayRatioWidth").is_none());
		assert!(encoded.get("displayRatioHeight").is_none());
	}

	#[test]
	fn decodes_legacy_display_ratio_keys() {
		// A catalog serialized by a pre-0.20 publisher used displayRatio*; the
		// alias keeps the aspect ratio from being silently dropped.
		let json = serde_json::json!({
			"codec": "avc1.640028",
			"displayRatioWidth": 16,
			"displayRatioHeight": 9,
		});
		let config: VideoConfig = serde_json::from_value(json).expect("failed to decode legacy keys");
		assert_eq!(config.display_aspect_width, Some(16));
		assert_eq!(config.display_aspect_height, Some(9));
	}

	#[test]
	fn enabled_is_written_only_when_false() {
		let mut config = VideoConfig::new(VideoCodec::VP8);
		let encoded = serde_json::to_value(&config).expect("failed to encode");
		assert!(encoded.get("enabled").is_none());

		config.enabled = false;
		let encoded = serde_json::to_value(&config).expect("failed to encode");
		assert_eq!(encoded["enabled"], false);

		let decoded: VideoConfig = serde_json::from_value(encoded).expect("failed to decode");
		assert!(!decoded.enabled);
	}

	#[test]
	fn interlaced_is_written_only_when_true() {
		let mut config = VideoConfig::new(VideoCodec::VP8);
		let encoded = serde_json::to_value(&config).expect("failed to encode");
		assert!(encoded.get("interlaced").is_none());

		config.interlaced = true;
		let encoded = serde_json::to_value(&config).expect("failed to encode");
		assert_eq!(encoded["interlaced"], true);

		let decoded: VideoConfig = serde_json::from_value(encoded).expect("failed to decode");
		assert!(decoded.interlaced);
	}

	#[test]
	fn preference_is_written_only_when_nonzero() {
		let mut config = VideoConfig::new(VideoCodec::VP8);
		let encoded = serde_json::to_value(&config).expect("failed to encode");
		assert!(encoded.get("preference").is_none());

		config.preference = -1;
		let encoded = serde_json::to_value(&config).expect("failed to encode");
		assert_eq!(encoded["preference"], -1);

		let decoded: VideoConfig = serde_json::from_value(encoded).expect("failed to decode");
		assert_eq!(decoded.preference, -1);
	}

	#[test]
	fn ranked_orders_by_preference_before_picture() {
		let mut source = VideoConfig::new(VideoCodec::VP8);
		source.coded_width = Some(640);
		source.coded_height = Some(360);
		source.bitrate = Some(500_000);

		let mut fallback = VideoConfig::new(VideoCodec::VP8);
		fallback.coded_width = Some(1920);
		fallback.coded_height = Some(1080);
		fallback.bitrate = Some(6_000_000);
		fallback.preference = -1;

		let mut video = Video::default();
		video.insert("a", fallback).unwrap();
		video.insert("b", source).unwrap();

		let names: Vec<_> = video.ranked().map(|(name, _)| name.as_str()).collect();
		assert_eq!(names, ["b", "a"]);

		// A disabled source ranks last whatever its preference.
		video.renditions.get_mut("b").unwrap().enabled = false;
		let names: Vec<_> = video.ranked().map(|(name, _)| name.as_str()).collect();
		assert_eq!(names, ["a", "b"]);
	}

	#[test]
	fn legacy_stalled_is_ignored() {
		let json = serde_json::json!({ "codec": "vp8", "stalled": true });
		let config: VideoConfig = serde_json::from_value(json).expect("failed to decode");
		assert!(config.enabled);
		assert!(serde_json::to_value(&config).unwrap().get("stalled").is_none());
	}

	#[test]
	fn normalizes_video_rotation_to_quarter_turns() {
		for (rotation, expected) in [
			(0.0, 0.0),
			(44.9, 0.0),
			(45.0, 90.0),
			(134.9, 90.0),
			(135.0, 180.0),
			(225.0, 270.0),
			(315.0, 0.0),
			(360.0, 0.0),
			(-45.0, 0.0),
		] {
			assert_eq!(normalize_video_rotation(rotation).unwrap(), expected);
		}
	}

	#[test]
	fn rejects_non_finite_video_rotation() {
		for rotation in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
			assert!(matches!(
				normalize_video_rotation(rotation),
				Err(crate::Error::InvalidVideoRotation)
			));
		}
	}

	#[test]
	fn invalid_rotation_does_not_replace_video_properties() {
		let mut video = Video {
			display: Some(Display {
				width: 640,
				height: 480,
			}),
			rotation: Some(90.0),
			flip: Some(true),
			..Default::default()
		};
		let expected = video.clone();

		assert!(matches!(
			video.set_properties(VideoProperties {
				display: None,
				rotation: Some(f64::NAN),
				flip: None,
			}),
			Err(crate::Error::InvalidVideoRotation)
		));
		assert_eq!(video, expected);
	}

	#[test]
	fn video_properties_replace_shared_fields_without_touching_renditions() {
		let mut video = Video::default();
		video
			.insert(
				"video",
				VideoConfig::new(H264 {
					profile: 0x64,
					constraints: 0,
					level: 0x1f,
					inline: false,
				}),
			)
			.unwrap();
		video.display = Some(Display {
			width: 640,
			height: 480,
		});
		video.rotation = Some(90.0);
		video.flip = Some(true);
		let renditions = video.renditions.clone();

		video
			.set_properties(VideoProperties {
				display: Some(Display {
					width: 1920,
					height: 1080,
				}),
				rotation: Some(315.0),
				flip: None,
			})
			.unwrap();

		assert_eq!(video.renditions, renditions);
		assert_eq!(
			video.display,
			Some(Display {
				width: 1920,
				height: 1080
			})
		);
		assert_eq!(video.rotation, Some(0.0));
		assert_eq!(video.flip, None);
	}
}
