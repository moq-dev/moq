use std::str::FromStr;
use std::task::Poll;

use anyhow::Context;
use base64::Engine;
use hang::catalog::{AudioCodec, AudioConfig, Container, VideoCodec, VideoConfig};

/// A consumer for the MSF catalog track.
///
/// Mirrors [`crate::catalog::Consumer`] but for the MSF (MOQT Streaming Format) catalog
/// track. Each update is parsed as [`moq_msf::Catalog`] and converted to [`hang::Catalog`]
/// so the rest of the pipeline only deals with hang types.
pub struct MsfConsumer {
	/// Access to the underlying track consumer.
	pub track: moq_net::TrackConsumer,
	group: Option<moq_net::GroupConsumer>,
}

impl MsfConsumer {
	/// Create a new MSF catalog consumer from a MoQ track consumer.
	///
	/// The track is expected to carry MSF catalog payloads (track name [`moq_msf::DEFAULT_NAME`]).
	pub fn new(track: moq_net::TrackConsumer) -> Self {
		Self { track, group: None }
	}

	/// Poll for the next catalog update, returned as a [`hang::Catalog`].
	pub fn poll_next(&mut self, waiter: &conducer::Waiter) -> Poll<anyhow::Result<Option<hang::Catalog>>> {
		// Drain pending groups, keeping only the newest. Remember whether the track is done
		// so we can distinguish "more groups may arrive" from "no more groups, ever".
		let track_finished = loop {
			match self.track.poll_next_group(waiter)? {
				Poll::Ready(Some(group)) => self.group = Some(group),
				Poll::Ready(None) => break true,
				Poll::Pending => break false,
			}
		};

		if let Some(group) = &mut self.group {
			match group.poll_read_frame(waiter)? {
				Poll::Ready(Some(frame)) => {
					self.group = None;
					let json = std::str::from_utf8(&frame).context("MSF catalog frame is not valid UTF-8")?;
					let msf = moq_msf::Catalog::from_str(json).context("failed to parse MSF catalog frame")?;
					let catalog = from_msf(&msf)?;
					return Poll::Ready(Ok(Some(catalog)));
				}
				Poll::Ready(None) => self.group = None,
				Poll::Pending => return Poll::Pending,
			}
		}

		if track_finished {
			Poll::Ready(Ok(None))
		} else {
			Poll::Pending
		}
	}

	/// Get the next catalog update.
	///
	/// Waits for the next MSF catalog publication and returns it converted to a
	/// [`hang::Catalog`]. Returns `None` when the track has ended with no further updates.
	pub async fn next(&mut self) -> anyhow::Result<Option<hang::Catalog>> {
		conducer::wait(|waiter| self.poll_next(waiter)).await
	}
}

impl From<moq_net::TrackConsumer> for MsfConsumer {
	fn from(inner: moq_net::TrackConsumer) -> Self {
		Self::new(inner)
	}
}

/// Convert an MSF catalog to a hang catalog.
///
/// Each MSF track is mapped onto a `hang::Catalog` rendition based on its [`moq_msf::Role`]:
/// video tracks become [`VideoConfig`] entries, audio tracks become [`AudioConfig`] entries.
/// Tracks with no role, with an unsupported role (caption, subtitle, sign language, audio
/// description, custom roles), or with packaging other than [`moq_msf::Packaging::Loc`],
/// [`moq_msf::Packaging::Cmaf`], or [`moq_msf::Packaging::Legacy`] are skipped with a warning.
///
/// Both [`moq_msf::Packaging::Loc`] and [`moq_msf::Packaging::Legacy`] map to
/// [`Container::Legacy`]. [`moq_msf::Packaging::Cmaf`] requires `init_data` to be present
/// (base64-encoded ftyp+moov); a missing or malformed init segment is an error.
///
/// Fields with no representation in `hang::Catalog` (`is_live`, `render_group`, `alt_group`,
/// `max_grp_sap_starting_type`, `max_obj_sap_starting_type`) are dropped.
pub(crate) fn from_msf(msf: &moq_msf::Catalog) -> anyhow::Result<hang::Catalog> {
	let mut catalog = hang::Catalog::default();

	for track in &msf.tracks {
		let Some(role) = track.role.as_ref() else {
			tracing::warn!(track = %track.name, "skipping MSF track with no role");
			continue;
		};

		match role {
			moq_msf::Role::Video => match video_config_from_msf(track)? {
				Some(config) => {
					catalog.video.renditions.insert(track.name.clone(), config);
				}
				None => {
					tracing::warn!(
						track = %track.name,
						packaging = %track.packaging,
						"skipping MSF video track with unsupported packaging",
					);
				}
			},
			moq_msf::Role::Audio => match audio_config_from_msf(track)? {
				Some(config) => {
					catalog.audio.renditions.insert(track.name.clone(), config);
				}
				None => {
					tracing::warn!(
						track = %track.name,
						packaging = %track.packaging,
						"skipping MSF audio track with unsupported packaging",
					);
				}
			},
			other => {
				tracing::warn!(track = %track.name, role = %other, "skipping MSF track with unsupported role");
			}
		}
	}

	Ok(catalog)
}

