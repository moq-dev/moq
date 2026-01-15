use crate::catalog::{
	AudioCodec, AudioConfig, CatalogProducer, Container, VideoCodec, VideoConfig, AAC, AV1, H264, H265, VP9,
};
use crate::{self as hang, Timestamp};
use anyhow::Context;
use bytes::{Buf, Bytes, BytesMut};
use moq_lite as moq;
use mp4_atom::{Any, Atom, DecodeMaybe, Mdat, Moof, Moov, Trak};
use std::collections::HashMap;

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
	// This `hang` variant includes a catalog.
	broadcast: hang::BroadcastProducer,

	// A clone of the broadcast's catalog for mutable access.
	// This is the same underlying catalog (via Arc), just a separate binding.
	catalog: CatalogProducer,

	// A lookup to tracks in the broadcast
	tracks: HashMap<u32, hang::TrackProducer>,

	// The timestamp of the last keyframe for each track
	last_keyframe: HashMap<u32, hang::Timestamp>,

	// Track if we've sent the first frame for each track (needed for passthrough mode)
	first_frame_sent: HashMap<u32, bool>,

	// The moov atom at the start of the file.
	moov: Option<Moov>,

	// The latest moof header
	moof: Option<Moof>,
	moof_size: usize,

	/// When true, transport CMAF fragments directly (passthrough mode)
	/// When false, decompose fragments into individual samples (current behavior)
	passthrough_mode: bool,

	/// When passthrough_mode is enabled, store raw bytes of moof
	moof_bytes: Option<Bytes>,

	/// When passthrough_mode is enabled, store raw bytes of ftyp (file type box)
	ftyp_bytes: Option<Bytes>,

	/// When passthrough_mode is enabled, store raw bytes of moov (init segment)
	moov_bytes: Option<Bytes>,
}

impl Fmp4 {
	/// Create a new CMAF importer that will write to the given broadcast.
	///
	/// The broadcast will be populated with tracks as they're discovered in the
	/// fMP4 file. The catalog from the `hang::BroadcastProducer` is used automatically.
	pub fn new(broadcast: hang::BroadcastProducer) -> Self {
		let catalog = broadcast.catalog.clone();
		Self {
			broadcast,
			catalog,
			tracks: HashMap::default(),
			last_keyframe: HashMap::default(),
			first_frame_sent: HashMap::default(),
			moov: None,
			moof: None,
			moof_size: 0,
			passthrough_mode: false,
			moof_bytes: None,
			ftyp_bytes: None,
			moov_bytes: None,
		}
	}

	/// Set passthrough mode for CMAF fragment transport.
	///
	/// When enabled, complete fMP4 fragments (moof+mdat) are transported directly
	/// instead of being decomposed into individual samples.
	pub fn set_passthrough_mode(&mut self, enabled: bool) {
		self.passthrough_mode = enabled;
	}

