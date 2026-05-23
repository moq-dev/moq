use std::ops::{Deref, DerefMut};
use std::sync::{Arc, Mutex, MutexGuard};

use base64::Engine;

/// Produces both a hang and MSF catalog track for a broadcast.
///
/// The JSON catalog is updated when tracks are added/removed but is *not* automatically published.
/// You'll have to call [`lock`](Self::lock) to update and publish the catalog.
/// Both the hang (`catalog.json`) and MSF (`catalog`) tracks are published on drop of the guard.
#[derive(Clone)]
pub struct Producer {
	hang_track: moq_net::TrackProducer,
	msf_track: moq_net::TrackProducer,

	current: Arc<Mutex<hang::Catalog>>,
}

impl Producer {
	/// Create a new catalog producer, inserting both catalog tracks into the broadcast.
	pub fn new(broadcast: &mut moq_net::BroadcastProducer) -> Result<Self, moq_net::Error> {
		Self::with_catalog(broadcast, hang::Catalog::default())
	}

	/// Create a new catalog producer with the given initial catalog.
	pub fn with_catalog(
		broadcast: &mut moq_net::BroadcastProducer,
		catalog: hang::Catalog,
	) -> Result<Self, moq_net::Error> {
		let hang_track = broadcast.create_track(hang::Catalog::default_track())?;
		let msf_track = broadcast.create_track(moq_net::Track::new(moq_msf::DEFAULT_NAME))?;

		Ok(Self {
			hang_track,
			msf_track,
			current: Arc::new(Mutex::new(catalog)),
		})
	}

	/// Get mutable access to the catalog, publishing it after any changes.
	pub fn lock(&mut self) -> Guard<'_> {
		Guard {
			catalog: self.current.lock().unwrap(),
			hang_track: &mut self.hang_track,
			msf_track: &mut self.msf_track,
			updated: false,
		}
	}

	/// Get a snapshot of the current catalog.
	pub fn snapshot(&self) -> hang::Catalog {
		self.current.lock().unwrap().clone()
	}

	/// Create a consumer for this catalog, receiving updates as they're published.
	pub fn consume(&self) -> Result<super::Consumer, moq_net::Error> {
		let track = self.hang_track.consume();
		let subscriber = track;
		Ok(super::Consumer::new(subscriber))
	}

	/// Finish publishing to this catalog.
	pub fn finish(&mut self) -> Result<(), moq_net::Error> {
		self.hang_track.finish()?;
		self.msf_track.finish()?;
		Ok(())
	}
}

/// RAII guard for modifying a catalog with automatic publishing on drop.
///
/// Obtained via [`Producer::lock`].
///
/// On drop, both the hang and MSF catalog tracks are updated if the catalog was mutated.
pub struct Guard<'a> {
	catalog: MutexGuard<'a, hang::Catalog>,
	hang_track: &'a mut moq_net::TrackProducer,
	msf_track: &'a mut moq_net::TrackProducer,
	updated: bool,
}

impl<'a> Deref for Guard<'a> {
	type Target = hang::Catalog;

	fn deref(&self) -> &Self::Target {
		&self.catalog
	}
}

impl<'a> DerefMut for Guard<'a> {
	fn deref_mut(&mut self) -> &mut Self::Target {
		self.updated = true;
		&mut self.catalog
	}
}