/// Decode the [`Container`] for a track based on its packaging and `init_data`.
///
/// Returns `Ok(None)` when the packaging is unsupported (e.g. `MediaTimeline`,
/// `EventTimeline`, or an unknown variant). The caller skips these tracks with a warning
/// rather than failing the whole catalog, since unsupported packaging is a downstream
/// pipeline limitation, not a malformed catalog.
///
/// Returns `Err` when a CMAF track is missing or has malformed `init_data`. This is an
/// intentional hard error: a CMAF rendition is unusable without its `ftyp+moov` init
/// segment, and silently skipping it would mask a publisher bug.
fn container_from_msf(track: &moq_msf::Track) -> anyhow::Result<Option<Container>> {
	match &track.packaging {
		// Both LOC and Legacy represent raw payloads without ISO-BMFF boxing.
		moq_msf::Packaging::Loc | moq_msf::Packaging::Legacy => Ok(Some(Container::Legacy)),
		moq_msf::Packaging::Cmaf => {
			let init = decode_init_data(track)?
				.with_context(|| format!("MSF CMAF track {:?} missing init_data", track.name))?;
			Ok(Some(Container::Cmaf { init }))
		}
		_ => Ok(None),
	}
}

/// Base64-decode `track.init_data` into a `Bytes` buffer, propagating a
/// descriptive error on malformed input. Returns `Ok(None)` when no
/// `init_data` is present.
///
/// For CMAF tracks the decoded bytes are the full `ftyp+moov` init segment.
/// For Legacy/LOC tracks the bytes are the codec-specific decoder
/// description (e.g. an AVCC/HVCC config record or AAC AudioSpecificConfig)
/// that downstream decoders need to configure their bitstream parsers.
fn decode_init_data(track: &moq_msf::Track) -> anyhow::Result<Option<bytes::Bytes>> {
	track
		.init_data
		.as_ref()
		.map(|b64| {
			base64::engine::general_purpose::STANDARD
				.decode(b64)
				.map(bytes::Bytes::from)
				.with_context(|| format!("MSF track {:?} has malformed init_data", track.name))
		})
		.transpose()
}

/// Pull the decoder description out of a Legacy/LOC MSF track's `init_data`.
///
/// CMAF tracks carry their config inside `Container::Cmaf::init`, so this
/// returns `Ok(None)` for them to avoid duplicating the bytes.
fn legacy_description(track: &moq_msf::Track) -> anyhow::Result<Option<bytes::Bytes>> {
	match track.packaging {
		moq_msf::Packaging::Loc | moq_msf::Packaging::Legacy => decode_init_data(track),
		_ => Ok(None),
	}
}

fn video_config_from_msf(track: &moq_msf::Track) -> anyhow::Result<Option<VideoConfig>> {
	// Unsupported packaging (e.g. MediaTimeline) bubbles up as Ok(None) so the caller can
	// skip the track with a warning rather than fail the whole catalog.
	let Some(container) = container_from_msf(track)? else {
		return Ok(None);
	};

	let codec_str = track
		.codec
		.as_deref()
		.with_context(|| format!("MSF video track {:?} missing codec", track.name))?;
	// VideoCodec::from_str returns Ok(VideoCodec::Unknown(s)) for codecs it doesn't know,
	// so this only fails for malformed structured codec strings (avc1.xxx, hvc1.xxx, etc.).
	let codec = VideoCodec::from_str(codec_str)
		.with_context(|| format!("MSF video track {:?} has invalid codec {codec_str:?}", track.name))?;

	Ok(Some(VideoConfig {
		codec,
		description: legacy_description(track)?,
		coded_width: track.width,
		coded_height: track.height,
		display_ratio_width: None,
		display_ratio_height: None,
		bitrate: track.bitrate,
		framerate: track.framerate,
		optimize_for_latency: None,
		container,
		// Jitter is converted from f64 milliseconds to integer milliseconds.
		// Fractional milliseconds are truncated (e.g. 15.5ms becomes 15ms).
		// This is acceptable for the jitter use case where sub-ms precision
		// is not meaningful.
		jitter: track
			.jitter
			.filter(|v| v.is_finite() && *v >= 0.0)
			.and_then(|v| moq_net::Time::from_millis(v as u64).ok()),
	}))
}

