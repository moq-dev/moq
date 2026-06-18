use std::ops::{Deref, DerefMut};
use std::sync::{Arc, Mutex, MutexGuard};

use base64::Engine;

use super::hang::{Catalog, CatalogExt, Consumer};

/// Produces both a hang and MSF catalog track for a broadcast.
///
/// Generic over the application extension `E` (defaulting to `()` for none). The catalog is a
/// [`Catalog<E>`](super::hang::Catalog): `video`/`audio` are direct fields (`catalog.video`) and the
/// extension is reachable directly via deref (`catalog.scte35`) or as `catalog.ext`. Define an
/// extension with [`CatalogExt`](super::hang::CatalogExt). The MSF track is always derived from the base
/// media sections, regardless of any extension.
///
/// The JSON catalog is updated when tracks are added/removed but is *not* automatically published.
/// You'll have to call [`lock`](Self::lock) to update and publish the catalog.
/// Both the hang (`catalog.json`) and MSF (`catalog`) tracks are published on drop of the guard.
///
/// The hang track is published through [`moq_json`], which currently emits one snapshot per
/// group (deltas disabled). This routes catalog publishing through the JSON merge-patch helper
/// so deltas can be enabled later without changing the wire format used today.
pub struct Producer<E: CatalogExt = ()> {
	hang: moq_json::Producer<Catalog<E>>,
	msf_track: moq_net::TrackProducer,

	current: Arc<Mutex<Catalog<E>>>,

	/// Shared wall clock for the broadcast's tracks. Every importer on this catalog
	/// gets a clone (a `Copy` of the same epoch), so timestamps they synthesize when
	/// a caller has none land on one timeline and audio/video stay in sync.
	clock: crate::Clock,
}

// Manual Clone so a producer is cheaply clonable regardless of whether `E` is.
impl<E: CatalogExt> Clone for Producer<E> {
	fn clone(&self) -> Self {
		Self {
			hang: self.hang.clone(),
			msf_track: self.msf_track.clone(),
			current: self.current.clone(),
			clock: self.clock,
		}
	}
}

impl Producer<()> {
	/// Create a new catalog producer with the default (empty) catalog.
	///
	/// To publish an extended catalog, use [`with_catalog`](Self::with_catalog) with a `Catalog<E>`.
	pub fn new(broadcast: &mut moq_net::BroadcastProducer) -> Result<Self, moq_net::Error> {
		Self::with_catalog(broadcast, Catalog::default())
	}
}

impl<E: CatalogExt> Producer<E> {
	/// Create a new catalog producer with the given initial catalog.
	pub fn with_catalog(
		broadcast: &mut moq_net::BroadcastProducer,
		catalog: Catalog<E>,
	) -> Result<Self, moq_net::Error> {
		let hang_track = broadcast.create_track(hang::Catalog::DEFAULT_NAME, hang::Catalog::default_track_info())?;
		let msf_track = broadcast.create_track(moq_msf::DEFAULT_NAME, None)?;

		// Disable deltas for now to stay byte-compatible with consumers that only read snapshots.
		let hang = moq_json::Producer::new(hang_track, moq_json::Config { delta_ratio: 0 });

		Ok(Self {
			hang,
			msf_track,
			current: Arc::new(Mutex::new(catalog)),
			clock: crate::Clock::new(),
		})
	}

	/// Resolve a timestamp, synthesizing one from the broadcast's shared
	/// [`Clock`](crate::Clock) when the caller has none.
	///
	/// Sharing the clock across the catalog's tracks keeps concurrently-produced
	/// audio and video on a single timeline.
	pub fn timestamp(&self, hint: Option<moq_net::Timestamp>) -> crate::Result<moq_net::Timestamp> {
		match hint {
			Some(pts) => Ok(pts),
			None => Ok(moq_net::Timestamp::from_micros(self.clock.micros())?),
		}
	}

