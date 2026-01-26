use crate::catalog::{AAC, AV1, AudioCodec, AudioConfig, Container, H264, H265, VP9, VideoCodec, VideoConfig};
use crate::{self as hang, Timestamp};
use anyhow::Context;
use bytes::{Buf, Bytes, BytesMut};
use moq_lite as moq;
use mp4_atom::{Any, Atom, DecodeMaybe, Mdat, Moof, Moov, Trak};
use std::collections::HashMap;
use tokio::io::{AsyncRead, AsyncReadExt};

/// Configuration for the fMP4 importer.
#[derive(Clone, Default)]
pub struct Fmp4Config {
	/// When true, transport fMP4 fragments directly (passthrough mode)
	///
	/// This requires a player that can decode the fragments directly.
	pub passthrough: bool,
}

/// Converts fMP4/CMAF files into hang broadcast streams.
///
/// This struct processes fragmented MP4 (fMP4) files and converts them into hang broadcasts.
/// Not all MP4 features are supported.
///
/// ## Supported Codecs
///
/// **Video:**
/// - H.264 (AVC1)
/// - H.265 (HEVC/HEV1/HVC1)
/// - VP8
/// - VP9
/// - AV1
///
/// **Audio:**
/// - AAC (MP4A)
/// - Opus
pub struct Fmp4 {
	// The broadcast being produced
	broadcast: hang::BroadcastProducer,

	// A lookup to tracks in the broadcast
	tracks: HashMap<u32, Fmp4Track>,

	// The moov atom at the start of the file.
	moov: Option<Moov>,

	// The latest moof header
	moof: Option<Moof>,
	moof_size: usize,

	/// Configuration for the fMP4 importer.
	config: Fmp4Config,

	// -- PASSTHROUGH ONLY --
	moof_raw: Option<Bytes>,
}

struct Fmp4Track {
	producer: hang::TrackProducer,

	// The current group being written, only used for passthrough mode.
	group: Option<moq_lite::GroupProducer>,

	// The minimum buffer required for the track.
	min_buffer: Option<Timestamp>,

	// The last timestamp seen for this track.
	last_timestamp: Option<Timestamp>,

	// The minimum duration between frames for this track.
	min_duration: Option<Timestamp>,
}

impl Fmp4Track {
	fn new(producer: hang::TrackProducer) -> Self {
		Self {
			producer,
			group: None,
			min_buffer: None,
			last_timestamp: None,
			min_duration: None,
		}
	}
}

impl Fmp4 {
	/// Create a new CMAF importer that will write to the given broadcast.
	///
	/// The broadcast will be populated with tracks as they're discovered in the
	/// fMP4 file. The catalog from the `hang::BroadcastProducer` is used automatically.
	pub fn new(broadcast: hang::BroadcastProducer, config: Fmp4Config) -> Self {
		Self {
			tracks: HashMap::default(),
			moov: None,
			moof: None,
			moof_size: 0,
			broadcast,
			config,
			moof_raw: None,
		}
	}

	/// Decode from an asynchronous reader.
	pub async fn decode_from<T: AsyncRead + Unpin>(&mut self, reader: &mut T) -> anyhow::Result<()> {
		let mut buffer = BytesMut::new();
		while reader.read_buf(&mut buffer).await? > 0 {
			self.decode(&mut buffer)?;
		}

		Ok(())
	}

	/// Decode a buffer of bytes.
	pub fn decode<T: Buf + AsRef<[u8]>>(&mut self, buf: &mut T) -> anyhow::Result<()> {
		let mut cursor = std::io::Cursor::new(buf);
		let mut position = 0;

		while let Some(atom) = mp4_atom::Any::decode_maybe(&mut cursor)? {
			// Process the parsed atom.
			let size = cursor.position() as usize - position;

			// The raw bytes of the atom we just parsed (not copied).
			let raw = &cursor.get_ref().as_ref()[position..position + size];

			match atom {
				Any::Ftyp(_) | Any::Styp(_) => {}
				Any::Moov(moov) => {
					self.init(moov)?;
				}
				Any::Moof(moof) => {
					anyhow::ensure!(self.moof.is_none(), "duplicate moof box");
					self.moof.replace(moof);
					self.moof_size = size;

					if self.config.passthrough {
						self.moof_raw.replace(Bytes::copy_from_slice(raw));
					}
				}
				Any::Mdat(mdat) => {
					self.extract(mdat, raw)?;
				}
				_ => {
					// Skip unknown atoms (e.g., sidx, which is optional and used for segment indexing)
					// These are safe to ignore and don't affect playback
				}
			}

			position = cursor.position() as usize;
		}

		// Advance the buffer by the amount of data that was processed.
		cursor.into_inner().advance(position);

		Ok(())
	}