fn audio_config_from_msf(track: &moq_msf::Track) -> anyhow::Result<Option<AudioConfig>> {
	let Some(container) = container_from_msf(track)? else {
		return Ok(None);
	};

	let codec_str = track
		.codec
		.as_deref()
		.with_context(|| format!("MSF audio track {:?} missing codec", track.name))?;
	let codec = AudioCodec::from_str(codec_str)
		.with_context(|| format!("MSF audio track {:?} has invalid codec {codec_str:?}", track.name))?;

	// MSF leaves samplerate and channelConfig optional. Hang requires both, so we fall back
	// to the WebCodecs-typical defaults (48kHz stereo) when the upstream catalog omits them.
	let sample_rate = track.samplerate.unwrap_or(48_000);
	let channel_count = track
		.channel_config
		.as_deref()
		.and_then(|s| s.parse::<u32>().ok())
		.unwrap_or(2);

	Ok(Some(AudioConfig {
		codec,
		sample_rate,
		channel_count,
		bitrate: track.bitrate,
		description: legacy_description(track)?,
		container,
		// Jitter is converted from f64 milliseconds to integer milliseconds.
		// Fractional milliseconds are truncated (e.g. 15.5ms becomes 15ms).
		// This is acceptable for the jitter use case where sub-ms precision
		// is not meaningful.
		jitter: track
			.jitter
			.filter(|v| v.is_finite() && *v >= 0.0)
			.and_then(|v| moq_net::Time::from_millis(v as u64).ok()),
	}))
}

#[cfg(test)]
mod test {
	use super::*;

	fn video_track(name: &str, packaging: moq_msf::Packaging, init_data: Option<&str>) -> moq_msf::Track {
		moq_msf::Track {
			name: name.to_string(),
			packaging,
			is_live: true,
			role: Some(moq_msf::Role::Video),
			codec: Some("avc1.640028".to_string()),
			width: Some(1920),
			height: Some(1080),
			framerate: Some(30.0),
			samplerate: None,
			channel_config: None,
			bitrate: Some(5_000_000),
			init_data: init_data.map(str::to_string),
			render_group: Some(1),
			alt_group: None,
			max_grp_sap_starting_type: None,
			max_obj_sap_starting_type: None,
			jitter: None,
		}
	}

	fn audio_track(name: &str, packaging: moq_msf::Packaging) -> moq_msf::Track {
		moq_msf::Track {
			name: name.to_string(),
			packaging,
			is_live: true,
			role: Some(moq_msf::Role::Audio),
			codec: Some("opus".to_string()),
			width: None,
			height: None,
			framerate: None,
			samplerate: Some(48_000),
			channel_config: Some("2".to_string()),
			bitrate: Some(128_000),
			init_data: None,
			render_group: Some(1),
			alt_group: None,
			max_grp_sap_starting_type: None,
			max_obj_sap_starting_type: None,
			jitter: None,
		}
	}

	#[test]
	fn cmaf_video_yields_cmaf_container() {
		// "AAAYZ2Z0eXA=" decodes to a tiny ftyp-shaped stub; we just verify the bytes
		// round-trip through base64 into Container::Cmaf.init.
		let init_b64 = "AAAYZ2Z0eXA=";
		let expected_init = base64::engine::general_purpose::STANDARD.decode(init_b64).unwrap();

		let msf = moq_msf::Catalog {
			version: 1,
			tracks: vec![video_track("video0", moq_msf::Packaging::Cmaf, Some(init_b64))],
		};

		let catalog = from_msf(&msf).expect("CMAF video should convert");
		let video = catalog.video.renditions.get("video0").expect("video0 rendition");

		match &video.container {
			Container::Cmaf { init } => assert_eq!(init.as_ref(), expected_init.as_slice()),
			Container::Legacy => panic!("expected Cmaf container, got Legacy"),
		}
		assert_eq!(video.coded_width, Some(1920));
		assert_eq!(video.coded_height, Some(1080));
		assert_eq!(video.framerate, Some(30.0));
		assert_eq!(video.bitrate, Some(5_000_000));
	}