	/// Get mutable access to the catalog, publishing it after any changes.
	pub fn lock(&mut self) -> Guard<'_, E> {
		Guard {
			catalog: self.current.lock().unwrap(),
			hang: &mut self.hang,
			msf_track: &mut self.msf_track,
			updated: false,
		}
	}

	/// Get a snapshot of the current catalog.
	pub fn snapshot(&self) -> Catalog<E> {
		self.current.lock().unwrap().clone()
	}

	/// A handle for one importer to publish a video rendition, retired on drop.
	///
	/// See [`VideoTrack`](super::VideoTrack).
	pub fn video_track(&self, name: impl Into<String>) -> super::VideoTrack<E> {
		super::VideoTrack::new(self.clone(), name)
	}

	/// A handle for one importer to publish an audio rendition, retired on drop.
	///
	/// See [`AudioTrack`](super::AudioTrack).
	pub fn audio_track(&self, name: impl Into<String>) -> super::AudioTrack<E> {
		super::AudioTrack::new(self.clone(), name)
	}

	/// Create a consumer for this catalog, receiving updates as they're published.
	pub fn consume(&self) -> Result<Consumer<E>, moq_net::Error> {
		Ok(Consumer::new(self.hang.consume()))
	}

	/// Finish publishing to this catalog.
	pub fn finish(&mut self) -> crate::Result<()> {
		self.hang.finish()?;
		self.msf_track.finish()?;
		Ok(())
	}
}

/// RAII guard for modifying a catalog with automatic publishing on drop.
///
/// Obtained via [`Producer::lock`]. Derefs to the [`Catalog<E>`](super::hang::Catalog), so `video`/`audio`
/// and (through the catalog's own deref) the extension sections are editable directly.
///
/// On drop, both the hang and MSF catalog tracks are updated if the catalog was mutated.
pub struct Guard<'a, E: CatalogExt = ()> {
	catalog: MutexGuard<'a, Catalog<E>>,
	hang: &'a mut moq_json::Producer<Catalog<E>>,
	msf_track: &'a mut moq_net::TrackProducer,
	updated: bool,
}

impl<E: CatalogExt> Deref for Guard<'_, E> {
	type Target = Catalog<E>;

	fn deref(&self) -> &Self::Target {
		&self.catalog
	}
}

impl<E: CatalogExt> DerefMut for Guard<'_, E> {
	fn deref_mut(&mut self) -> &mut Self::Target {
		self.updated = true;
		&mut self.catalog
	}
}

impl<E: CatalogExt> Drop for Guard<'_, E> {
	fn drop(&mut self) {
		if !self.updated {
			return;
		}

		// Publish the hang catalog (one snapshot per group while deltas are disabled).
		let catalog: &Catalog<E> = &self.catalog;
		let _ = self.hang.update(catalog);

		// Publish the MSF catalog, derived from the base media sections.
		let msf = to_msf(&self.catalog.media());
		if let Ok(mut group) = self.msf_track.append_group() {
			let _ = group.write_frame(msf.to_string().expect("invalid MSF catalog"));
			let _ = group.finish();
		}
	}
}

/// Determine the SAP starting type for a given video codec.
///
/// SAP type 1: closed GOP with no leading pictures (IDR at every SAP).
/// Used for VP8, which has no B-frames.
///
/// SAP type 2: closed GOP with possible leading pictures. Used for codecs
/// that can carry B-frames (H.264, AV1, VP9) since the encoder may emit
/// leading B-frames after an IDR.
///
/// Returns None for unknown codecs (and H.265, which we don't yet validate
/// the SAP behavior of) so the field is omitted from the catalog.
fn video_sap_type(codec: &hang::catalog::VideoCodec) -> Option<u8> {
	use hang::catalog::VideoCodec;
	match codec {
		VideoCodec::VP8 => Some(1),
		VideoCodec::H264(_) | VideoCodec::AV1(_) | VideoCodec::VP9(_) => Some(2),
		_ => None,
	}
}