	pub fn is_initialized(&self) -> bool {
		self.moov.is_some()
	}

	fn init(&mut self, moov: Moov) -> anyhow::Result<()> {
		// Make a clone to avoid the borrow checker.
		let mut catalog = self.broadcast.catalog.clone();
		let mut catalog = catalog.lock();

		// Track which specific tracks were created in this init call
		let mut created_video_tracks = Vec::new();
		let mut created_audio_tracks = Vec::new();

		for trak in &moov.trak {
			let track_id = trak.tkhd.track_id;
			let handler = &trak.mdia.hdlr.handler;

			let track = match handler.as_ref() {
				b"vide" => {
					let config = self.init_video(trak)?;

					let track = moq::Track {
						name: self.broadcast.track_name("video"),
						priority: 1,
					};

					let video = catalog.insert_video(track.name.clone(), config.clone());
					video.priority = 1;

					// Record this track name
					created_video_tracks.push(track.name.clone());

					let track = track.produce();
					self.broadcast.insert_track(track.consumer);
					hang::TrackProducer::new(track.producer)
				}
				b"soun" => {
					let config = self.init_audio(trak)?;

					let track = moq::Track {
						name: self.broadcast.track_name("audio"),
						priority: 2,
					};

					let audio = catalog.insert_audio(track.name.clone(), config.clone());
					audio.priority = 2;

					// Record this track name
					created_audio_tracks.push(track.name.clone());

					let track = track.produce();
					self.broadcast.insert_track(track.consumer);
					hang::TrackProducer::new(track.producer)
				}
				b"sbtl" => anyhow::bail!("subtitle tracks are not supported"),
				handler => anyhow::bail!("unknown track type: {:?}", handler),
			};

			self.tracks.insert(track_id, Fmp4Track::new(track));
		}

		self.moov = Some(moov);

		Ok(())
	}

	fn container(&self, trak: &Trak) -> Container {
		if self.config.passthrough {
			Container::Cmaf {
				timescale: trak.mdia.mdhd.timescale as u64,
				track_id: trak.tkhd.track_id,
			}
		} else {
			Container::Legacy
		}
	}