	pub fn decode<T: Buf + AsRef<[u8]>>(&mut self, buf: &mut T) -> anyhow::Result<()> {
		// If passthrough mode, we need to extract raw bytes before parsing.
		let available_bytes = if self.passthrough_mode && buf.has_remaining() {
			let chunk = buf.chunk();
			Some(Bytes::copy_from_slice(chunk))
		} else {
			None
		};

		let mut cursor = std::io::Cursor::new(buf);
		let mut position = 0;
		let mut bytes_offset = 0;

		while let Some(atom) = mp4_atom::Any::decode_maybe(&mut cursor)? {
			// Process the parsed atom.
			let size = cursor.position() as usize - position;
			position = cursor.position() as usize;

			match atom {
				Any::Ftyp(_) | Any::Styp(_) => {
					// If passthrough mode, capture raw bytes of ftyp (file type box)
					if self.passthrough_mode {
						if let Some(ref bytes) = available_bytes {
							if bytes_offset + size <= bytes.len() {
								self.ftyp_bytes = Some(bytes.slice(bytes_offset..bytes_offset + size));
								tracing::debug!(ftyp_size = size, bytes_offset, "captured ftyp bytes for init segment");
							} else {
								tracing::warn!(
									bytes_offset,
									size,
									available_len = bytes.len(),
									"ftyp bytes out of range"
								);
							}
						} else {
							tracing::warn!("passthrough mode but available_bytes is None when processing ftyp");
						}
					}
					// Skip ftyp/styp atoms in normal processing
				}
				Any::Moov(moov) => {
					// If passthrough mode, capture raw bytes of moov (init segment)
					if self.passthrough_mode {
						if let Some(ref bytes) = available_bytes {
							if bytes_offset + size <= bytes.len() {
								self.moov_bytes = Some(bytes.slice(bytes_offset..bytes_offset + size));
								tracing::debug!(moov_size = size, bytes_offset, "captured moov bytes for init segment");
							} else {
								tracing::warn!(
									bytes_offset,
									size,
									available_len = bytes.len(),
									"moov bytes out of range"
								);
							}
						} else {
							tracing::warn!("passthrough mode but available_bytes is None when processing moov");
						}
					}
					// Create the broadcast.
					self.init(moov)?;
				}
				Any::Moof(moof) => {
					if self.moof.is_some() {
						// Two moof boxes in a row.
						anyhow::bail!("duplicate moof box");
					}

					self.moof = Some(moof);
					self.moof_size = size;

					// If passthrough mode, extract and store raw bytes of moof
					if let Some(ref bytes) = available_bytes {
						if bytes_offset + size <= bytes.len() {
							self.moof_bytes = Some(bytes.slice(bytes_offset..bytes_offset + size));
						}
					}
				}
				Any::Mdat(mdat) => {
					if self.passthrough_mode {
						// Transport complete fragment
						let moof = self.moof.take().context("missing moof box")?;
						let moof_bytes = self.moof_bytes.take().context("missing moof bytes")?;

						// Extract mdat bytes
						let mdat_bytes = if let Some(ref bytes) = available_bytes {
							if bytes_offset + size <= bytes.len() {
								bytes.slice(bytes_offset..bytes_offset + size)
							} else {
								anyhow::bail!("invalid buffer position for mdat");
							}
						} else {
							anyhow::bail!("missing available bytes in passthrough mode");
						};

						// Combine moof + mdat into complete fragment
						let mut fragment_bytes = BytesMut::with_capacity(moof_bytes.len() + mdat_bytes.len());
						fragment_bytes.extend_from_slice(&moof_bytes);
						fragment_bytes.extend_from_slice(&mdat_bytes);
						let fragment = fragment_bytes.freeze();

						tracing::info!(
							moof_size = moof_bytes.len(),
							mdat_size = mdat_bytes.len(),
							total_fragment_size = fragment.len(),
							"processing CMAF fragment (moof+mdat)"
						);
						self.transport_fragment(fragment, moof)?;
						tracing::info!("finished processing CMAF fragment, ready for next fragment");
					} else {
						// Extract the samples from the mdat atom (existing behavior)
						let header_size = size - mdat.data.len();
						self.extract(mdat, header_size)?;
					}
				}
				_ => {
					// Skip unknown atoms (e.g., sidx, which is optional and used for segment indexing)
					// These are safe to ignore and don't affect playback
					tracing::debug!(?atom, "skipping optional atom")
				}
			}

			bytes_offset += size;
		}

		// Advance the buffer by the amount of data that was processed.
		cursor.into_inner().advance(position);

		Ok(())
	}

	pub fn is_initialized(&self) -> bool {
		self.moov.is_some()
	}