/// Convert a hang catalog to an MSF catalog.
fn to_msf(catalog: &hang::Catalog) -> moq_msf::Catalog {
	let mut tracks = Vec::new();

	let has_multiple_video = catalog.video.renditions.len() > 1;
	for (name, config) in &catalog.video.renditions {
		let packaging = match &config.container {
			hang::catalog::Container::Cmaf { .. } => moq_msf::Packaging::Cmaf,
			_ => moq_msf::Packaging::Legacy,
		};

		let init_data = match &config.container {
			hang::catalog::Container::Cmaf { init, .. } => Some(base64::engine::general_purpose::STANDARD.encode(init)),
			_ => config
				.description
				.as_ref()
				.map(|d| base64::engine::general_purpose::STANDARD.encode(d.as_ref())),
		};

		let sap_type = video_sap_type(&config.codec);
		let mut track = moq_msf::Track::new(name.clone(), packaging);
		track.is_live = true;
		track.role = Some(moq_msf::Role::Video);
		track.codec = Some(config.codec.to_string());
		track.width = config.coded_width;
		track.height = config.coded_height;
		track.framerate = config.framerate;
		track.bitrate = config.bitrate;
		track.init_data = init_data;
		track.render_group = Some(1);
		track.alt_group = if has_multiple_video { Some(1) } else { None };
		track.max_grp_sap_starting_type = sap_type;
		track.max_obj_sap_starting_type = sap_type;
		track.jitter = config.jitter;
		tracks.push(track);
	}

	let has_multiple_audio = catalog.audio.renditions.len() > 1;
	for (name, config) in &catalog.audio.renditions {
		let packaging = match &config.container {
			hang::catalog::Container::Cmaf { .. } => moq_msf::Packaging::Cmaf,
			_ => moq_msf::Packaging::Legacy,
		};

		let init_data = match &config.container {
			hang::catalog::Container::Cmaf { init, .. } => Some(base64::engine::general_purpose::STANDARD.encode(init)),
			_ => config
				.description
				.as_ref()
				.map(|d| base64::engine::general_purpose::STANDARD.encode(d.as_ref())),
		};

		let mut track = moq_msf::Track::new(name.clone(), packaging);
		track.is_live = true;
		track.role = Some(moq_msf::Role::Audio);
		track.codec = Some(config.codec.to_string());
		track.samplerate = Some(config.sample_rate);
		track.channel_config = Some(config.channel_count.to_string());
		track.bitrate = config.bitrate;
		track.init_data = init_data;
		track.render_group = Some(1);
		track.alt_group = if has_multiple_audio { Some(1) } else { None };
		track.max_grp_sap_starting_type = Some(1);
		track.max_obj_sap_starting_type = Some(1);
		track.jitter = config.jitter;
		tracks.push(track);
	}

	moq_msf::Catalog { version: 1, tracks }
}

#[cfg(test)]
mod test {
	use std::collections::BTreeMap;

	use bytes::Bytes;
	use hang::catalog::{Audio, AudioCodec, AudioConfig, Container, H264, Video, VideoConfig};

	use super::*;