	fn init_video(&mut self, trak: &Trak) -> anyhow::Result<VideoConfig> {
		let container = self.container(trak);
		let stsd = &trak.mdia.minf.stbl.stsd;

		let codec = match stsd.codecs.len() {
			0 => anyhow::bail!("missing codec"),
			1 => &stsd.codecs[0],
			_ => anyhow::bail!("multiple codecs"),
		};

		let config = match codec {
			mp4_atom::Codec::Avc1(avc1) => {
				let avcc = &avc1.avcc;

				let mut description = BytesMut::new();
				avcc.encode_body(&mut description)?;

				VideoConfig {
					coded_width: Some(avc1.visual.width as _),
					coded_height: Some(avc1.visual.height as _),
					codec: H264 {
						profile: avcc.avc_profile_indication,
						constraints: avcc.profile_compatibility,
						level: avcc.avc_level_indication,
						inline: false,
					}
					.into(),
					description: Some(description.freeze()),
					// TODO: populate these fields
					framerate: None,
					bitrate: None,
					display_ratio_width: None,
					display_ratio_height: None,
					optimize_for_latency: None,
					container,
					min_buffer: None,
				}
			}
			mp4_atom::Codec::Hev1(hev1) => self.init_h265(true, &hev1.hvcc, &hev1.visual, container)?,
			mp4_atom::Codec::Hvc1(hvc1) => self.init_h265(false, &hvc1.hvcc, &hvc1.visual, container)?,
			mp4_atom::Codec::Vp08(vp08) => VideoConfig {
				codec: VideoCodec::VP8,
				description: Default::default(),
				coded_width: Some(vp08.visual.width as _),
				coded_height: Some(vp08.visual.height as _),
				// TODO: populate these fields
				framerate: None,
				bitrate: None,
				display_ratio_width: None,
				display_ratio_height: None,
				optimize_for_latency: None,
				container,
				min_buffer: None,
			},
			mp4_atom::Codec::Vp09(vp09) => {
				// https://github.com/gpac/mp4box.js/blob/325741b592d910297bf609bc7c400fc76101077b/src/box-codecs.js#L238
				let vpcc = &vp09.vpcc;

				VideoConfig {
					codec: VP9 {
						profile: vpcc.profile,
						level: vpcc.level,
						bit_depth: vpcc.bit_depth,
						color_primaries: vpcc.color_primaries,
						chroma_subsampling: vpcc.chroma_subsampling,
						transfer_characteristics: vpcc.transfer_characteristics,
						matrix_coefficients: vpcc.matrix_coefficients,
						full_range: vpcc.video_full_range_flag,
					}
					.into(),
					description: Default::default(),
					coded_width: Some(vp09.visual.width as _),
					coded_height: Some(vp09.visual.height as _),
					// TODO: populate these fields
					display_ratio_width: None,
					display_ratio_height: None,
					optimize_for_latency: None,
					bitrate: None,
					framerate: None,
					container,
					min_buffer: None,
				}
			}
			mp4_atom::Codec::Av01(av01) => {
				let av1c = &av01.av1c;

				VideoConfig {
					codec: AV1 {
						profile: av1c.seq_profile,
						level: av1c.seq_level_idx_0,
						bitdepth: match (av1c.seq_tier_0, av1c.high_bitdepth) {
							(true, true) => 12,
							(true, false) => 10,
							(false, true) => 10,
							(false, false) => 8,
						},
						mono_chrome: av1c.monochrome,
						chroma_subsampling_x: av1c.chroma_subsampling_x,
						chroma_subsampling_y: av1c.chroma_subsampling_y,
						chroma_sample_position: av1c.chroma_sample_position,
						// TODO HDR stuff?
						..Default::default()
					}
					.into(),
					description: Default::default(),
					coded_width: Some(av01.visual.width as _),
					coded_height: Some(av01.visual.height as _),
					// TODO: populate these fields
					display_ratio_width: None,
					display_ratio_height: None,
					optimize_for_latency: None,
					bitrate: None,
					framerate: None,
					container,
					min_buffer: None,
				}
			}
			mp4_atom::Codec::Unknown(unknown) => anyhow::bail!("unknown codec: {:?}", unknown),
			unsupported => anyhow::bail!("unsupported codec: {:?}", unsupported),
		};

		Ok(config)
	}

	// There's two almost identical hvcc atoms in the wild.
	fn init_h265(
		&mut self,
		in_band: bool,
		hvcc: &mp4_atom::Hvcc,
		visual: &mp4_atom::Visual,
		container: Container,
	) -> anyhow::Result<VideoConfig> {
		let mut description = BytesMut::new();
		hvcc.encode_body(&mut description)?;

		Ok(VideoConfig {
			codec: H265 {
				in_band,
				profile_space: hvcc.general_profile_space,
				profile_idc: hvcc.general_profile_idc,
				profile_compatibility_flags: hvcc.general_profile_compatibility_flags,
				tier_flag: hvcc.general_tier_flag,
				level_idc: hvcc.general_level_idc,
				constraint_flags: hvcc.general_constraint_indicator_flags,
			}
			.into(),
			description: Some(description.freeze()),
			coded_width: Some(visual.width as _),
			coded_height: Some(visual.height as _),
			// TODO: populate these fields
			bitrate: None,
			framerate: None,
			display_ratio_width: None,
			display_ratio_height: None,
			optimize_for_latency: None,
			container,
			min_buffer: None,
		})
	}

