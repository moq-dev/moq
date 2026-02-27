//! MSF catalog conversion and helpers.
//!
//! Converts a [`hang::Catalog`] to an MSF [`moq_msf::Catalog`].

use base64::Engine;

/// Convert a hang catalog to an MSF catalog.
pub fn to_msf(catalog: &hang::Catalog) -> moq_msf::Catalog {
	let mut tracks = Vec::new();

	let has_multiple_video = catalog.video.renditions.len() > 1;
	for (name, config) in &catalog.video.renditions {
		let packaging = match &config.container {
			hang::catalog::Container::Cmaf { .. } => moq_msf::Packaging::Cmaf,
			// TODO: For CMAF packaging, build proper CMAF init segments (ftyp+moov).
			// See draft-ietf-moq-cmsf-00 for the required structure.
			_ => moq_msf::Packaging::Legacy,
		};

		let init_data = config
			.description
			.as_ref()
			.map(|d| base64::engine::general_purpose::STANDARD.encode(d.as_ref()));

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
		});
	}

	let has_multiple_audio = catalog.audio.renditions.len() > 1;
	for (name, config) in &catalog.audio.renditions {
		let packaging = match &config.container {
			hang::catalog::Container::Cmaf { .. } => moq_msf::Packaging::Cmaf,
			_ => moq_msf::Packaging::Legacy,
		};

		let init_data = config
			.description
			.as_ref()
			.map(|d| base64::engine::general_purpose::STANDARD.encode(d.as_ref()));

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
		});
	}

	moq_msf::Catalog { version: 1, tracks }
}

/// Publish the MSF catalog derived from a hang catalog to the given track.
pub fn publish(catalog: &hang::Catalog, track: &mut moq_lite::TrackProducer) {
	let msf = to_msf(catalog);
	let Ok(mut group) = track.append_group() else {
		return;
	};
	let _ = group.write_frame(msf.to_string().expect("invalid MSF catalog"));
	let _ = group.finish();
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

		let audio = &msf.tracks[1];
		assert_eq!(audio.name, "audio0");
		assert_eq!(audio.role, Some(moq_msf::Role::Audio));
		assert_eq!(audio.packaging, moq_msf::Packaging::Legacy);
		assert_eq!(audio.codec, Some("opus".to_string()));
		assert_eq!(audio.samplerate, Some(48_000));
		assert_eq!(audio.channel_config, Some("2".to_string()));
		assert_eq!(audio.bitrate, Some(128_000));
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
					timescale: 90000,
					track_id: 1,
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
	}
}