impl Drop for Guard<'_> {
	fn drop(&mut self) {
		if !self.updated {
			return;
		}

		// Publish hang catalog
		if let Ok(mut group) = self.hang_track.append_group() {
			let frame = self.catalog.to_string().expect("invalid catalog");
			let _ = group.write_frame(frame);
			let _ = group.finish();
		}

		// Publish MSF catalog
		let msf = to_msf(&self.catalog);
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
		tracks.push(moq_msf::Track {
			name: name.clone(),
			packaging,
			is_live: true,
			role: Some(moq_msf::Role::Video),
			codec: Some(config.codec.to_string()),
			width: config.coded_width,
			height: config.coded_height,
			framerate: config.framerate,
			samplerate: None,
			channel_config: None,
			bitrate: config.bitrate,
			init_data,
			render_group: Some(1),
			alt_group: if has_multiple_video { Some(1) } else { None },
			max_grp_sap_starting_type: sap_type,
			max_obj_sap_starting_type: sap_type,
			jitter: config.jitter.map(|t| t.as_millis() as f64),
		});
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

		tracks.push(moq_msf::Track {
			name: name.clone(),
			packaging,
			is_live: true,
			role: Some(moq_msf::Role::Audio),
			codec: Some(config.codec.to_string()),
			width: None,
			height: None,
			framerate: None,
			samplerate: Some(config.sample_rate),
			channel_config: Some(config.channel_count.to_string()),
			bitrate: config.bitrate,
			init_data,
			render_group: Some(1),
			alt_group: if has_multiple_audio { Some(1) } else { None },
			max_grp_sap_starting_type: Some(1),
			max_obj_sap_starting_type: Some(1),
			jitter: config.jitter.map(|t| t.as_millis() as f64),
		});
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
		let mut video_renditions = BTreeMap::new();
		video_renditions.insert(
			"video0.avc3".to_string(),
			VideoConfig {
				codec: H264 {
					profile: 0x64,
					constraints: 0x00,
					level: 0x1f,
					inline: true,
				}
				.into(),
				description: None,
				coded_width: Some(1280),
				coded_height: Some(720),
				display_ratio_width: None,
				display_ratio_height: None,
				bitrate: Some(6_000_000),
				framerate: Some(30.0),
				optimize_for_latency: None,
				container: Container::Legacy,
				jitter: None,
			},
		);

		let mut audio_renditions = BTreeMap::new();
		audio_renditions.insert(
			"audio0".to_string(),
			AudioConfig {
				codec: AudioCodec::Opus,
				sample_rate: 48_000,
				channel_count: 2,
				bitrate: Some(128_000),
				description: None,
				container: Container::Legacy,
				jitter: None,
			},
		);

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
			..Default::default()
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
		let mut video_renditions = BTreeMap::new();
		video_renditions.insert(
			"video0.m4s".to_string(),
			VideoConfig {
				codec: H264 {
					profile: 0x64,
					constraints: 0x00,
					level: 0x1f,
					inline: false,
				}
				.into(),
				description: Some(Bytes::from_static(&[0x01, 0x02, 0x03])),
				coded_width: Some(1920),
				coded_height: Some(1080),
				display_ratio_width: None,
				display_ratio_height: None,
				bitrate: None,
				framerate: None,
				optimize_for_latency: None,
				container: Container::Legacy,
				jitter: None,
			},
		);

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
		let mut video_renditions = BTreeMap::new();
		video_renditions.insert(
			"video0.m4s".to_string(),
			VideoConfig {
				codec: H264 {
					profile: 0x64,
					constraints: 0x00,
					level: 0x28,
					inline: false,
				}
				.into(),
				description: None,
				coded_width: Some(1920),
				coded_height: Some(1080),
				display_ratio_width: None,
				display_ratio_height: None,
				bitrate: None,
				framerate: None,
				optimize_for_latency: None,
				container: Container::Cmaf {
					init: base64::engine::general_purpose::STANDARD
						.decode("AAAYZ2Z0eXA=")
						.unwrap()
						.into(),
					timescale: None,
					track_id: None,
				},
				jitter: None,
			},
		);

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
		let mut video_renditions = BTreeMap::new();
		video_renditions.insert(
			"video0".to_string(),
			VideoConfig {
				codec: H264 {
					profile: 0x64,
					constraints: 0x00,
					level: 0x1f,
					inline: true,
				}
				.into(),
				description: None,
				coded_width: Some(1280),
				coded_height: Some(720),
				display_ratio_width: None,
				display_ratio_height: None,
				bitrate: None,
				framerate: Some(30.0),
				optimize_for_latency: None,
				container: Container::Legacy,
				jitter: Some(moq_net::Time::from_millis_unchecked(100)),
			},
		);

		let mut audio_renditions = BTreeMap::new();
		audio_renditions.insert(
			"audio0".to_string(),
			AudioConfig {
				codec: AudioCodec::Opus,
				sample_rate: 48_000,
				channel_count: 2,
				bitrate: None,
				description: None,
				container: Container::Legacy,
				jitter: Some(moq_net::Time::from_millis_unchecked(40)),
			},
		);

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
			..Default::default()
		};

		let msf = to_msf(&catalog);

		let video = &msf.tracks[0];
		assert_eq!(video.role, Some(moq_msf::Role::Video));
		// H.264 may carry B-frames, so SAP starting type is 2.
		assert_eq!(video.max_grp_sap_starting_type, Some(2));
		assert_eq!(video.max_obj_sap_starting_type, Some(2));
		assert_eq!(video.jitter, Some(100.0));

		let audio = &msf.tracks[1];
		assert_eq!(audio.role, Some(moq_msf::Role::Audio));
		assert_eq!(audio.max_grp_sap_starting_type, Some(1));
		assert_eq!(audio.max_obj_sap_starting_type, Some(1));
		assert_eq!(audio.jitter, Some(40.0));
	}

	#[test]
	fn convert_sap_h265() {
		use hang::catalog::H265;

		let mut video_renditions = BTreeMap::new();
		video_renditions.insert(
			"video0".to_string(),
			VideoConfig {
				codec: H265 {
					in_band: false,
					profile_space: 0,
					profile_idc: 1,
					profile_compatibility_flags: [0, 0, 0, 0],
					tier_flag: false,
					level_idc: 93,
					constraint_flags: [0, 0, 0, 0, 0, 0],
				}
				.into(),
				description: None,
				coded_width: Some(1920),
				coded_height: Some(1080),
				display_ratio_width: None,
				display_ratio_height: None,
				bitrate: None,
				framerate: Some(60.0),
				optimize_for_latency: None,
				container: Container::Legacy,
				jitter: None,
			},
		);

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

		let mut video_renditions = BTreeMap::new();
		video_renditions.insert(
			"video0".to_string(),
			VideoConfig {
				codec: VideoCodec::Unknown("future-codec.01".to_string()),
				description: None,
				coded_width: None,
				coded_height: None,
				display_ratio_width: None,
				display_ratio_height: None,
				bitrate: None,
				framerate: None,
				optimize_for_latency: None,
				container: Container::Legacy,
				jitter: None,
			},
		);

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