	#[test]
	fn convert_simple() {
		let mut video_config = VideoConfig::new(H264 {
			profile: 0x64,
			constraints: 0x00,
			level: 0x1f,
			inline: true,
		});
		video_config.coded_width = Some(1280);
		video_config.coded_height = Some(720);
		video_config.bitrate = Some(6_000_000);
		video_config.framerate = Some(30.0);
		video_config.container = Container::Legacy;

		let mut video_renditions = BTreeMap::new();
		video_renditions.insert("video0.avc3".to_string(), video_config);

		let mut audio_config = AudioConfig::new(AudioCodec::Opus, 48_000, 2);
		audio_config.bitrate = Some(128_000);
		audio_config.container = Container::Legacy;

		let mut audio_renditions = BTreeMap::new();
		audio_renditions.insert("audio0".to_string(), audio_config);

		let catalog = hang::Catalog {
			video: Video {
				renditions: video_renditions,
				display: None,
				rotation: None,
				flip: None,
			},
			audio: Audio {
				renditions: audio_renditions,
			},
		};

		let msf = to_msf(&catalog);

		assert_eq!(msf.version, 1);
		assert_eq!(msf.tracks.len(), 2);

		let video = &msf.tracks[0];
		assert_eq!(video.name, "video0.avc3");
		assert_eq!(video.role, Some(moq_msf::Role::Video));
		assert_eq!(video.packaging, moq_msf::Packaging::Legacy);
		assert_eq!(video.codec, Some("avc3.64001f".to_string()));
		assert_eq!(video.width, Some(1280));
		assert_eq!(video.height, Some(720));
		assert_eq!(video.framerate, Some(30.0));
		assert_eq!(video.bitrate, Some(6_000_000));
		assert!(video.init_data.is_none());
		// H.264 may carry B-frames, so SAP starting type is 2 (leading pictures allowed).
		assert_eq!(video.max_grp_sap_starting_type, Some(2));
		assert_eq!(video.max_obj_sap_starting_type, Some(2));
		assert_eq!(video.jitter, None);

		let audio = &msf.tracks[1];
		assert_eq!(audio.name, "audio0");
		assert_eq!(audio.role, Some(moq_msf::Role::Audio));
		assert_eq!(audio.packaging, moq_msf::Packaging::Legacy);
		assert_eq!(audio.codec, Some("opus".to_string()));
		assert_eq!(audio.samplerate, Some(48_000));
		assert_eq!(audio.channel_config, Some("2".to_string()));
		assert_eq!(audio.bitrate, Some(128_000));
		assert_eq!(audio.max_grp_sap_starting_type, Some(1));
		assert_eq!(audio.max_obj_sap_starting_type, Some(1));
		assert_eq!(audio.jitter, None);
	}

	#[test]
	fn convert_with_description() {
		let mut video_config = VideoConfig::new(H264 {
			profile: 0x64,
			constraints: 0x00,
			level: 0x1f,
			inline: false,
		});
		video_config.description = Some(Bytes::from_static(&[0x01, 0x02, 0x03]));
		video_config.coded_width = Some(1920);
		video_config.coded_height = Some(1080);
		video_config.container = Container::Legacy;

		let mut video_renditions = BTreeMap::new();
		video_renditions.insert("video0.m4s".to_string(), video_config);

		let catalog = hang::Catalog {
			video: Video {
				renditions: video_renditions,
				display: None,
				rotation: None,
				flip: None,
			},
			..Default::default()
		};

		let msf = to_msf(&catalog);
		let video = &msf.tracks[0];
		assert_eq!(video.init_data, Some("AQID".to_string()));
	}

	#[test]
	fn convert_empty() {
		let catalog = hang::Catalog::default();
		let msf = to_msf(&catalog);
		assert_eq!(msf.version, 1);
		assert!(msf.tracks.is_empty());
	}

	#[test]
	fn convert_cmaf_packaging() {
		let mut video_config = VideoConfig::new(H264 {
			profile: 0x64,
			constraints: 0x00,
			level: 0x28,
			inline: false,
		});
		video_config.coded_width = Some(1920);
		video_config.coded_height = Some(1080);
		video_config.container = Container::Cmaf {
			init: base64::engine::general_purpose::STANDARD
				.decode("AAAYZ2Z0eXA=")
				.unwrap()
				.into(),
			timescale: None,
			track_id: None,
		};

		let mut video_renditions = BTreeMap::new();
		video_renditions.insert("video0.m4s".to_string(), video_config);

		let catalog = hang::Catalog {
			video: Video {
				renditions: video_renditions,
				display: None,
				rotation: None,
				flip: None,
			},
			..Default::default()
		};

		let msf = to_msf(&catalog);
		let video = &msf.tracks[0];
		assert_eq!(video.packaging, moq_msf::Packaging::Cmaf);
		assert_eq!(video.init_data, Some("AAAYZ2Z0eXA=".to_string()));
	}