	fn init(&mut self, moov: Moov) -> anyhow::Result<()> {
		let passthrough_mode = self.passthrough_mode;
		tracing::info!(passthrough_mode, "initializing fMP4 with passthrough mode");
		let mut catalog = self.catalog.lock();

		// Track which specific tracks were created in this init call
		let mut created_video_tracks = Vec::new();
		let mut created_audio_tracks = Vec::new();

		for trak in &moov.trak {
			let track_id = trak.tkhd.track_id;
			let handler = &trak.mdia.hdlr.handler;

			let track = match handler.as_ref() {
				b"vide" => {
					let config = Self::init_video_static(trak, passthrough_mode)?;
					tracing::info!(container = ?config.container, "created video config with container");

					let track = moq::Track {
						name: self.broadcast.track_name("video"),
						priority: 1,
					};

					tracing::debug!(name = ?track.name, ?config, "starting track");

					let video = catalog.insert_video(track.name.clone(), config.clone());
					video.priority = 1;

					// Record this track name
					created_video_tracks.push(track.name.clone());

					let track = track.produce();
					self.broadcast.insert_track(track.consumer);
					hang::TrackProducer::new(track.producer, config.container)
				}
				b"soun" => {
					let config = Self::init_audio_static(trak, passthrough_mode)?;
					tracing::info!(container = ?config.container, "created audio config with container");

					let track = moq::Track {
						name: self.broadcast.track_name("audio"),
						priority: 2,
					};

					tracing::debug!(name = ?track.name, ?config, "starting track");

					let audio = catalog.insert_audio(track.name.clone(), config.clone());
					audio.priority = 2;

					// Record this track name
					created_audio_tracks.push(track.name.clone());

					let track = track.produce();
					self.broadcast.insert_track(track.consumer);
					hang::TrackProducer::new(track.producer, config.container)
				}
				b"sbtl" => anyhow::bail!("subtitle tracks are not supported"),
				handler => anyhow::bail!("unknown track type: {:?}", handler),
			};

			self.tracks.insert(track_id, track);
		}

		// Verify that the moov atom contains all expected tracks BEFORE moving it
		let moov_track_count = moov.trak.len();
		let has_video = moov.trak.iter().any(|t| t.mdia.hdlr.handler.as_ref() == b"vide");
		let has_audio = moov.trak.iter().any(|t| t.mdia.hdlr.handler.as_ref() == b"soun");

		self.moov = Some(moov);

		// In passthrough mode, store the init segment (ftyp+moov) in the catalog
		// instead of sending it over the data tracks. This allows clients to
		// reconstruct init segments from the catalog.
		//
		// Note: Init segments are embedded in the catalog.
		// A future optimization could build init segments from the description field
		// (e.g., avcC box for H.264) along with other catalog metadata, but for now
		// we store the complete init segment for simplicity and correctness.
		if passthrough_mode {
			if let Some(moov_bytes) = self.moov_bytes.as_ref() {
				// Build init segment: ftyp (if available) + moov
				let mut init_segment = BytesMut::new();
				if let Some(ref ftyp_bytes) = self.ftyp_bytes {
					init_segment.extend_from_slice(ftyp_bytes);
					tracing::debug!(ftyp_size = ftyp_bytes.len(), "including ftyp in init segment");
				}
				init_segment.extend_from_slice(moov_bytes);
				let init_segment_bytes = init_segment.freeze();

				// Verify that the moov atom contains all expected tracks
				let expected_video_tracks = catalog.video.as_ref().map(|v| v.renditions.len()).unwrap_or(0);
				let expected_audio_tracks = catalog.audio.as_ref().map(|a| a.renditions.len()).unwrap_or(0);

				tracing::info!(
					tracks_in_moov = moov_track_count,
					expected_video = expected_video_tracks,
					expected_audio = expected_audio_tracks,
					tracks_processed = self.tracks.len(),
					init_segment_size = init_segment_bytes.len(),
					ftyp_included = self.ftyp_bytes.is_some(),
					has_video = has_video,
					has_audio = has_audio,
					"storing init segment in catalog"
				);

				// Verify moov atom signature
				let moov_offset = self.ftyp_bytes.as_ref().map(|f| f.len()).unwrap_or(0);
				if moov_offset + 8 <= init_segment_bytes.len() {
					let atom_type = String::from_utf8_lossy(&init_segment_bytes[moov_offset + 4..moov_offset + 8]);
					tracing::info!(atom_type = %atom_type, "verifying moov atom signature in init segment");
				}

				// Warn if moov doesn't contain expected tracks.
				// For HLS, inits are per-track (video-only or audio-only), so skip cross-track warnings.
				let video_only = has_video && !has_audio;
				let audio_only = has_audio && !has_video;
				if expected_video_tracks > 0 && !has_video && !audio_only {
					tracing::error!(
						"moov atom does not contain video track but video configs exist! This will cause client-side errors."
					);
				}
				if expected_audio_tracks > 0 && !has_audio && !video_only {
					tracing::error!(
						"moov atom does not contain audio track but audio configs exist! This will cause client-side errors."
					);
				}

				// Store init segment in catalog for the relevant track type
				// For HLS, each track has its own init segment (video init segment only has video,
				// audio init segment only has audio). For direct fMP4 files, the init segment
				// contains all tracks. We store track-specific init segments only in the tracks
				// created in this init call, not all renditions of that type.

				if has_video {
					if let Some(video) = catalog.video.as_mut() {
						for track_name in &created_video_tracks {
							if let Some(config) = video.renditions.get_mut(track_name) {
								config.init_segment = Some(init_segment_bytes.clone());
								tracing::debug!(
									video_track = %track_name,
									init_segment_size = init_segment_bytes.len(),
									has_audio_track = has_audio,
									"stored init segment in video config"
								);
							}
						}
					}
				}

				if has_audio {
					if let Some(audio) = catalog.audio.as_mut() {
						for track_name in &created_audio_tracks {
							if let Some(config) = audio.renditions.get_mut(track_name) {
								config.init_segment = Some(init_segment_bytes.clone());
								tracing::debug!(
									audio_track = %track_name,
									init_segment_size = init_segment_bytes.len(),
									has_video_track = has_video,
									"stored init segment in audio config"
								);
							}
						}
					}
				}

				// Init has been stored; clear cached moov/ftyp to avoid repeated warnings later.
				self.moov_bytes = None;
				self.ftyp_bytes = None;
			} else {
				tracing::warn!(
					"passthrough mode enabled but moov_bytes is None - init segment will not be stored in catalog"
				);
			}
		}

		Ok(())
	}