	fn init_audio(&mut self, trak: &Trak) -> anyhow::Result<AudioConfig> {
		let container = self.container(trak);
		let stsd = &trak.mdia.minf.stbl.stsd;

		let codec = match stsd.codecs.len() {
			0 => anyhow::bail!("missing codec"),
			1 => &stsd.codecs[0],
			_ => anyhow::bail!("multiple codecs"),
		};

		let config = match codec {
			mp4_atom::Codec::Mp4a(mp4a) => {
				let desc = &mp4a.esds.es_desc.dec_config;

				// TODO Also support mp4a.67
				if desc.object_type_indication != 0x40 {
					anyhow::bail!("unsupported codec: MPEG2");
				}

				let bitrate = desc.avg_bitrate.max(desc.max_bitrate);

				AudioConfig {
					codec: AAC {
						profile: desc.dec_specific.profile,
					}
					.into(),
					sample_rate: mp4a.audio.sample_rate.integer() as _,
					channel_count: mp4a.audio.channel_count as _,
					bitrate: Some(bitrate.into()),
					description: None, // TODO?
					container,
					min_buffer: None,
				}
			}
			mp4_atom::Codec::Opus(opus) => {
				AudioConfig {
					codec: AudioCodec::Opus,
					sample_rate: opus.audio.sample_rate.integer() as _,
					channel_count: opus.audio.channel_count as _,
					bitrate: None,
					description: None, // TODO?
					container,
					min_buffer: None,
				}
			}
			mp4_atom::Codec::Unknown(unknown) => anyhow::bail!("unknown codec: {:?}", unknown),
			unsupported => anyhow::bail!("unsupported codec: {:?}", unsupported),
		};

		Ok(config)
	}

