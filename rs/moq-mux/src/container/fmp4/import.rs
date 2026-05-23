use crate::container::Timestamp;
use anyhow::Context;
use bytes::{Buf, Bytes, BytesMut};
use hang::catalog::{AAC, AudioCodec, AudioConfig, Container, H264, H265, VP9, VideoCodec, VideoConfig};
use mp4_atom::{Any, Atom, DecodeMaybe, Encode, Mdat, Moof, Moov, Trak};
use std::collections::HashMap;
use tokio::io::{AsyncRead, AsyncReadExt};

/// Converts fMP4/CMAF files into MoQ broadcast streams using CMAF passthrough.
///
/// This struct processes fragmented MP4 (fMP4) files and transports complete
/// moof+mdat fragments directly as MoQ frames, preserving the CMAF container format.
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
pub struct Import {
	/// The broadcast being produced
	broadcast: moq_net::BroadcastProducer,

	/// The catalog being produced
	catalog: crate::catalog::hang::Producer,

	// A lookup to tracks in the broadcast
	tracks: HashMap<u32, Fmp4Track>,

	// The moov atom at the start of the file.
	moov: Option<Moov>,

	// The latest moof header
	moof: Option<Moof>,
	moof_size: usize,
}

#[derive(PartialEq, Debug)]
enum TrackKind {
	Video,
	Audio,
}

struct Fmp4Track {
	kind: TrackKind,

	track: moq_net::TrackProducer,
	group: Option<moq_net::GroupProducer>,

	// The minimum buffer required for the track.
	jitter: Option<Timestamp>,

	// The last timestamp seen for this track.
	last_timestamp: Option<Timestamp>,

	// The minimum duration between frames for this track.
	min_duration: Option<Timestamp>,
}