	#[test]
	fn loc_audio_yields_legacy_container() {
		let msf = moq_msf::Catalog {
			version: 1,
			tracks: vec![audio_track("audio0", moq_msf::Packaging::Loc)],
		};

		let catalog = from_msf(&msf).expect("LOC audio should convert");
		let audio = catalog.audio.renditions.get("audio0").expect("audio0 rendition");

		assert_eq!(audio.container, Container::Legacy);
		assert_eq!(audio.codec, AudioCodec::Opus);
		assert_eq!(audio.sample_rate, 48_000);
		assert_eq!(audio.channel_count, 2);
		assert_eq!(audio.bitrate, Some(128_000));
	}

	#[test]
	fn legacy_init_data_round_trips_into_description() {
		// Legacy tracks carry the decoder description in `init_data` (base64).
		// Roundtripping the bytes through Container::Legacy must preserve them
		// in the `description` field for downstream decoders.
		let description_bytes: &[u8] = &[0x01, 0x42, 0xc0, 0x1e, 0xff, 0xe1];
		let init_b64 = base64::engine::general_purpose::STANDARD.encode(description_bytes);

		let mut video = video_track("video0", moq_msf::Packaging::Legacy, Some(&init_b64));
		video.codec = Some("avc1.42c01e".to_string());

		let mut audio = audio_track("audio0", moq_msf::Packaging::Loc);
		audio.init_data = Some(init_b64);

		let msf = moq_msf::Catalog {
			version: 1,
			tracks: vec![video, audio],
		};

		let catalog = from_msf(&msf).expect("legacy tracks should convert");
		let v = catalog.video.renditions.get("video0").expect("video0 rendition");
		let a = catalog.audio.renditions.get("audio0").expect("audio0 rendition");

		assert_eq!(v.description.as_deref(), Some(description_bytes));
		assert_eq!(a.description.as_deref(), Some(description_bytes));
	}

	#[test]
	fn cmaf_description_stays_none() {
		// CMAF tracks carry their bytes inside Container::Cmaf::init; description
		// must stay None so downstream code reads the bytes from one place only.
		let init_b64 = "AAAYZ2Z0eXA=";
		let msf = moq_msf::Catalog {
			version: 1,
			tracks: vec![video_track("video0", moq_msf::Packaging::Cmaf, Some(init_b64))],
		};
		let catalog = from_msf(&msf).unwrap();
		assert!(catalog.video.renditions["video0"].description.is_none());
	}

	#[test]
	fn legacy_malformed_init_data_is_error() {
		let mut track = video_track("video0", moq_msf::Packaging::Legacy, Some("!!!not-base64!!!"));
		track.codec = Some("avc1.42c01e".to_string());
		let msf = moq_msf::Catalog {
			version: 1,
			tracks: vec![track],
		};
		let err = from_msf(&msf).expect_err("malformed base64 should error");
		assert!(
			err.to_string().contains("malformed init_data"),
			"unexpected error: {}",
			err
		);
	}

	#[test]
	fn unknown_codec_yields_unknown_variant() {
		let mut track = video_track("video0", moq_msf::Packaging::Legacy, None);
		track.codec = Some("weirdcodec".to_string());
		let msf = moq_msf::Catalog {
			version: 1,
			tracks: vec![track],
		};

		let catalog = from_msf(&msf).expect("unknown codec is not an error");
		let video = catalog.video.renditions.get("video0").expect("video0 rendition");
		assert_eq!(video.codec, VideoCodec::Unknown("weirdcodec".to_string()));
	}

	#[test]
	fn cmaf_without_init_data_is_error() {
		let msf = moq_msf::Catalog {
			version: 1,
			tracks: vec![video_track("video0", moq_msf::Packaging::Cmaf, None)],
		};

		let err = from_msf(&msf).expect_err("CMAF without init_data must error");
		let msg = format!("{err:#}");
		assert!(msg.contains("init_data"), "expected init_data in error, got: {msg}");
	}

	#[test]
	fn empty_catalog_is_empty_hang_catalog() {
		let msf = moq_msf::Catalog {
			version: 1,
			tracks: vec![],
		};

		let catalog = from_msf(&msf).expect("empty catalog should convert");
		assert!(catalog.video.renditions.is_empty());
		assert!(catalog.audio.renditions.is_empty());
	}

	#[test]
	fn track_without_role_is_skipped() {
		let mut track = video_track("video0", moq_msf::Packaging::Legacy, None);
		track.role = None;
		let msf = moq_msf::Catalog {
			version: 1,
			tracks: vec![track],
		};

		let catalog = from_msf(&msf).expect("no-role track should be skipped, not error");
		assert!(catalog.video.renditions.is_empty());
		assert!(catalog.audio.renditions.is_empty());
	}

