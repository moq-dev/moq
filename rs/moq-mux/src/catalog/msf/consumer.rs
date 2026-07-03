use std::str::FromStr;
use std::task::Poll;

use base64::Engine;
use hang::catalog::{AudioCodec, AudioConfig, Container, VideoCodec, VideoConfig};

use crate::Result;
use crate::catalog::msf::Error;

/// A consumer for the MSF catalog track.
///
/// Mirrors [`crate::catalog::hang::Consumer`] but for the MSF (MOQT Streaming Format) catalog
/// track. Each update is parsed as [`moq_msf::Catalog`] and converted to [`hang::Catalog`]
/// so the rest of the pipeline only deals with hang types.
pub struct Consumer {
	/// Access to the underlying track consumer.
	pub track: moq_net::TrackSubscriber,
	group: Option<moq_net::GroupConsumer>,
}

impl Consumer {
	/// Create a new MSF catalog consumer from a MoQ track consumer.
	///
	/// The track is expected to carry MSF catalog payloads (track name [`moq_msf::DEFAULT_NAME`]).
	pub fn new(track: moq_net::TrackSubscriber) -> Self {
		Self { track, group: None }
	}

	/// Poll for the next catalog update, returned as a [`hang::Catalog`].
	pub fn poll_next(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<hang::Catalog>>> {
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
					let json = std::str::from_utf8(&frame).map_err(|_| Error::InvalidUtf8)?;
					let msf = match moq_msf::Catalog::from_str(json) {
						Ok(msf) => msf,
						Err(err) => {
							tracing::warn!(error = %err, "failed to parse MSF catalog frame");
							return Poll::Ready(Err(Error::ParseFrame.into()));
						}
					};
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
	pub async fn next(&mut self) -> Result<Option<hang::Catalog>> {
		kio::wait(|waiter| self.poll_next(waiter)).await
	}
}

impl From<moq_net::TrackSubscriber> for Consumer {
	fn from(inner: moq_net::TrackSubscriber) -> Self {
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
/// Fields with no representation in `hang::Catalog` (`generated_at`, `is_complete`, `is_live`,
/// `render_group`, `alt_group`, `max_grp_sap_starting_type`, `max_obj_sap_starting_type`) are
/// dropped.
pub(crate) fn from_msf(msf: &moq_msf::Catalog) -> Result<hang::Catalog> {
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
fn container_from_msf(track: &moq_msf::Track) -> Result<Option<Container>> {
	match &track.packaging {
		// Both LOC and Legacy represent raw payloads without ISO-BMFF boxing.
		moq_msf::Packaging::Loc | moq_msf::Packaging::Legacy => Ok(Some(Container::Legacy)),
		moq_msf::Packaging::Cmaf => {
			let init = decode_init_data(track)?.ok_or_else(|| Error::MissingCmafInit(track.name.clone()))?;
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
fn decode_init_data(track: &moq_msf::Track) -> Result<Option<bytes::Bytes>> {
	track
		.init_data
		.as_ref()
		.map(|b64| {
			base64::engine::general_purpose::STANDARD
				.decode(b64)
				.map(bytes::Bytes::from)
				.map_err(|_| Error::MalformedInitData(track.name.clone()).into())
		})
		.transpose()
}

/// Pull the decoder description out of a Legacy/LOC MSF track's `init_data`.
///
/// CMAF tracks carry their config inside `Container::Cmaf::init`, so this
/// returns `Ok(None)` for them to avoid duplicating the bytes.
fn legacy_description(track: &moq_msf::Track) -> Result<Option<bytes::Bytes>> {
	match track.packaging {
		moq_msf::Packaging::Loc | moq_msf::Packaging::Legacy => decode_init_data(track),
		_ => Ok(None),
	}
}

fn video_config_from_msf(track: &moq_msf::Track) -> Result<Option<VideoConfig>> {
	// Unsupported packaging (e.g. MediaTimeline) bubbles up as Ok(None) so the caller can
	// skip the track with a warning rather than fail the whole catalog.
	let Some(container) = container_from_msf(track)? else {
		return Ok(None);
	};

	let codec_str = track
		.codec
		.as_deref()
		.ok_or_else(|| Error::MissingVideoCodec(track.name.clone()))?;
	// VideoCodec::from_str returns Ok(VideoCodec::Unknown(s)) for codecs it doesn't know,
	// so this only fails for malformed structured codec strings (avc1.xxx, hvc1.xxx, etc.).
	let codec = VideoCodec::from_str(codec_str).map_err(|_| Error::InvalidVideoCodec {
		name: track.name.clone(),
		codec: codec_str.to_string(),
	})?;

	let mut config = VideoConfig::new(codec);
	config.description = legacy_description(track)?;
	config.coded_width = track.width;
	config.coded_height = track.height;
	config.bitrate = track.bitrate;
	config.framerate = track.framerate;
	config.container = container;
	config.jitter = track.jitter;
	Ok(Some(config))
}

fn audio_config_from_msf(track: &moq_msf::Track) -> Result<Option<AudioConfig>> {
	let Some(container) = container_from_msf(track)? else {
		return Ok(None);
	};

	let codec_str = track
		.codec
		.as_deref()
		.ok_or_else(|| Error::MissingAudioCodec(track.name.clone()))?;
	let codec = AudioCodec::from_str(codec_str).map_err(|_| Error::InvalidAudioCodec {
		name: track.name.clone(),
		codec: codec_str.to_string(),
	})?;

	// MSF leaves samplerate and channelConfig optional, but hang requires both. Trust the
	// explicit fields when present; otherwise parse the codec init data (AAC
	// AudioSpecificConfig, OpusHead, or the CMAF moov's audio sample entry) to derive what's
	// missing. Defaults are dangerous here: a wrong sample rate produces audible distortion
	// or no audio at all.
	let channel_count_from_field = track.channel_config.as_deref().and_then(|s| s.parse::<u32>().ok());
	let (sample_rate, channel_count) = match (track.samplerate, channel_count_from_field) {
		(Some(sr), Some(cc)) => (sr, cc),
		(sr_opt, cc_opt) => {
			let derived = derive_audio_params(track, &codec)?;
			(
				sr_opt.unwrap_or(derived.sample_rate),
				cc_opt.unwrap_or(derived.channel_count),
			)
		}
	};

	let mut config = AudioConfig::new(codec, sample_rate, channel_count);
	config.bitrate = track.bitrate;
	config.description = legacy_description(track)?;
	config.container = container;
	config.jitter = track.jitter;
	Ok(Some(config))
}

/// Audio parameters derived from a track's `init_data`.
struct DerivedAudio {
	sample_rate: u32,
	channel_count: u32,
}

/// Derive sample rate and channel count from an MSF audio track's `init_data`.
///
/// - **Legacy / LOC**: the bytes are the codec config directly. For AAC we parse the
///   `AudioSpecificConfig` and for Opus we parse the `OpusHead`.
/// - **CMAF**: the bytes are an `ftyp+moov` init segment. We walk the moov to find the
///   audio `trak` and pull `sample_rate` / `channel_count` from its sample entry.
///
/// Returns an error if `init_data` is absent, malformed, or doesn't carry usable audio
/// parameters. The caller is expected to surface this as a hard failure rather than
/// substitute defaults: a wrong sample rate produces silent or distorted playback.
fn derive_audio_params(track: &moq_msf::Track, codec: &AudioCodec) -> Result<DerivedAudio> {
	let init = decode_init_data(track)?.ok_or_else(|| Error::MissingAudioParams(track.name.clone()))?;

	match track.packaging {
		moq_msf::Packaging::Loc | moq_msf::Packaging::Legacy => derive_from_codec_config(track, codec, init),
		moq_msf::Packaging::Cmaf => derive_from_cmaf_moov(track, init),
		_ => Err(Error::UnsupportedDerivationPackaging {
			name: track.name.clone(),
			packaging: format!("{:?}", track.packaging),
		}
		.into()),
	}
}

fn derive_from_codec_config(track: &moq_msf::Track, codec: &AudioCodec, init: bytes::Bytes) -> Result<DerivedAudio> {
	use bytes::Buf;
	let mut buf = init;
	match codec {
		AudioCodec::AAC(_) => {
			// AudioSpecificConfig carries valid variable-length extensions (SBR/PS) after
			// the core fields, so `parse` consumes the whole buffer; bytes past the core
			// fields are legitimate config, not trailing junk.
			let cfg =
				crate::codec::aac::Config::parse(&mut buf).map_err(|_| Error::MalformedAac(track.name.clone()))?;
			Ok(DerivedAudio {
				sample_rate: cfg.sample_rate,
				channel_count: cfg.channel_count,
			})
		}
		AudioCodec::Opus => {
			let cfg =
				crate::codec::opus::Config::parse(&mut buf).map_err(|_| Error::MalformedOpus(track.name.clone()))?;
			if buf.has_remaining() {
				return Err(Error::OpusTrailingBytes(track.name.clone()).into());
			}
			Ok(DerivedAudio {
				sample_rate: cfg.sample_rate,
				channel_count: cfg.channel_count,
			})
		}
		AudioCodec::Flac => {
			// The init data is the FLAC header (`fLaC` marker + STREAMINFO). `parse` stops
			// after STREAMINFO; trailing metadata blocks (Vorbis comments, etc.) are valid
			// and left unread, so unlike Opus there is no trailing-bytes check.
			let cfg =
				crate::codec::flac::Config::parse(&mut buf).map_err(|_| Error::MalformedFlac(track.name.clone()))?;
			Ok(DerivedAudio {
				sample_rate: cfg.sample_rate,
				channel_count: cfg.channel_count,
			})
		}
		_ => Err(Error::UnsupportedDerivationCodec(track.name.clone()).into()),
	}
}

fn derive_from_cmaf_moov(track: &moq_msf::Track, init: bytes::Bytes) -> Result<DerivedAudio> {
	use mp4_atom::{Any, DecodeMaybe};

	let mut cursor = std::io::Cursor::new(init.as_ref());
	let mut moov: Option<mp4_atom::Moov> = None;
	while let Some(atom) =
		mp4_atom::Any::decode_maybe(&mut cursor).map_err(|_| Error::MalformedInitSegment(track.name.clone()))?
	{
		if let Any::Moov(m) = atom {
			moov = Some(m);
			break;
		}
	}
	let moov = moov.ok_or_else(|| Error::MissingInitMoov(track.name.clone()))?;

	// Walk every trak looking for an audio sample entry. A single-track audio init is
	// the only thing we expect here, but rather than enforce that we just take the first
	// audio trak we find. The rest of the catalog identifies tracks by name, not by
	// position in the moov.
	for trak in &moov.trak {
		let stbl = &trak.mdia.minf.stbl;
		for sample in &stbl.stsd.codecs {
			match sample {
				mp4_atom::Codec::Mp4a(mp4a) => {
					return Ok(DerivedAudio {
						sample_rate: mp4a.audio.sample_rate.integer() as u32,
						channel_count: mp4a.audio.channel_count as u32,
					});
				}
				mp4_atom::Codec::Opus(opus) => {
					return Ok(DerivedAudio {
						sample_rate: opus.audio.sample_rate.integer() as u32,
						channel_count: opus.audio.channel_count as u32,
					});
				}
				_ => {}
			}
		}
	}
	Err(Error::MissingAudioSampleEntry(track.name.clone()).into())
}

#[cfg(test)]
mod test {
	use super::*;

	fn video_track(name: &str, packaging: moq_msf::Packaging, init_data: Option<&str>) -> moq_msf::Track {
		let mut track = moq_msf::Track::new(name, packaging);
		track.is_live = true;
		track.role = Some(moq_msf::Role::Video);
		track.codec = Some("avc1.640028".to_string());
		track.width = Some(1920);
		track.height = Some(1080);
		track.framerate = Some(30.0);
		track.bitrate = Some(5_000_000);
		track.init_data = init_data.map(str::to_string);
		track.render_group = Some(1);
		track
	}

	fn audio_track(name: &str, packaging: moq_msf::Packaging) -> moq_msf::Track {
		let mut track = moq_msf::Track::new(name, packaging);
		track.is_live = true;
		track.role = Some(moq_msf::Role::Audio);
		track.codec = Some("opus".to_string());
		track.samplerate = Some(48_000);
		track.channel_config = Some("2".to_string());
		track.bitrate = Some(128_000);
		track.render_group = Some(1);
		track
	}

	#[test]
	fn cmaf_video_yields_cmaf_container() {
		// "AAAYZ2Z0eXA=" decodes to a tiny ftyp-shaped stub; we just verify the bytes
		// round-trip through base64 into Container::Cmaf.init.
		let init_b64 = "AAAYZ2Z0eXA=";
		let expected_init = base64::engine::general_purpose::STANDARD.decode(init_b64).unwrap();

		let msf = moq_msf::Catalog::new(vec![video_track("video0", moq_msf::Packaging::Cmaf, Some(init_b64))]);

		let catalog = from_msf(&msf).expect("CMAF video should convert");
		let video = catalog.video.renditions.get("video0").expect("video0 rendition");

		match &video.container {
			Container::Cmaf { init, .. } => assert_eq!(init.as_ref(), expected_init.as_slice()),
			other => panic!("expected Cmaf container, got {other:?}"),
		}
		assert_eq!(video.coded_width, Some(1920));
		assert_eq!(video.coded_height, Some(1080));
		assert_eq!(video.framerate, Some(30.0));
		assert_eq!(video.bitrate, Some(5_000_000));
	}

	#[test]
	fn loc_audio_yields_legacy_container() {
		let msf = moq_msf::Catalog::new(vec![audio_track("audio0", moq_msf::Packaging::Loc)]);

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

		let msf = moq_msf::Catalog::new(vec![video, audio]);

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
		let msf = moq_msf::Catalog::new(vec![video_track("video0", moq_msf::Packaging::Cmaf, Some(init_b64))]);
		let catalog = from_msf(&msf).unwrap();
		assert!(catalog.video.renditions["video0"].description.is_none());
	}

	#[test]
	fn flac_params_derived_from_streaminfo() {
		// MSF omits samplerate/channelConfig, so they must be derived from the FLAC
		// header (`fLaC` marker + STREAMINFO) carried in init_data.
		let description = crate::codec::flac::Config {
			min_block_size: 4096,
			max_block_size: 4096,
			min_frame_size: 0,
			max_frame_size: 0,
			sample_rate: 96_000,
			channel_count: 2,
			bits_per_sample: 24,
			total_samples: 0,
			md5: [0; 16],
		}
		.description();
		let init_b64 = base64::engine::general_purpose::STANDARD.encode(&description);

		let mut audio = audio_track("audio0", moq_msf::Packaging::Loc);
		audio.codec = Some("flac".to_string());
		audio.samplerate = None;
		audio.channel_config = None;
		audio.init_data = Some(init_b64);

		let msf = moq_msf::Catalog::new(vec![audio]);

		let catalog = from_msf(&msf).expect("flac track should convert");
		let a = catalog.audio.renditions.get("audio0").expect("audio0 rendition");
		assert_eq!(a.codec, AudioCodec::Flac);
		assert_eq!(a.sample_rate, 96_000);
		assert_eq!(a.channel_count, 2);
	}

	#[test]
	fn legacy_malformed_init_data_is_error() {
		let mut track = video_track("video0", moq_msf::Packaging::Legacy, Some("!!!not-base64!!!"));
		track.codec = Some("avc1.42c01e".to_string());
		let msf = moq_msf::Catalog::new(vec![track]);
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
		let msf = moq_msf::Catalog::new(vec![track]);

		let catalog = from_msf(&msf).expect("unknown codec is not an error");
		let video = catalog.video.renditions.get("video0").expect("video0 rendition");
		assert_eq!(video.codec, VideoCodec::Unknown("weirdcodec".to_string()));
	}

	#[test]
	fn cmaf_without_init_data_is_error() {
		let msf = moq_msf::Catalog::new(vec![video_track("video0", moq_msf::Packaging::Cmaf, None)]);

		let err = from_msf(&msf).expect_err("CMAF without init_data must error");
		let msg = format!("{err:#}");
		assert!(msg.contains("init_data"), "expected init_data in error, got: {msg}");
	}

	#[test]
	fn empty_catalog_is_empty_hang_catalog() {
		let msf = moq_msf::Catalog::new(vec![]);

		let catalog = from_msf(&msf).expect("empty catalog should convert");
		assert!(catalog.video.renditions.is_empty());
		assert!(catalog.audio.renditions.is_empty());
	}

	#[test]
	fn track_without_role_is_skipped() {
		let mut track = video_track("video0", moq_msf::Packaging::Legacy, None);
		track.role = None;
		let msf = moq_msf::Catalog::new(vec![track]);

		let catalog = from_msf(&msf).expect("no-role track should be skipped, not error");
		assert!(catalog.video.renditions.is_empty());
		assert!(catalog.audio.renditions.is_empty());
	}

	#[test]
	fn unsupported_role_is_skipped() {
		let mut track = audio_track("caption0", moq_msf::Packaging::Legacy);
		track.role = Some(moq_msf::Role::Caption);
		let msf = moq_msf::Catalog::new(vec![track]);

		let catalog = from_msf(&msf).expect("unsupported role should be skipped, not error");
		assert!(catalog.audio.renditions.is_empty());
		assert!(catalog.video.renditions.is_empty());
	}

	#[test]
	fn audio_missing_samplerate_and_channels_without_init_data_errors() {
		// No explicit fields and no init_data to fall back on → hard failure
		// (defaults would produce silent or distorted playback).
		let mut track = audio_track("audio0", moq_msf::Packaging::Legacy);
		track.samplerate = None;
		track.channel_config = None;
		track.init_data = None;
		let msf = moq_msf::Catalog::new(vec![track]);

		let err = from_msf(&msf).expect_err("missing fields with no init_data should error");
		assert!(err.to_string().contains("no init_data"), "unexpected error: {}", err);
	}

	#[test]
	fn audio_missing_samplerate_and_channels_derived_from_opus_head() {
		// Legacy/LOC + Opus → parse OpusHead for samplerate and channels.
		// OpusHead layout: "OpusHead" magic, ver, channels, pre_skip(2), sample_rate(4 LE), ...
		let mut head = Vec::with_capacity(19);
		head.extend_from_slice(b"OpusHead");
		head.push(1); // version
		head.push(6); // channel_count (5.1)
		head.extend_from_slice(&0u16.to_le_bytes()); // pre_skip
		head.extend_from_slice(&24_000u32.to_le_bytes()); // sample_rate
		head.extend_from_slice(&[0, 0, 0]); // output gain (i16) + channel mapping family (1 byte)
		let init_b64 = base64::engine::general_purpose::STANDARD.encode(&head);

		let mut track = audio_track("audio0", moq_msf::Packaging::Loc);
		track.codec = Some("opus".to_string());
		track.samplerate = None;
		track.channel_config = None;
		track.init_data = Some(init_b64);
		let msf = moq_msf::Catalog::new(vec![track]);

		let catalog = from_msf(&msf).expect("Opus OpusHead should parse");
		let audio = catalog.audio.renditions.get("audio0").expect("audio0 rendition");
		assert_eq!(audio.sample_rate, 24_000);
		assert_eq!(audio.channel_count, 6);
	}

	#[test]
	fn audio_missing_samplerate_and_channels_derived_from_aac_config() {
		// Legacy + AAC → parse AudioSpecificConfig for samplerate and channels.
		// 2-byte ASC: object_type=2 (AAC LC), freq_index=3 (48000), channel_config=2 (stereo).
		// Layout: object_type(5 bits) | freq_index_hi(3 bits) ; freq_index_lo(1) | chan(4) | ext(3)
		// 2 = 0b00010, freq_index 3 = 0b0011, channel_config 2 = 0b0010
		// byte0 = 0b00010_000 (obj=2, freq_hi=000) | 0b001 (freq_hi from index 3 = 0b001) → 0b00010001 = 0x11
		// byte1 = 0b1_0010_000 (freq_lo=1, chan=0010, ext=000) = 0x90
		let asc = [0x11u8, 0x90];
		let init_b64 = base64::engine::general_purpose::STANDARD.encode(asc);

		let mut track = audio_track("audio0", moq_msf::Packaging::Legacy);
		track.codec = Some("mp4a.40.2".to_string());
		track.samplerate = None;
		track.channel_config = None;
		track.init_data = Some(init_b64);
		let msf = moq_msf::Catalog::new(vec![track]);

		let catalog = from_msf(&msf).expect("AAC AudioSpecificConfig should parse");
		let audio = catalog.audio.renditions.get("audio0").expect("audio0 rendition");
		assert_eq!(audio.sample_rate, 48_000);
		assert_eq!(audio.channel_count, 2);
	}

	#[test]
	fn audio_only_channels_missing_uses_explicit_samplerate() {
		// Half the fields missing: trust the explicit one, derive the missing one.
		let mut head = Vec::with_capacity(19);
		head.extend_from_slice(b"OpusHead");
		head.push(1);
		head.push(2); // channel_count derived = 2
		head.extend_from_slice(&0u16.to_le_bytes());
		head.extend_from_slice(&48_000u32.to_le_bytes()); // ignored: explicit samplerate wins
		head.extend_from_slice(&[0, 0, 0]);
		let init_b64 = base64::engine::general_purpose::STANDARD.encode(&head);

		let mut track = audio_track("audio0", moq_msf::Packaging::Loc);
		track.codec = Some("opus".to_string());
		track.samplerate = Some(24_000); // explicit, must be preserved
		track.channel_config = None; // missing, derive from init_data
		track.init_data = Some(init_b64);
		let msf = moq_msf::Catalog::new(vec![track]);

		let catalog = from_msf(&msf).expect("partial derivation should succeed");
		let audio = catalog.audio.renditions.get("audio0").expect("audio0 rendition");
		assert_eq!(audio.sample_rate, 24_000);
		assert_eq!(audio.channel_count, 2);
	}

	#[test]
	fn unsupported_packaging_video_is_skipped() {
		// MediaTimeline isn't a media payload, so the track must be skipped (not error).
		let bad = video_track("timeline0", moq_msf::Packaging::MediaTimeline, None);
		let good = video_track("video0", moq_msf::Packaging::Legacy, None);
		let msf = moq_msf::Catalog::new(vec![bad, good]);

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
		let msf = moq_msf::Catalog::new(vec![bad, good]);

		let catalog = from_msf(&msf).expect("unsupported packaging should be skipped, not error");
		assert!(!catalog.audio.renditions.contains_key("event0"));
		assert!(catalog.audio.renditions.contains_key("audio0"));
	}

	#[test]
	fn unknown_packaging_variant_is_skipped() {
		let track = video_track("video0", moq_msf::Packaging::Unknown("custom".to_string()), None);
		let msf = moq_msf::Catalog::new(vec![track]);

		let catalog = from_msf(&msf).expect("unknown packaging should be skipped, not error");
		assert!(catalog.video.renditions.is_empty());
	}

	#[test]
	fn missing_video_codec_is_error() {
		let mut track = video_track("video0", moq_msf::Packaging::Legacy, None);
		track.codec = None;
		let msf = moq_msf::Catalog::new(vec![track]);

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
		let msf = moq_msf::Catalog::new(vec![track]);

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
		let msf = moq_msf::Catalog::new(vec![track]);

		let err = from_msf(&msf).expect_err("malformed avc1 codec must error");
		let msg = format!("{err:#}");
		assert!(msg.contains("avc1.0"), "expected codec string in error, got: {msg}");
	}
}