	fn init_video_static(trak: &Trak, passthrough_mode: bool) -> anyhow::Result<VideoConfig> {
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
					container: if passthrough_mode {
						Container::Cmaf
					} else {
						Container::Native
					},
					init_segment: None,
				}
			}
			mp4_atom::Codec::Hev1(hev1) => Self::init_h265_static(true, &hev1.hvcc, &hev1.visual, passthrough_mode)?,
			mp4_atom::Codec::Hvc1(hvc1) => Self::init_h265_static(false, &hvc1.hvcc, &hvc1.visual, passthrough_mode)?,
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
				container: if passthrough_mode {
					Container::Cmaf
				} else {
					Container::Native
				},
				init_segment: None,
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
					container: if passthrough_mode {
						Container::Cmaf
					} else {
						Container::Native
					},
					init_segment: None,
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
					container: if passthrough_mode {
						Container::Cmaf
					} else {
						Container::Native
					},
					init_segment: None,
				}
			}
			mp4_atom::Codec::Unknown(unknown) => anyhow::bail!("unknown codec: {:?}", unknown),
			unsupported => anyhow::bail!("unsupported codec: {:?}", unsupported),
		};

		Ok(config)
	}

	// There's two almost identical hvcc atoms in the wild.
	fn init_h265_static(
		in_band: bool,
		hvcc: &mp4_atom::Hvcc,
		visual: &mp4_atom::Visual,
		passthrough_mode: bool,
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
			init_segment: None,
			framerate: None,
			display_ratio_width: None,
			display_ratio_height: None,
			optimize_for_latency: None,
			container: if passthrough_mode {
				Container::Cmaf
			} else {
				Container::Native
			},
		})
	}

	fn init_audio_static(trak: &Trak, passthrough_mode: bool) -> anyhow::Result<AudioConfig> {
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
					container: if passthrough_mode {
						Container::Cmaf
					} else {
						Container::Native
					},
					init_segment: None,
				}
			}
			mp4_atom::Codec::Opus(opus) => {
				AudioConfig {
					codec: AudioCodec::Opus,
					sample_rate: opus.audio.sample_rate.integer() as _,
					channel_count: opus.audio.channel_count as _,
					bitrate: None,
					description: None, // TODO?
					container: if passthrough_mode {
						Container::Cmaf
					} else {
						Container::Native
					},
					init_segment: None,
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

	// Transport a complete CMAF fragment (moof+mdat) directly without decomposing.
	fn transport_fragment(&mut self, fragment: Bytes, moof: Moof) -> anyhow::Result<()> {
		// Verify that init segment was sent before fragments
		if self.moov_bytes.is_some() {
			tracing::warn!("transporting fragment but moov_bytes is still set - init segment may not have been sent");
		}

		// Verify fragment starts with moof atom
		if fragment.len() >= 8 {
			let atom_type = String::from_utf8_lossy(&fragment[4..8]);
			tracing::info!(atom_type = %atom_type, fragment_size = fragment.len(), passthrough_mode = self.passthrough_mode, "transporting fragment");
		}

		// Ensure moov is available (init segment must be processed first)
		let moov = self.moov.as_ref().ok_or_else(|| {
			anyhow::anyhow!("missing moov box - init segment must be processed before fragments. Make sure ensure_init_segment() is called first.")
		})?;

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

			let tfdt = traf.tfdt.as_ref().context("missing tfdt box")?;
			let dts = tfdt.base_media_decode_time;
			let timescale = trak.mdia.mdhd.timescale as u64;

			// Convert timestamp from track timescale to microseconds
			let micros = (dts as u128 * 1_000_000 / timescale as u128) as u64;
			let timestamp = hang::Timestamp::from_micros(micros)?;

			// Determine keyframe status (reuse logic from extract())
			let is_keyframe = if trak.mdia.hdlr.handler == b"vide".into() {
				// For video, check sample flags in trun entries
				let mut is_keyframe = false;
				if let Some(trun) = traf.trun.first() {
					if let Some(entry) = trun.entries.first() {
						let tfhd = &traf.tfhd;
						let flags = entry.flags.unwrap_or(tfhd.default_sample_flags.unwrap_or_default());
						// https://chromium.googlesource.com/chromium/src/media/+/master/formats/mp4/track_run_iterator.cc#177
						let keyframe_flag = (flags >> 24) & 0x3 == 0x2; // kSampleDependsOnNoOther
						let non_sync = (flags >> 16) & 0x1 == 0x1; // kSampleIsNonSyncSample
						is_keyframe = keyframe_flag && !non_sync;

						if is_keyframe {
							// Force an audio keyframe on video keyframes
							for audio in moov.trak.iter().filter(|t| t.mdia.hdlr.handler == b"soun".into()) {
								self.last_keyframe.remove(&audio.tkhd.track_id);
							}
						}
					}
				}
				is_keyframe
			} else {
				// For audio, force keyframe every 10 seconds or at video keyframes
				match self.last_keyframe.get(&track_id) {
					Some(prev) => timestamp - *prev > Timestamp::from_secs(10).unwrap(),
					None => true,
				}
			};

			if is_keyframe {
				self.last_keyframe.insert(track_id, timestamp);
			}

			// In passthrough mode, send fragments directly without init segments
			// Init segments are stored in the catalog and reconstructed on the client side
			if self.passthrough_mode {
				// The first frame must be a keyframe to create the initial group
				// After that, we can send fragments based on their actual keyframe status
				let is_first_frame = !self.first_frame_sent.get(&track_id).copied().unwrap_or(false);
				let should_be_keyframe = is_first_frame || is_keyframe;

				if is_first_frame {
					self.first_frame_sent.insert(track_id, true);
				}

				let frame = hang::Frame {
					timestamp,
					keyframe: should_be_keyframe,
					payload: fragment.clone().into(),
				};
				track.write(frame)?;
				if should_be_keyframe {
					tracing::info!(track_id, timestamp = ?timestamp, fragment_size = fragment.len(), is_first = is_first_frame, "sent fragment in passthrough mode (keyframe - creates group)");
				} else {
					tracing::debug!(track_id, timestamp = ?timestamp, fragment_size = fragment.len(), "sent non-keyframe fragment in passthrough mode");
				}
			} else {
				// For non-passthrough mode, just write the frame normally
				let frame = hang::Frame {
					timestamp,
					keyframe: is_keyframe,
					payload: fragment.clone().into(),
				};
				track.write(frame)?;
				tracing::info!(track_id, timestamp = ?timestamp, fragment_size = fragment.len(), is_keyframe = is_keyframe, "sent fragment (non-passthrough mode)");
			}
		}

		Ok(())
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