	#[test]
	fn unsupported_role_is_skipped() {
		let mut track = audio_track("caption0", moq_msf::Packaging::Legacy);
		track.role = Some(moq_msf::Role::Caption);
		let msf = moq_msf::Catalog {
			version: 1,
			tracks: vec![track],
		};

		let catalog = from_msf(&msf).expect("unsupported role should be skipped, not error");
		assert!(catalog.audio.renditions.is_empty());
		assert!(catalog.video.renditions.is_empty());
	}

	#[test]
	fn audio_defaults_when_samplerate_and_channels_missing() {
		let mut track = audio_track("audio0", moq_msf::Packaging::Legacy);
		track.samplerate = None;
		track.channel_config = None;
		let msf = moq_msf::Catalog {
			version: 1,
			tracks: vec![track],
		};

		let catalog = from_msf(&msf).expect("missing samplerate/channels should default");
		let audio = catalog.audio.renditions.get("audio0").expect("audio0 rendition");
		assert_eq!(audio.sample_rate, 48_000);
		assert_eq!(audio.channel_count, 2);
	}

	#[test]
	fn unsupported_packaging_video_is_skipped() {
		// MediaTimeline isn't a media payload, so the track must be skipped (not error).
		let bad = video_track("timeline0", moq_msf::Packaging::MediaTimeline, None);
		let good = video_track("video0", moq_msf::Packaging::Legacy, None);
		let msf = moq_msf::Catalog {
			version: 1,
			tracks: vec![bad, good],
		};

		let catalog = from_msf(&msf).expect("unsupported packaging should be skipped, not error");
		assert!(
			!catalog.video.renditions.contains_key("timeline0"),
			"timeline track must be skipped"
		);
		assert!(
			catalog.video.renditions.contains_key("video0"),
			"sibling track must still be parsed"
		);
	}

	#[test]
	fn unsupported_packaging_audio_is_skipped() {
		let mut bad = audio_track("event0", moq_msf::Packaging::EventTimeline);
		// Drop the codec so we'd see a hard error if the skip path didn't short-circuit.
		bad.codec = None;
		let good = audio_track("audio0", moq_msf::Packaging::Loc);
		let msf = moq_msf::Catalog {
			version: 1,
			tracks: vec![bad, good],
		};

		let catalog = from_msf(&msf).expect("unsupported packaging should be skipped, not error");
		assert!(!catalog.audio.renditions.contains_key("event0"));
		assert!(catalog.audio.renditions.contains_key("audio0"));
	}

	#[test]
	fn unknown_packaging_variant_is_skipped() {
		let track = video_track("video0", moq_msf::Packaging::Unknown("custom".to_string()), None);
		let msf = moq_msf::Catalog {
			version: 1,
			tracks: vec![track],
		};

		let catalog = from_msf(&msf).expect("unknown packaging should be skipped, not error");
		assert!(catalog.video.renditions.is_empty());
	}

	#[test]
	fn missing_video_codec_is_error() {
		let mut track = video_track("video0", moq_msf::Packaging::Legacy, None);
		track.codec = None;
		let msf = moq_msf::Catalog {
			version: 1,
			tracks: vec![track],
		};

		let err = from_msf(&msf).expect_err("missing video codec must error");
		let msg = format!("{err:#}");
		assert!(
			msg.contains("missing codec"),
			"expected 'missing codec' in error, got: {msg}"
		);
	}

	#[test]
	fn missing_audio_codec_is_error() {
		let mut track = audio_track("audio0", moq_msf::Packaging::Legacy);
		track.codec = None;
		let msf = moq_msf::Catalog {
			version: 1,
			tracks: vec![track],
		};

		let err = from_msf(&msf).expect_err("missing audio codec must error");
		let msg = format!("{err:#}");
		assert!(
			msg.contains("missing codec"),
			"expected 'missing codec' in error, got: {msg}"
		);
	}

	#[test]
	fn invalid_video_codec_includes_codec_in_error() {
		// avc1 with a too-short profile string is a malformed structured codec.
		let mut track = video_track("video0", moq_msf::Packaging::Legacy, None);
		track.codec = Some("avc1.0".to_string());
		let msf = moq_msf::Catalog {
			version: 1,
			tracks: vec![track],
		};

		let err = from_msf(&msf).expect_err("malformed avc1 codec must error");
		let msg = format!("{err:#}");
		assert!(msg.contains("avc1.0"), "expected codec string in error, got: {msg}");
	}
}