	#[test]
	fn convert_sap_h264_with_jitter() {
		let mut video_config = VideoConfig::new(H264 {
			profile: 0x64,
			constraints: 0x00,
			level: 0x1f,
			inline: true,
		});
		video_config.coded_width = Some(1280);
		video_config.coded_height = Some(720);
		video_config.framerate = Some(30.0);
		video_config.container = Container::Legacy;
		video_config.jitter = Some(std::time::Duration::from_millis(100));

		let mut video_renditions = BTreeMap::new();
		video_renditions.insert("video0".to_string(), video_config);

		let mut audio_config = AudioConfig::new(AudioCodec::Opus, 48_000, 2);
		audio_config.container = Container::Legacy;
		audio_config.jitter = Some(std::time::Duration::from_millis(40));

		let mut audio_renditions = BTreeMap::new();
		audio_renditions.insert("audio0".to_string(), audio_config);

		let catalog = hang::Catalog {
			video: Video {
				renditions: video_renditions,
				display: None,
				rotation: None,
				flip: None,
			},
			audio: Audio {
				renditions: audio_renditions,
			},
		};

		let msf = to_msf(&catalog);

		let video = &msf.tracks[0];
		assert_eq!(video.role, Some(moq_msf::Role::Video));
		// H.264 may carry B-frames, so SAP starting type is 2.
		assert_eq!(video.max_grp_sap_starting_type, Some(2));
		assert_eq!(video.max_obj_sap_starting_type, Some(2));
		assert_eq!(video.jitter, Some(std::time::Duration::from_millis(100)));

		let audio = &msf.tracks[1];
		assert_eq!(audio.role, Some(moq_msf::Role::Audio));
		assert_eq!(audio.max_grp_sap_starting_type, Some(1));
		assert_eq!(audio.max_obj_sap_starting_type, Some(1));
		assert_eq!(audio.jitter, Some(std::time::Duration::from_millis(40)));
	}

	#[test]
	fn convert_sap_h265() {
		use hang::catalog::H265;

		let mut video_config = VideoConfig::new(H265 {
			in_band: false,
			profile_space: 0,
			profile_idc: 1,
			profile_compatibility_flags: [0, 0, 0, 0],
			tier_flag: false,
			level_idc: 93,
			constraint_flags: [0, 0, 0, 0, 0, 0],
		});
		video_config.coded_width = Some(1920);
		video_config.coded_height = Some(1080);
		video_config.framerate = Some(60.0);
		video_config.container = Container::Legacy;

		let mut video_renditions = BTreeMap::new();
		video_renditions.insert("video0".to_string(), video_config);

		let catalog = hang::Catalog {
			video: Video {
				renditions: video_renditions,
				display: None,
				rotation: None,
				flip: None,
			},
			..Default::default()
		};

		let msf = to_msf(&catalog);
		let video = &msf.tracks[0];
		// H.265 SAP behavior isn't validated end-to-end yet, so we omit the
		// SAP fields rather than advertise something we haven't verified.
		assert_eq!(video.max_grp_sap_starting_type, None);
		assert_eq!(video.max_obj_sap_starting_type, None);
		assert_eq!(video.jitter, None);
	}

	#[test]
	fn convert_sap_unknown_codec() {
		use hang::catalog::VideoCodec;

		let mut video_config = VideoConfig::new(VideoCodec::Unknown("future-codec.01".to_string()));
		video_config.container = Container::Legacy;

		let mut video_renditions = BTreeMap::new();
		video_renditions.insert("video0".to_string(), video_config);

		let catalog = hang::Catalog {
			video: Video {
				renditions: video_renditions,
				display: None,
				rotation: None,
				flip: None,
			},
			..Default::default()
		};

		let msf = to_msf(&catalog);
		let video = &msf.tracks[0];
		assert_eq!(video.max_grp_sap_starting_type, None);
		assert_eq!(video.max_obj_sap_starting_type, None);
	}
}
