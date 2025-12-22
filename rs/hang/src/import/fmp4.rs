use crate::catalog::{AudioCodec, AudioConfig, CatalogProducer, VideoCodec, VideoConfig, AAC, AV1, H264, H265, VP9};
use crate::{self as hang, Timestamp};
use anyhow::Context;
use bytes::{Buf, Bytes, BytesMut};
use moq_lite as moq;
use mp4_atom::{Any, Atom, DecodeMaybe, Mdat, Moof, Moov, Trak};
use std::collections::HashMap;

/// Mode for importing fMP4 content.
///
/// This determines how frames are transmitted over MOQ:
/// - `Frames`: Individual frames for WebCodecs (lower latency)
/// - `Segments`: Complete fMP4 segments for MSE (broader compatibility)
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ImportMode {
	/// Extract individual frames from segments (default, for WebCodecs).
	///
	/// Each frame is sent separately with a timestamp header.
	/// This provides the lowest latency but requires WebCodecs on the client.
	#[default]
	Frames,

	/// Send complete fMP4 segments (moof+mdat) for MSE.
	///
	/// Init segment (ftyp+moov) is sent at each keyframe.
	/// This is compatible with MSE-based players but has slightly higher latency.
	Segments,
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
///
/// ## Import Modes
///
/// - [`ImportMode::Frames`]: Extract individual frames (for WebCodecs)
/// - [`ImportMode::Segments`]: Send complete fMP4 segments (for MSE)
pub struct Fmp4 {
	// The broadcast being produced
	// This `hang` variant includes a catalog.
	broadcast: hang::BroadcastProducer,

	// A clone of the broadcast's catalog for mutable access.
	// This is the same underlying catalog (via Arc), just a separate binding.
	catalog: CatalogProducer,

	// A lookup to tracks in the broadcast
	tracks: HashMap<u32, hang::TrackProducer>,

	// The timestamp of the last keyframe for each track
	last_keyframe: HashMap<u32, hang::Timestamp>,

	// The moov atom at the start of the file.
	moov: Option<Moov>,

	// The latest moof header
	moof: Option<Moof>,
	moof_size: usize,

	// --- MSE Segment Mode fields ---
	// Import mode (frames vs segments)
	mode: ImportMode,

	// Buffer to accumulate raw moof bytes (for Segments mode)
	moof_buffer: BytesMut,

	// Buffer to store ftyp box for init segment
	ftyp_buffer: BytesMut,

	// Per-track init segments (ftyp + moov with single track) for MSE mode
	// Key is track_id, value is complete init segment bytes
	track_init_segments: HashMap<u32, Bytes>,
}

impl Fmp4 {
	/// Create a new CMAF importer that will write to the given broadcast.
	///
	/// The broadcast will be populated with tracks as they're discovered in the
	/// fMP4 file. The catalog from the `hang::BroadcastProducer` is used automatically.
	///
	/// Uses [`ImportMode::Frames`] by default (for WebCodecs).
	/// Use [`Self::with_mode`] to specify a different mode.
	pub fn new(broadcast: hang::BroadcastProducer) -> Self {
		Self::with_mode(broadcast, ImportMode::default())
	}

	/// Create a new CMAF importer with a specific import mode.
	///
	/// # Arguments
	///
	/// * `broadcast` - The broadcast to write tracks to
	/// * `mode` - The import mode:
	///   - [`ImportMode::Frames`]: Extract individual frames (for WebCodecs)
	///   - [`ImportMode::Segments`]: Send complete fMP4 segments (for MSE)
	pub fn with_mode(broadcast: hang::BroadcastProducer, mode: ImportMode) -> Self {
		let catalog = broadcast.catalog.clone();
		Self {
			broadcast,
			catalog,
			tracks: HashMap::default(),
			last_keyframe: HashMap::default(),
			moov: None,
			moof: None,
			moof_size: 0,
			mode,
			moof_buffer: BytesMut::new(),
			ftyp_buffer: BytesMut::new(),
			track_init_segments: HashMap::default(),
		}
	}

	pub fn decode<T: Buf + AsRef<[u8]>>(&mut self, buf: &mut T) -> anyhow::Result<()> {
		let mut cursor = std::io::Cursor::new(buf);
		let mut position = 0;

		while let Some(atom) = mp4_atom::Any::decode_maybe(&mut cursor)? {
			// Process the parsed atom.
			let size = cursor.position() as usize - position;
			let atom_start = position;
			position = cursor.position() as usize;

			match atom {
				Any::Ftyp(_) | Any::Styp(_) => {
					// In Segments mode, capture ftyp bytes for init segment
					if self.mode == ImportMode::Segments && self.ftyp_buffer.is_empty() {
						let data = cursor.get_ref().as_ref();
						self.ftyp_buffer.extend_from_slice(&data[atom_start..position]);
						tracing::debug!("Captured ftyp/styp box: {} bytes", size);
					}
				}
				Any::Moov(moov) => {
					// Create the broadcast first
					self.init(moov.clone())?;

					// In Segments mode, create per-track init segments
					if self.mode == ImportMode::Segments && self.track_init_segments.is_empty() {
						self.create_per_track_init_segments(&moov)?;
					}
				}
				Any::Moof(moof) => {
					if self.moof.is_some() {
						// Two moof boxes in a row.
						anyhow::bail!("duplicate moof box");
					}

					// In Segments mode, capture moof bytes
					if self.mode == ImportMode::Segments {
						self.moof_buffer.clear();
						let data = cursor.get_ref().as_ref();
						self.moof_buffer.extend_from_slice(&data[atom_start..position]);
					}

					self.moof = Some(moof);
					self.moof_size = size;
				}
				Any::Mdat(mdat) => {
					// Extract the samples from the mdat atom.
					let header_size = size - mdat.data.len();
					match self.mode {
						ImportMode::Frames => self.extract(mdat, header_size)?,
						ImportMode::Segments => self.extract_segment(mdat, header_size)?,
					}
				}
				_ => {
					// Skip unknown atoms
					tracing::warn!(?atom, "skipping")
				}
			}
		}

		// Advance the buffer by the amount of data that was processed.
		cursor.into_inner().advance(position);

		Ok(())
	}

	pub fn is_initialized(&self) -> bool {
		self.moov.is_some()
	}

	fn init(&mut self, moov: Moov) -> anyhow::Result<()> {
		let mut catalog = self.catalog.lock();

		for trak in &moov.trak {
			let track_id = trak.tkhd.track_id;
			let handler = &trak.mdia.hdlr.handler;

			let track = match handler.as_ref() {
				b"vide" => {
					let config = Self::init_video(trak)?;

					let track = moq::Track {
						name: self.broadcast.track_name("video"),
						priority: 1,
					};

					tracing::debug!(name = ?track.name, ?config, "starting track");

					let video = catalog.insert_video(track.name.clone(), config);
					video.priority = 1;

					let track = track.produce();
					self.broadcast.insert_track(track.consumer);
					track.producer
				}
				b"soun" => {
					let config = Self::init_audio(trak)?;

					let track = moq::Track {
						name: self.broadcast.track_name("audio"),
						priority: 1,
					};

					tracing::debug!(name = ?track.name, ?config, "starting track");

					let audio = catalog.insert_audio(track.name.clone(), config);
					audio.priority = 1;

					let track = track.produce();
					self.broadcast.insert_track(track.consumer);
					track.producer
				}
				b"sbtl" => anyhow::bail!("subtitle tracks are not supported"),
				handler => anyhow::bail!("unknown track type: {:?}", handler),
			};

			self.tracks.insert(track_id, track.into());
		}

		self.moov = Some(moov);

		Ok(())
	}

	/// Create per-track init segments for MSE mode.
	/// Each track needs its own ftyp + moov with only that track's info.
	fn create_per_track_init_segments(&mut self, moov: &Moov) -> anyhow::Result<()> {
		use mp4_atom::Encode;

		for trak in &moov.trak {
			let track_id = trak.tkhd.track_id;

			// Create a new moov with only this track
			let single_track_moov = Moov {
				mvhd: moov.mvhd.clone(),
				mvex: moov.mvex.clone(),
				trak: vec![trak.clone()],
				udta: moov.udta.clone(),
				meta: moov.meta.clone(),
			};

			// Encode the single-track moov to bytes
			let mut moov_bytes = BytesMut::new();
			single_track_moov.encode(&mut moov_bytes)?;

			// Create the complete init segment: ftyp + moov
			let mut init_segment = BytesMut::new();
			init_segment.extend_from_slice(&self.ftyp_buffer);
			init_segment.extend_from_slice(&moov_bytes);

			tracing::debug!(
				"Created init segment for track {}: {} bytes (ftyp: {}, moov: {})",
				track_id,
				init_segment.len(),
				self.ftyp_buffer.len(),
				moov_bytes.len()
			);

			self.track_init_segments.insert(track_id, init_segment.freeze());
		}

		Ok(())
	}

	fn init_video(trak: &Trak) -> anyhow::Result<VideoConfig> {
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
				}
			}
			mp4_atom::Codec::Hev1(hev1) => Self::init_h265(true, &hev1.hvcc, &hev1.visual)?,
			mp4_atom::Codec::Hvc1(hvc1) => Self::init_h265(false, &hvc1.hvcc, &hvc1.visual)?,
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
				}
			}
			mp4_atom::Codec::Unknown(unknown) => anyhow::bail!("unknown codec: {:?}", unknown),
			unsupported => anyhow::bail!("unsupported codec: {:?}", unsupported),
		};

		Ok(config)
	}

	// There's two almost identical hvcc atoms in the wild.
	fn init_h265(in_band: bool, hvcc: &mp4_atom::Hvcc, visual: &mp4_atom::Visual) -> anyhow::Result<VideoConfig> {
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
		})
	}

	fn init_audio(trak: &Trak) -> anyhow::Result<AudioConfig> {
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
				}
			}
			mp4_atom::Codec::Opus(opus) => {
				AudioConfig {
					codec: AudioCodec::Opus,
					sample_rate: opus.audio.sample_rate.integer() as _,
					channel_count: opus.audio.channel_count as _,
					bitrate: None,
					description: None, // TODO?
				}
			}
			mp4_atom::Codec::Unknown(unknown) => anyhow::bail!("unknown codec: {:?}", unknown),
			unsupported => anyhow::bail!("unsupported codec: {:?}", unsupported),
		};

		Ok(config)
	}

	// Extract all frames out of an mdat atom.
	fn extract(&mut self, mdat: Mdat, header_size: usize) -> anyhow::Result<()> {
		let mdat = Bytes::from(mdat.data);
		let moov = self.moov.as_ref().context("missing moov box")?;
		let moof = self.moof.take().context("missing moof box")?;

		// Keep track of the minimum and maximum timestamp so we can scold the user.
		// Ideally these should both be the same value.
		let mut min_timestamp = None;
		let mut max_timestamp = None;

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
			for trun in &traf.trun {
				let tfhd = &traf.tfhd;

				if let Some(data_offset) = trun.data_offset {
					let base_offset = tfhd.base_data_offset.unwrap_or_default() as usize;
					// This is relative to the start of the MOOF, not the MDAT.
					// Note: The trun data offset can be negative, but... that's not supported here.
					let data_offset: usize = data_offset.try_into().context("invalid data offset")?;
					if data_offset < self.moof_size {
						anyhow::bail!("invalid data offset");
					}
					// Reset offset if the TRUN has a data offset
					offset = base_offset + data_offset - self.moof_size - header_size;
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
					let micros = (pts as u128 * 1_000_000 / timescale as u128) as u64;
					let timestamp = hang::Timestamp::from_micros(micros)?;

					if offset + size > mdat.len() {
						anyhow::bail!("invalid data offset");
					}

					let keyframe = if trak.mdia.hdlr.handler == b"vide".into() {
						// https://chromium.googlesource.com/chromium/src/media/+/master/formats/mp4/track_run_iterator.cc#177
						let keyframe = (flags >> 24) & 0x3 == 0x2; // kSampleDependsOnNoOther
						let non_sync = (flags >> 16) & 0x1 == 0x1; // kSampleIsNonSyncSample

						if keyframe && !non_sync {
							for audio in moov.trak.iter().filter(|t| t.mdia.hdlr.handler == b"soun".into()) {
								// Force an audio keyframe on video keyframes
								self.last_keyframe.remove(&audio.tkhd.track_id);
							}

							true
						} else {
							false
						}
					} else {
						match self.last_keyframe.get(&track_id) {
							// Force an audio keyframe at least every 10 seconds, but ideally at video keyframes
							Some(prev) => timestamp - *prev > Timestamp::from_secs(10).unwrap(),
							None => true,
						}
					};

					if keyframe {
						self.last_keyframe.insert(track_id, timestamp);
					}

					let payload = mdat.slice(offset..(offset + size));

					let frame = hang::Frame {
						timestamp,
						keyframe,
						payload: payload.into(),
					};
					track.write(frame)?;

					dts += duration as u64;
					offset += size;

					if timestamp >= max_timestamp.unwrap_or_default() {
						max_timestamp = Some(timestamp);
					}
					if timestamp <= min_timestamp.unwrap_or_default() {
						min_timestamp = Some(timestamp);
					}
				}
			}
		}

		if let (Some(min), Some(max)) = (min_timestamp, max_timestamp) {
			let diff = max - min;

			if diff > Timestamp::from_millis(1).unwrap() {
				tracing::warn!("fMP4 introduced {:?} of latency", diff);
			}
		}

		Ok(())
	}

	/// Extract complete fMP4 segment (moof + mdat) as a single frame.
	///
	/// Used when [`ImportMode::Segments`] is selected for MSE compatibility.
	fn extract_segment(&mut self, mdat: Mdat, _header_size: usize) -> anyhow::Result<()> {
		let mdat_data = Bytes::from(mdat.data);
		let moov = self.moov.as_ref().context("missing moov box")?;
		let moof = self.moof.take().context("missing moof box")?;

		// Loop over ALL tracks in moof
		for traf in &moof.traf {
			let track_id = traf.tfhd.track_id;

			// Check if the track exists (immutable borrow)
			if !self.tracks.contains_key(&track_id) {
				tracing::warn!("Unknown track_id: {}, skipping", track_id);
				continue;
			}

			// Find the track information in the moov
			let trak = moov
				.trak
				.iter()
				.find(|trak| trak.tkhd.track_id == track_id)
				.context("unknown track")?;

			let tfdt = traf.tfdt.as_ref().context("missing tfdt box")?;
			let dts = tfdt.base_media_decode_time;
			let timescale = trak.mdia.mdhd.timescale as u64;

			// Calculate timestamp from DTS
			let micros = (dts as u128 * 1_000_000 / timescale as u128) as u64;
			let timestamp = hang::Timestamp::from_micros(micros)?;

			// Check if this is a keyframe segment by looking at first sample flags
			let keyframe = if let Some(trun) = traf.trun.first() {
				if let Some(entry) = trun.entries.first() {
					let trex = moov
						.mvex
						.as_ref()
						.and_then(|mvex| mvex.trex.iter().find(|trex| trex.track_id == track_id));
					let default_sample_flags = trex.map(|trex| trex.default_sample_flags).unwrap_or_default();

					let flags = entry
						.flags
						.unwrap_or(traf.tfhd.default_sample_flags.unwrap_or(default_sample_flags));

					if trak.mdia.hdlr.handler == b"vide".into() {
						let is_keyframe = (flags >> 24) & 0x3 == 0x2; // kSampleDependsOnNoOther
						let non_sync = (flags >> 16) & 0x1 == 0x1; // kSampleIsNonSyncSample

						if is_keyframe && !non_sync {
							// Force audio keyframe when video has a keyframe (for sync)
							for audio in moov.trak.iter().filter(|t| t.mdia.hdlr.handler == b"soun".into()) {
								self.last_keyframe.remove(&audio.tkhd.track_id);
							}
							true
						} else {
							false
						}
					} else {
						// Audio - check if it's been 10 seconds since last keyframe
						// OR if video just had a keyframe (last_keyframe was cleared)
						match self.last_keyframe.get(&track_id) {
							Some(prev) => timestamp - *prev > Timestamp::from_secs(10).unwrap(),
							None => true, // No previous keyframe = force one now
						}
					}
				} else {
					false
				}
			} else {
				false
			};

			if keyframe {
				self.last_keyframe.insert(track_id, timestamp);
			}

			// Create complete fMP4 segment
			let mut segment_data = BytesMut::new();

			// For keyframes, prepend the per-track init segment to make each
			// keyframe self-contained. Each track has its own init segment with
			// only that track's moov info - this is required for MSE with separate
			// SourceBuffers.
			if keyframe {
				if let Some(init_segment) = self.track_init_segments.get(&track_id) {
					segment_data.extend_from_slice(init_segment);
					tracing::debug!(
						"Prepending per-track init segment to keyframe for track {}: {} bytes",
						track_id,
						init_segment.len()
					);
				}
			}

			// Build per-track moof and mdat containing only this track's data.
			// This is required for MSE with demuxed SourceBuffers.
			let (per_track_moof_bytes, per_track_mdat_bytes) =
				self.build_per_track_segment(&moof.mfhd, traf, &mdat_data)?;

			segment_data.extend_from_slice(&per_track_moof_bytes);
			segment_data.extend_from_slice(&per_track_mdat_bytes);

			tracing::debug!(
				"Sending fMP4 segment: {} bytes (moof: {}, mdat: {}), keyframe: {}, timestamp: {:?}",
				segment_data.len(),
				per_track_moof_bytes.len(),
				per_track_mdat_bytes.len(),
				keyframe,
				timestamp
			);

			let frame = hang::Frame {
				timestamp,
				keyframe,
				payload: segment_data.freeze().into(),
			};

			// Now get mutable reference to write the frame
			let track = self.tracks.get_mut(&track_id).context("track disappeared")?;
			track.write(frame)?;
		}

		Ok(())
	}

	/// Build a per-track moof and mdat pair containing only the specified track's data.
	///
	/// This is required for MSE with demuxed SourceBuffers, where each buffer
	/// expects to receive data for only its track.
	fn build_per_track_segment(
		&self,
		mfhd: &mp4_atom::Mfhd,
		traf: &mp4_atom::Traf,
		global_mdat: &Bytes,
	) -> anyhow::Result<(Bytes, Bytes)> {
		use mp4_atom::Encode;

		let moov = self.moov.as_ref().context("missing moov box")?;
		let track_id = traf.tfhd.track_id;
		let trex = moov
			.mvex
			.as_ref()
			.and_then(|mvex| mvex.trex.iter().find(|trex| trex.track_id == track_id));
		let default_sample_size = trex.map(|trex| trex.default_sample_size).unwrap_or_default();

		// First, extract all sample data for this track from the global mdat
		let mut track_mdat_data = BytesMut::new();
		let mut offset = traf.tfhd.base_data_offset.unwrap_or_default() as usize;

		for trun in &traf.trun {
			if let Some(data_offset) = trun.data_offset {
				let base_offset = traf.tfhd.base_data_offset.unwrap_or_default() as usize;
				// data_offset is relative to the start of the moof
				let data_offset_usize: usize = data_offset.try_into().context("invalid data offset")?;
				if data_offset_usize < self.moof_size {
					anyhow::bail!("invalid data offset: {} < moof_size {}", data_offset_usize, self.moof_size);
				}
				// Calculate offset into mdat (moof_size is subtracted, and we also need to
				// account for the 8-byte mdat header that was stripped when parsing)
				offset = base_offset + data_offset_usize - self.moof_size - 8;
			}

			for entry in &trun.entries {
				let size = entry
					.size
					.unwrap_or(traf.tfhd.default_sample_size.unwrap_or(default_sample_size)) as usize;

				if offset + size > global_mdat.len() {
					anyhow::bail!(
						"invalid sample offset: {} + {} > {}",
						offset,
						size,
						global_mdat.len()
					);
				}

				track_mdat_data.extend_from_slice(&global_mdat[offset..offset + size]);
				offset += size;
			}
		}

		// Now build the per-track moof with updated trun data_offsets
		// Clone traf and update trun data_offsets
		let mut new_traf = traf.clone();

		// Create a temporary moof to calculate its size
		let temp_moof = Moof {
			mfhd: mfhd.clone(),
			traf: vec![new_traf.clone()],
		};
		let mut temp_moof_bytes = BytesMut::new();
		temp_moof.encode(&mut temp_moof_bytes)?;
		let new_moof_size = temp_moof_bytes.len();

		// Now update the trun data_offsets to point into the new per-track mdat
		// data_offset should be: new_moof_size + 8 (mdat header) + offset_within_track_mdat
		let mut current_offset_in_track_mdat: i32 = 0;
		for trun in &mut new_traf.trun {
			// Set data_offset to point to the start of this trun's samples in the per-track mdat
			// The offset is relative to the start of the moof
			trun.data_offset = Some((new_moof_size as i32) + 8 + current_offset_in_track_mdat);

			// Calculate how many bytes this trun consumes
			for entry in &trun.entries {
				let size = entry
					.size
					.unwrap_or(traf.tfhd.default_sample_size.unwrap_or(default_sample_size));
				current_offset_in_track_mdat += size as i32;
			}
		}

		// Build the final per-track moof
		let per_track_moof = Moof {
			mfhd: mfhd.clone(),
			traf: vec![new_traf],
		};
		let mut moof_bytes = BytesMut::new();
		per_track_moof.encode(&mut moof_bytes)?;

		// Build the per-track mdat: 4 bytes size + 4 bytes 'mdat' + data
		let mdat_size = (8 + track_mdat_data.len()) as u32;
		let mut mdat_bytes = BytesMut::new();
		mdat_bytes.extend_from_slice(&mdat_size.to_be_bytes());
		mdat_bytes.extend_from_slice(b"mdat");
		mdat_bytes.extend_from_slice(&track_mdat_data);

		Ok((moof_bytes.freeze(), mdat_bytes.freeze()))
	}
}