impl Import {
	/// Create a new CMAF importer that will write to the given broadcast.
	///
	/// The broadcast will be populated with tracks as they're discovered in the fMP4 file.
	pub fn new(broadcast: moq_net::BroadcastProducer, catalog: crate::catalog::hang::Producer) -> Self {
		Self {
			catalog,
			tracks: HashMap::default(),
			moov: None,
			moof: None,
			moof_size: 0,
			broadcast,
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
		// Clone the catalog to avoid the borrow checker.
		let mut catalog = self.catalog.clone();
		let mut catalog = catalog.lock();

		for trak in &moov.trak {
			let track_id = trak.tkhd.track_id;
			let handler = &trak.mdia.hdlr.handler;
			let suffix = ".m4s";

			let track = self.broadcast.unique_track(suffix)?;

			let kind = match handler.as_ref() {
				b"vide" => {
					let config = self.init_video(trak, &moov)?;
					catalog.video.renditions.insert(track.name.clone(), config);
					TrackKind::Video
				}
				b"soun" => {
					let config = self.init_audio(trak, &moov)?;
					catalog.audio.renditions.insert(track.name.clone(), config);
					TrackKind::Audio
				}
				b"sbtl" => anyhow::bail!("subtitle tracks are not supported"),
				handler => anyhow::bail!("unknown track type: {:?}", handler),
			};

			self.tracks.insert(
				track_id,
				Fmp4Track {
					kind,
					track,
					group: None,
					jitter: None,
					last_timestamp: None,
					min_duration: None,
				},
			);
		}

		drop(catalog);

		self.moov = Some(moov);

		Ok(())
	}

	fn container(&self, trak: &Trak, moov: &Moov) -> anyhow::Result<Container> {
		// Build a single-track init segment (ftyp+moov) for this track.
		{
			let ftyp = mp4_atom::Ftyp {
				major_brand: b"isom".into(),
				minor_version: 0x200,
				compatible_brands: vec![b"isom".into(), b"iso6".into(), b"mp41".into()],
			};

			// Build a moov with just this single track and matching mvex/trex.
			let track_id = trak.tkhd.track_id;
			let trex = moov
				.mvex
				.as_ref()
				.and_then(|mvex| mvex.trex.iter().find(|trex| trex.track_id == track_id))
				.cloned()
				.unwrap_or(mp4_atom::Trex {
					track_id,
					default_sample_description_index: 1,
					..Default::default()
				});

			let single_moov = Moov {
				mvhd: moov.mvhd.clone(),
				trak: vec![trak.clone()],
				mvex: Some(mp4_atom::Mvex {
					mehd: None,
					trex: vec![trex],
				}),
				meta: None,
				udta: None,
			};

			let mut buf = Vec::new();
			ftyp.encode(&mut buf)?;
			single_moov.encode(&mut buf)?;

			Ok(Container::Cmaf {
				init: buf.into(),
				timescale: Some(trak.mdia.mdhd.timescale),
				track_id: Some(trak.tkhd.track_id),
			})
		}
	}

	fn init_video(&mut self, trak: &Trak, moov: &Moov) -> anyhow::Result<VideoConfig> {
		let container = self.container(trak, moov)?;
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
					jitter: None,
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
				jitter: None,
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
					jitter: None,
				}
			}
			mp4_atom::Codec::Av01(av01) => VideoConfig {
				codec: crate::codec::av1::av1_from_av1c(&av01.av1c).into(),
				description: Default::default(),
				coded_width: Some(av01.visual.width as _),
				coded_height: Some(av01.visual.height as _),
				display_ratio_width: None,
				display_ratio_height: None,
				optimize_for_latency: None,
				bitrate: None,
				framerate: None,
				container,
				jitter: None,
			},
			mp4_atom::Codec::Unknown(unknown) => anyhow::bail!("unknown codec: {:?}", unknown),
			unsupported => anyhow::bail!("unsupported codec: {:?}", unsupported),
		};

		Ok(config)
	}

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
			jitter: None,
		})
	}

	fn init_audio(&mut self, trak: &Trak, moov: &Moov) -> anyhow::Result<AudioConfig> {
		let container = self.container(trak, moov)?;
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
				let profile = desc.dec_specific.profile;
				let sample_rate = mp4a.audio.sample_rate.integer() as u32;
				let channel_count = mp4a.audio.channel_count as u32;

				// Build the AudioSpecificConfig (ISO 14496-3 §1.6.2.1)
				// This is what GStreamer/WebCodecs need as codec_data.
				let description = crate::codec::aac::Config {
					profile,
					sample_rate,
					channel_count,
				}
				.encode();

				AudioConfig {
					codec: AAC { profile }.into(),
					sample_rate,
					channel_count,
					bitrate: Some(bitrate.into()),
					description: Some(description),
					container,
					jitter: None,
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
					jitter: None,
				}
			}
			mp4_atom::Codec::Unknown(unknown) => anyhow::bail!("unknown codec: {:?}", unknown),
			unsupported => anyhow::bail!("unsupported codec: {:?}", unsupported),
		};

		Ok(config)
	}

	// Extract all frames out of an mdat atom using CMAF passthrough.
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
			let mut track_data_start: Option<usize> = None;

			if traf.trun.is_empty() {
				anyhow::bail!("missing trun box");
			}

			// Keep track of the minimum and maximum timestamp for this track to compute the jitter.
			let mut min_timestamp = None;
			let mut max_timestamp = None;
			let mut contains_keyframe = false;

			for trun in &traf.trun {
				let tfhd = &traf.tfhd;

				if let Some(data_offset) = trun.data_offset {
					let base_offset = tfhd.base_data_offset.unwrap_or_default() as usize;
					let data_offset: usize = data_offset.try_into().context("invalid data offset")?;

					let relative_offset = data_offset
						.checked_sub(moof_size)
						.and_then(|v| v.checked_sub(header_size))
						.context("invalid data offset: underflow")?;

					offset = base_offset
						.checked_add(relative_offset)
						.context("invalid data offset: overflow")?;
				}

				// Capture the actual start offset for this traf before consuming samples
				if track_data_start.is_none() {
					track_data_start = Some(offset);
				}

				for entry in &trun.entries {
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
					let timestamp = crate::container::Timestamp::from_scale(pts, timescale)?;

					if offset + size > mdat.data.len() {
						anyhow::bail!("invalid data offset");
					}

					let keyframe = match track.kind {
						TrackKind::Video => {
							let keyframe = (flags >> 24) & 0x3 == 0x2;
							let non_sync = (flags >> 16) & 0x1 == 0x1;
							keyframe && !non_sync
						}
						TrackKind::Audio => true,
					};

					contains_keyframe |= keyframe;

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

			// Build a per-track moof containing only this traf, and a per-track mdat
			// with only the samples belonging to this track.
			let single_traf_moof = Moof {
				mfhd: moof.mfhd.clone(),
				traf: vec![traf.clone()],
			};

			// Compute the data range within the original mdat for this traf's samples.
			let track_data_start = track_data_start.unwrap_or(0);
			let track_data_end = offset; // offset was advanced past all samples above

			// The per-track sample range must be in bounds of the original mdat.
			// If not, the parsed sample sizes/offsets disagree with the actual data
			// and we cannot safely emit a passthrough fragment with rewritten offsets.
			anyhow::ensure!(
				track_data_start <= track_data_end && track_data_end <= mdat.data.len(),
				"track sample range {}..{} is out of bounds of mdat (len {})",
				track_data_start,
				track_data_end,
				mdat.data.len()
			);
			let track_mdat_data = &mdat.data[track_data_start..track_data_end];

			let mut adjusted_moof = single_traf_moof;

			// Apply structural (flag-presence) changes BEFORE measuring the encoded
			// size, otherwise the rewritten data_offset values are computed from a
			// stale size and the resulting fragment misaddresses sample data.
			// In particular: clearing tfhd.base_data_offset removes 8 bytes per traf,
			// and ensuring trun.data_offset is Some(...) reserves 4 bytes per trun.
			for traf_mut in &mut adjusted_moof.traf {
				traf_mut.tfhd.base_data_offset = None;
				for trun_mut in &mut traf_mut.trun {
					// Reserve the data_offset field; the real value is filled in below.
					trun_mut.data_offset = Some(0);
				}
			}

			let mut moof_buf = Vec::new();
			adjusted_moof.encode(&mut moof_buf)?;
			let new_moof_size = moof_buf.len();

			// Re-encode moof with corrected per-trun data_offset for the per-track fragment.
			// Each trun's data_offset points to the start of that run's data within the new mdat.
			let mdat_header_size_new = 8u64; // 4 bytes size + 4 bytes 'mdat'
			let mut cumulative_offset = 0u64;
			for traf_mut in &mut adjusted_moof.traf {
				for trun_mut in &mut traf_mut.trun {
					trun_mut.data_offset =
						Some((new_moof_size as u64 + mdat_header_size_new + cumulative_offset) as i32);

					// Advance past this trun's sample data
					let trun_data_size: u64 = trun_mut
						.entries
						.iter()
						.map(|e| {
							e.size
								.unwrap_or(traf_mut.tfhd.default_sample_size.unwrap_or(default_sample_size)) as u64
						})
						.sum();
					cumulative_offset += trun_data_size;
				}
			}

			moof_buf.clear();
			adjusted_moof.encode(&mut moof_buf)?;

			let per_track_mdat = Mdat {
				data: track_mdat_data.to_vec(),
			};
			per_track_mdat.encode(&mut moof_buf)?;

			let fragment_bytes = Bytes::from(moof_buf);

			// Write the per-track fragment as a single MoQ frame (passthrough).
			let mut g = if contains_keyframe {
				if let Some(mut prev) = track.group.take() {
					prev.finish()?;
				}
				track.track.append_group()?
			} else {
				track.group.take().context("no keyframe at start")?
			};

			g.write_frame(fragment_bytes)?;

			track.group = Some(g);

			if let (Some(min), Some(max), Some(min_duration)) = (min_timestamp, max_timestamp, track.min_duration) {
				let jitter = max - min + min_duration;

				if jitter < track.jitter.unwrap_or(Timestamp::MAX) {
					track.jitter = Some(jitter);

					let mut catalog = self.catalog.lock();

					match track.kind {
						TrackKind::Video => {
							let config = catalog
								.video
								.renditions
								.get_mut(&track.track.name)
								.context("missing video config")?;
							config.jitter = Some(jitter.convert()?);
						}
						TrackKind::Audio => {
							let config = catalog
								.audio
								.renditions
								.get_mut(&track.track.name)
								.context("missing audio config")?;
							config.jitter = Some(jitter.convert()?);
						}
					}
				}
			}
		}

		Ok(())
	}
}

impl Import {
	/// Finish all tracks, flushing current groups.
	pub fn finish(&mut self) -> anyhow::Result<()> {
		for track in self.tracks.values_mut() {
			if let Some(mut g) = track.group.take() {
				g.finish()?;
			}
			track.track.finish()?;
		}
		Ok(())
	}
}

impl Drop for Import {
	fn drop(&mut self) {
		let mut catalog = self.catalog.lock();

		for track in self.tracks.values() {
			match track.kind {
				TrackKind::Video => {
					catalog.video.renditions.remove(&track.track.name);
				}
				TrackKind::Audio => {
					catalog.audio.renditions.remove(&track.track.name);
				}
			}
		}
	}
}