	// Extract all frames out of an mdat atom.
	fn extract(&mut self, mdat: Mdat, mdat_raw: &[u8]) -> anyhow::Result<()> {
		let moov = self.moov.as_ref().context("missing moov box")?;
		let moof = self.moof.take().context("missing moof box")?;
		let moof_size = self.moof_size;
		let header_size = mdat_raw.len() - mdat.data.len();

		// Loop over all of the traf boxes in the moof.
		for traf in &moof.traf {
			let track_id = traf.tfhd.track_id;
			let track = self.tracks.get_mut(&track_id).context("unknown track")?;

			// Find the track information in the moov
			let trak = moov
				.trak
				.iter()
				.find(|trak| trak.tkhd.track_id == track_id)
				.context("unknown track")?;
			let trex = moov
				.mvex
				.as_ref()
				.and_then(|mvex| mvex.trex.iter().find(|trex| trex.track_id == track_id));

			// The moov contains some defaults
			let default_sample_duration = trex.map(|trex| trex.default_sample_duration).unwrap_or_default();
			let default_sample_size = trex.map(|trex| trex.default_sample_size).unwrap_or_default();
			let default_sample_flags = trex.map(|trex| trex.default_sample_flags).unwrap_or_default();

			let tfdt = traf.tfdt.as_ref().context("missing tfdt box")?;
			let mut dts = tfdt.base_media_decode_time;
			let timescale = trak.mdia.mdhd.timescale as u64;

			let mut offset = traf.tfhd.base_data_offset.unwrap_or_default() as usize;

			if traf.trun.is_empty() {
				anyhow::bail!("missing trun box");
			}

			// Keep track of the minimum and maximum timestamp for this track to compute the min_buffer.
			// Ideally these should both be the same value (a single frame lul).
			let mut min_timestamp = None;
			let mut max_timestamp = None;
			let mut contains_keyframe = false;

			for trun in &traf.trun {
				let tfhd = &traf.tfhd;

				if let Some(data_offset) = trun.data_offset {
					let base_offset = tfhd.base_data_offset.unwrap_or_default() as usize;
					// This is relative to the start of the MOOF, not the MDAT.
					// Note: The trun data offset can be negative, but... that's not supported here.
					let data_offset: usize = data_offset.try_into().context("invalid data offset")?;

					// Use checked arithmetic to prevent underflow
					let relative_offset = data_offset
						.checked_sub(moof_size)
						.and_then(|v| v.checked_sub(header_size))
						.context("invalid data offset: underflow")?;

					// Reset offset if the TRUN has a data offset
					offset = base_offset
						.checked_add(relative_offset)
						.context("invalid data offset: overflow")?;
				}

				for entry in &trun.entries {
					// Use the moof defaults if the sample doesn't have its own values.
					let flags = entry
						.flags
						.unwrap_or(tfhd.default_sample_flags.unwrap_or(default_sample_flags));
					let duration = entry
						.duration
						.unwrap_or(tfhd.default_sample_duration.unwrap_or(default_sample_duration));
					let size = entry
						.size
						.unwrap_or(tfhd.default_sample_size.unwrap_or(default_sample_size)) as usize;

					let pts = (dts as i64 + entry.cts.unwrap_or_default() as i64) as u64;
					let timestamp = hang::Timestamp::from_scale(pts, timescale)?;

					if offset + size > mdat.data.len() {
						anyhow::bail!("invalid data offset");
					}

					let keyframe = if trak.mdia.hdlr.handler == b"vide".into() {
						// https://chromium.googlesource.com/chromium/src/media/+/master/formats/mp4/track_run_iterator.cc#177
						let keyframe = (flags >> 24) & 0x3 == 0x2; // kSampleDependsOnNoOther
						let non_sync = (flags >> 16) & 0x1 == 0x1; // kSampleIsNonSyncSample

						keyframe && !non_sync
					} else {
						// Audio frames are always keyframes.
						true
					};

					contains_keyframe |= keyframe;

					if !self.config.passthrough {
						// TODO Avoid a copy if mp4-atom uses Bytes?
						let payload = Bytes::copy_from_slice(&mdat.data[offset..(offset + size)]);

						let frame = hang::Frame {
							timestamp,
							keyframe,
							payload: payload.into(),
						};
						track.producer.write(frame)?;
					}

					if timestamp >= max_timestamp.unwrap_or(Timestamp::ZERO) {
						max_timestamp = Some(timestamp);
					}
					if timestamp <= min_timestamp.unwrap_or(Timestamp::MAX) {
						min_timestamp = Some(timestamp);
					}

					if let Some(last_timestamp) = track.last_timestamp
						&& let Ok(duration) = timestamp.checked_sub(last_timestamp)
						&& duration < track.min_duration.unwrap_or(Timestamp::MAX)
					{
						track.min_duration = Some(duration);
					}

					track.last_timestamp = Some(timestamp);

					dts += duration as u64;
					offset += size;
				}
			}

			// If we're doing passthrough mode, then we write one giant fragment instead of individual frames.
			if self.config.passthrough {
				let mut group = if contains_keyframe {
					if let Some(group) = track.group.take() {
						group.close();
					}

					track.producer.inner.append_group()
				} else {
					track.group.take().context("no keyframe at start")?
				};

				let moof_raw = self.moof_raw.as_ref().context("missing moof box")?;

				// To avoid an extra allocation, we use the chunked API to write the moof and mdat atoms separately.
				let mut frame = group.create_frame(moq_lite::Frame {
					size: moof_raw.len() as u64 + mdat_raw.len() as u64,
				});

				frame.write_chunk(moof_raw.clone());
				frame.write_chunk(Bytes::copy_from_slice(mdat_raw));
				frame.close();

				track.group = Some(group);
			}

			if let (Some(min), Some(max), Some(min_duration)) = (min_timestamp, max_timestamp, track.min_duration) {
				// We report the minimum buffer required as the difference between the min and max frames.
				// We also add the duration between frames to account for the frame rate.
				// ex. for 2s fragments, this should be exactly 2s if we did everything correctly.
				let min_buffer = max - min + min_duration;

				if min_buffer < track.min_buffer.unwrap_or(Timestamp::MAX) {
					track.min_buffer = Some(min_buffer);

					// Update the catalog with the new min_buffer
					let mut catalog = self.broadcast.catalog.lock();

					// We're lazy and don't keep track if this track is for audio or video, so just try to update both.
					if let Some(video) = catalog.video.as_mut()
						&& let Some(config) = video.renditions.get_mut(&track.producer.info.name)
					{
						config.min_buffer = Some(min_buffer.convert()?);
					}

					if let Some(audio) = catalog.audio.as_mut()
						&& let Some(config) = audio.renditions.get_mut(&track.producer.info.name)
					{
						config.min_buffer = Some(min_buffer.convert()?);
					}
				}
			}
		}

		Ok(())
	}
}

impl Drop for Fmp4 {
	fn drop(&mut self) {
		let mut catalog = self.broadcast.catalog.lock();

		for track in self.tracks.values() {
			// We're too lazy to keep track of if this track is for audio or video, so we just remove both.
			catalog.remove_video(&track.producer.info.name);
			catalog.remove_audio(&track.producer.info.name);
		}
	}
}