impl Drop for Fmp4 {
	fn drop(&mut self) {
		let mut catalog = self.broadcast.catalog.lock();

		for track in self.tracks.values() {
			tracing::debug!(name = ?track.info.name, "ending track");

			// We're too lazy to keep track of if this track is for audio or video, so we just remove both.
			catalog.remove_video(&track.info.name);
			catalog.remove_audio(&track.info.name);
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use mp4_atom::{Mfhd, Traf, Tfhd, Tfdt, Trun, TrunEntry};

	/// Test that build_per_track_segment creates a moof with only one traf
	/// and an mdat containing only that track's samples.
	#[test]
	fn test_per_track_moof_mdat_extraction() {
		// Create a mock mfhd (movie fragment header)
		let mfhd = Mfhd {
			sequence_number: 1,
		};

		// Create sample data that would be in the global mdat
		// Track 1 samples: [0x11, 0x12, 0x13] (3 bytes) + [0x14, 0x15] (2 bytes) = 5 bytes
		// Track 2 samples: [0x21, 0x22, 0x23, 0x24] (4 bytes) = 4 bytes
		let track1_sample1 = vec![0x11, 0x12, 0x13];
		let track1_sample2 = vec![0x14, 0x15];
		let track2_sample1 = vec![0x21, 0x22, 0x23, 0x24];

		// Simulate mdat containing both tracks' data
		let mut global_mdat_data = BytesMut::new();
		global_mdat_data.extend_from_slice(&track1_sample1);
		global_mdat_data.extend_from_slice(&track1_sample2);
		global_mdat_data.extend_from_slice(&track2_sample1);
		let global_mdat = global_mdat_data.freeze();

		// Create traf for track 1
		// The data_offset points to after the moof (we'll use a mock moof_size)
		let mock_moof_size = 100; // Assume moof is 100 bytes
		let mdat_header_size = 8; // mdat box header

		let traf1 = Traf {
			tfhd: Tfhd {
				track_id: 1,
				base_data_offset: None,
				sample_description_index: Some(1),
				default_sample_duration: Some(1024),
				default_sample_size: None,
				default_sample_flags: None,
			},
			tfdt: Some(Tfdt {
				base_media_decode_time: 0,
			}),
			trun: vec![Trun {
				// data_offset is relative to moof start, points to mdat data
				data_offset: Some((mock_moof_size + mdat_header_size) as i32),
				entries: vec![
					TrunEntry {
						duration: Some(1024),
						size: Some(3), // 3 bytes
						flags: Some(0x02000000), // keyframe
						cts: Some(0),
					},
					TrunEntry {
						duration: Some(1024),
						size: Some(2), // 2 bytes
						flags: Some(0x01010000), // non-keyframe
						cts: Some(0),
					},
				],
			}],
			..Default::default()
		};

		// Create traf for track 2
		let traf2 = Traf {
			tfhd: Tfhd {
				track_id: 2,
				base_data_offset: None,
				sample_description_index: Some(1),
				default_sample_duration: Some(1024),
				default_sample_size: None,
				default_sample_flags: None,
			},
			tfdt: Some(Tfdt {
				base_media_decode_time: 0,
			}),
			trun: vec![Trun {
				// Track 2 data starts at offset 5 (after track 1's 5 bytes)
				data_offset: Some((mock_moof_size + mdat_header_size + 5) as i32),
				entries: vec![TrunEntry {
					duration: Some(1024),
					size: Some(4), // 4 bytes
					flags: Some(0x02000000), // keyframe
					cts: Some(0),
				}],
			}],
			..Default::default()
		};

		// Test extracting track 1
		let (moof_bytes, mdat_bytes) =
			build_per_track_segment_standalone(&mfhd, &traf1, &global_mdat, mock_moof_size);

		// Verify mdat contains only track 1 samples (5 bytes + 8 byte header)
		assert_eq!(mdat_bytes.len(), 8 + 5, "Track 1 mdat should be 13 bytes (8 header + 5 data)");

		// Verify mdat header
		let mdat_size = u32::from_be_bytes([mdat_bytes[0], mdat_bytes[1], mdat_bytes[2], mdat_bytes[3]]);
		assert_eq!(mdat_size, 13, "Track 1 mdat size field should be 13");
		assert_eq!(&mdat_bytes[4..8], b"mdat", "mdat fourcc should be 'mdat'");

		// Verify mdat data content
		assert_eq!(&mdat_bytes[8..11], &[0x11, 0x12, 0x13], "Track 1 sample 1 data");
		assert_eq!(&mdat_bytes[11..13], &[0x14, 0x15], "Track 1 sample 2 data");

		// Verify moof contains only one traf
		let decoded_moof = decode_moof(&moof_bytes);
		assert_eq!(decoded_moof.traf.len(), 1, "Per-track moof should have exactly 1 traf");
		assert_eq!(decoded_moof.traf[0].tfhd.track_id, 1, "Traf should be for track 1");

		// Test extracting track 2
		let (moof_bytes2, mdat_bytes2) =
			build_per_track_segment_standalone(&mfhd, &traf2, &global_mdat, mock_moof_size);

		// Verify mdat contains only track 2 samples (4 bytes + 8 byte header)
		assert_eq!(mdat_bytes2.len(), 8 + 4, "Track 2 mdat should be 12 bytes (8 header + 4 data)");

		// Verify mdat data content
		assert_eq!(&mdat_bytes2[8..12], &[0x21, 0x22, 0x23, 0x24], "Track 2 sample data");

		// Verify moof contains only one traf for track 2
		let decoded_moof2 = decode_moof(&moof_bytes2);
		assert_eq!(decoded_moof2.traf.len(), 1, "Per-track moof should have exactly 1 traf");
		assert_eq!(decoded_moof2.traf[0].tfhd.track_id, 2, "Traf should be for track 2");
	}

	/// Test that data_offset in the per-track moof points correctly into the per-track mdat
	#[test]
	fn test_per_track_data_offset() {
		let mfhd = Mfhd { sequence_number: 1 };

		// Single sample of 10 bytes
		let sample_data = vec![0xAA; 10];
		let global_mdat = Bytes::from(sample_data.clone());

		let mock_moof_size = 80;
		let mdat_header_size = 8;

		let traf = Traf {
			tfhd: Tfhd {
				track_id: 1,
				base_data_offset: None,
				sample_description_index: Some(1),
				default_sample_duration: Some(1024),
				default_sample_size: None,
				default_sample_flags: None,
			},
			tfdt: Some(Tfdt {
				base_media_decode_time: 0,
			}),
			trun: vec![Trun {
				data_offset: Some((mock_moof_size + mdat_header_size) as i32),
				entries: vec![TrunEntry {
					duration: Some(1024),
					size: Some(10),
					flags: Some(0x02000000),
					cts: Some(0),
				}],
			}],
			..Default::default()
		};

		let (moof_bytes, mdat_bytes) =
			build_per_track_segment_standalone(&mfhd, &traf, &global_mdat, mock_moof_size);

		// Decode the moof to check the data_offset
		let decoded_moof = decode_moof(&moof_bytes);
		let new_moof_size = moof_bytes.len();

		// The data_offset should point to right after the moof + mdat header
		let expected_data_offset = (new_moof_size + 8) as i32;
		assert_eq!(
			decoded_moof.traf[0].trun[0].data_offset,
			Some(expected_data_offset),
			"data_offset should point to start of sample data in mdat"
		);

		// Verify that reading at that offset gives us the sample data
		let actual_offset = expected_data_offset as usize - new_moof_size;
		assert_eq!(&mdat_bytes[actual_offset..actual_offset + 10], &sample_data[..], "Data at offset should match sample");
	}

	/// Standalone version of build_per_track_segment for testing without Fmp4 instance
	fn build_per_track_segment_standalone(
		mfhd: &Mfhd,
		traf: &Traf,
		global_mdat: &Bytes,
		moof_size: usize,
	) -> (Bytes, Bytes) {
		use mp4_atom::Encode;

		let default_sample_size = traf.tfhd.default_sample_size.unwrap_or(0);

		// Extract sample data for this track
		let mut track_mdat_data = BytesMut::new();
		let mut offset = traf.tfhd.base_data_offset.unwrap_or_default() as usize;

		for trun in &traf.trun {
			if let Some(data_offset) = trun.data_offset {
				let base_offset = traf.tfhd.base_data_offset.unwrap_or_default() as usize;
				let data_offset_usize = data_offset as usize;
				// Calculate offset into mdat
				offset = base_offset + data_offset_usize - moof_size - 8;
			}

			for entry in &trun.entries {
				let size = entry.size.unwrap_or(default_sample_size) as usize;
				if offset + size <= global_mdat.len() {
					track_mdat_data.extend_from_slice(&global_mdat[offset..offset + size]);
				}
				offset += size;
			}
		}

		// Clone and update traf
		let mut new_traf = traf.clone();

		// Calculate new moof size
		let temp_moof = Moof {
			mfhd: mfhd.clone(),
			traf: vec![new_traf.clone()],
		};
		let mut temp_moof_bytes = BytesMut::new();
		temp_moof.encode(&mut temp_moof_bytes).unwrap();
		let new_moof_size = temp_moof_bytes.len();

		// Update trun data_offsets
		let mut current_offset_in_track_mdat: i32 = 0;
		for trun in &mut new_traf.trun {
			trun.data_offset = Some((new_moof_size as i32) + 8 + current_offset_in_track_mdat);
			for entry in &trun.entries {
				let size = entry.size.unwrap_or(default_sample_size);
				current_offset_in_track_mdat += size as i32;
			}
		}

		// Build final moof
		let per_track_moof = Moof {
			mfhd: mfhd.clone(),
			traf: vec![new_traf],
		};
		let mut moof_bytes = BytesMut::new();
		per_track_moof.encode(&mut moof_bytes).unwrap();

		// Build mdat
		let mdat_size = (8 + track_mdat_data.len()) as u32;
		let mut mdat_bytes = BytesMut::new();
		mdat_bytes.extend_from_slice(&mdat_size.to_be_bytes());
		mdat_bytes.extend_from_slice(b"mdat");
		mdat_bytes.extend_from_slice(&track_mdat_data);

		(moof_bytes.freeze(), mdat_bytes.freeze())
	}

	/// Helper to decode a Moof from bytes
	fn decode_moof(bytes: &Bytes) -> Moof {
		use mp4_atom::Decode;
		let mut cursor = std::io::Cursor::new(bytes);
		Moof::decode(&mut cursor).expect("Failed to decode moof")
	}
}
