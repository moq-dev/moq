use bytes::{Bytes, BytesMut};
use hang::catalog::{AAC, AudioCodec, AudioConfig, Container, H264, H265, VP9, VideoCodec, VideoConfig};
use moq_net::Timestamp;
use mp4_atom::{Any, Atom, DecodeMaybe, Encode, Mdat, Moof, Moov, Trak};
use std::collections::HashMap;

use super::Error;
use crate::Result;

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
pub struct Import<E: crate::catalog::hang::CatalogExt = ()> {
	/// The broadcast being produced
	broadcast: moq_net::BroadcastProducer,

	/// The catalog being produced
	catalog: crate::catalog::Producer<E>,

	// A lookup to tracks in the broadcast
	tracks: HashMap<u32, Fmp4Track>,

	// The moov atom at the start of the file.
	moov: Option<Moov>,

	// The latest moof header
	moof: Option<Moof>,
	moof_size: usize,

	// Bytes carried across calls: a partial atom at the tail of one `decode` waits
	// here for the rest to arrive on the next call.
	buffer: BytesMut,
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

	// Sequence to use for the next group, set by `Import::seek`.
	pending_sequence: Option<u64>,
}

impl<E: crate::catalog::hang::CatalogExt> Import<E> {
	/// Create a new CMAF importer that will write to the given broadcast.
	///
	/// The broadcast will be populated with tracks as they're discovered in the fMP4 file.
	pub fn new(broadcast: moq_net::BroadcastProducer, catalog: crate::catalog::Producer<E>) -> Self {
		Self {
			catalog,
			tracks: HashMap::default(),
			moov: None,
			moof: None,
			moof_size: 0,
			broadcast,
			buffer: BytesMut::new(),
		}
	}

	/// Decode a buffer of bytes.
	pub fn decode(&mut self, data: &[u8]) -> Result<()> {
		self.buffer.extend_from_slice(data);
		self.drain()
	}

	/// Parse every whole top-level atom buffered so far, leaving any trailing
	/// partial atom for the next call.
	fn drain(&mut self) -> Result<()> {
		// Parse complete atoms first, recording each one's byte range, then process
		// them. Collecting up front keeps `self.buffer` un-borrowed while the handlers
		// (`init`/`extract`) take `&mut self`.
		let mut parsed = Vec::new();
		let mut position = 0;
		loop {
			let mut cursor = std::io::Cursor::new(&self.buffer[position..]);
			let Some(atom) = mp4_atom::Any::decode_maybe(&mut cursor)? else {
				break;
			};
			let size = cursor.position() as usize;
			parsed.push((atom, position, size));
			position += size;
		}

		if position == 0 {
			return Ok(());
		}

		// Detach the fully-parsed prefix as a cheap ref-counted buffer so each mdat's
		// raw bytes can be sliced out without copying or borrowing `self`.
		let consumed = self.buffer.split_to(position).freeze();

		for (atom, start, size) in parsed {
			match atom {
				Any::Ftyp(_) | Any::Styp(_) => {}
				Any::Moov(moov) => {
					self.init(moov)?;
				}
				Any::Moof(moof) => {
					if self.moof.is_some() {
						return Err(Error::DuplicateMoof.into());
					}
					self.moof.replace(moof);
					self.moof_size = size;
				}
				Any::Mdat(mdat) => {
					let raw = consumed.slice(start..start + size);
					self.extract(mdat, &raw)?;
				}
				_ => {
					// Skip unknown atoms (e.g., sidx, which is optional and used for segment indexing)
					// These are safe to ignore and don't affect playback
				}
			}
		}

		Ok(())
	}

	fn init(&mut self, moov: Moov) -> Result<()> {
		// Clone the catalog to avoid the borrow checker.
		let mut catalog = self.catalog.clone();
		let mut catalog = catalog.lock();

		for trak in &moov.trak {
			let track_id = trak.tkhd.track_id;
			let handler = &trak.mdia.hdlr.handler;
			let suffix = ".m4s";

			// Declare the track at the fMP4's native timescale. Frame timestamps are
			// emitted at this same scale (see below), so they satisfy the track's
			// timescale invariant and ride the wire for the relay, redundant with the
			// timing already inside each CMAF fragment.
			let timescale = moq_net::Timescale::new(trak.mdia.mdhd.timescale as u64)?;
			let track = self.broadcast.create_track(
				self.broadcast.unique_name(suffix),
				moq_net::TrackInfo::default().with_timescale(timescale),
			)?;

			let kind = match handler.as_ref() {
				b"vide" => {
					let config = self.init_video(trak, &moov)?;
					catalog.video.renditions.insert(track.name().to_string(), config);
					TrackKind::Video
				}
				b"soun" => {
					let config = self.init_audio(trak, &moov)?;
					catalog.audio.renditions.insert(track.name().to_string(), config);
					TrackKind::Audio
				}
				b"sbtl" => return Err(Error::UnsupportedSubtitle.into()),
				handler => {
					let mut buf = [0u8; 4];
					buf[..handler.len().min(4)].copy_from_slice(&handler[..handler.len().min(4)]);
					return Err(Error::UnknownTrackHandler(buf).into());
				}
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
					pending_sequence: None,
				},
			);
		}

		drop(catalog);

		self.moov = Some(moov);

		Ok(())
	}

	fn container(&self, trak: &Trak, moov: &Moov) -> Result<Container> {
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
				ainf: None,
				meta: None,
				udta: None,
			};

			let mut buf = Vec::new();
			ftyp.encode(&mut buf)?;
			single_moov.encode(&mut buf)?;

			Ok(Container::Cmaf { init: buf.into() })
		}
	}

	fn init_video(&mut self, trak: &Trak, moov: &Moov) -> Result<VideoConfig> {
		let container = self.container(trak, moov)?;
		let stsd = &trak.mdia.minf.stbl.stsd;

		let codec = match stsd.codecs.len() {
			0 => return Err(Error::MissingCodec.into()),
			1 => &stsd.codecs[0],
			_ => return Err(Error::MultipleCodecs.into()),
		};

		let config = match codec {
			mp4_atom::Codec::Avc1(avc1) => {
				let avcc = &avc1.avcc;

				let mut description = BytesMut::new();
				avcc.encode_body(&mut description)?;

				let mut config = VideoConfig::new(H264 {
					profile: avcc.avc_profile_indication,
					constraints: avcc.profile_compatibility,
					level: avcc.avc_level_indication,
					inline: false,
				});
				config.coded_width = Some(avc1.visual.width as _);
				config.coded_height = Some(avc1.visual.height as _);
				config.description = Some(description.freeze());
				config.container = container;
				config
			}
			mp4_atom::Codec::Hev1(hev1) => self.init_h265(true, &hev1.hvcc, &hev1.visual, container)?,
			mp4_atom::Codec::Hvc1(hvc1) => self.init_h265(false, &hvc1.hvcc, &hvc1.visual, container)?,
			mp4_atom::Codec::Vp08(vp08) => {
				let mut config = VideoConfig::new(VideoCodec::VP8);
				config.coded_width = Some(vp08.visual.width as _);
				config.coded_height = Some(vp08.visual.height as _);
				config.container = container;
				config
			}
			mp4_atom::Codec::Vp09(vp09) => {
				// https://github.com/gpac/mp4box.js/blob/325741b592d910297bf609bc7c400fc76101077b/src/box-codecs.js#L238
				let vpcc = &vp09.vpcc;

				let mut config = VideoConfig::new(VP9 {
					profile: vpcc.profile,
					level: vpcc.level,
					bit_depth: vpcc.bit_depth,
					color_primaries: vpcc.color_primaries,
					chroma_subsampling: vpcc.chroma_subsampling,
					transfer_characteristics: vpcc.transfer_characteristics,
					matrix_coefficients: vpcc.matrix_coefficients,
					full_range: vpcc.video_full_range_flag,
				});
				config.coded_width = Some(vp09.visual.width as _);
				config.coded_height = Some(vp09.visual.height as _);
				config.container = container;
				config
			}
			mp4_atom::Codec::Av01(av01) => {
				let mut config = VideoConfig::new(crate::codec::av1::av1_from_av1c(&av01.av1c));
				config.coded_width = Some(av01.visual.width as _);
				config.coded_height = Some(av01.visual.height as _);
				config.container = container;
				config
			}
			mp4_atom::Codec::Unknown(unknown) => return Err(Error::UnknownCodec(*unknown).into()),
			unsupported => return Err(Error::UnsupportedCodec(Box::new(unsupported.clone())).into()),
		};

		Ok(config)
	}

	fn init_h265(
		&mut self,
		in_band: bool,
		hvcc: &mp4_atom::Hvcc,
		visual: &mp4_atom::Visual,
		container: Container,
	) -> Result<VideoConfig> {
		let mut description = BytesMut::new();
		hvcc.encode_body(&mut description)?;

		let mut config = VideoConfig::new(H265 {
			in_band,
			profile_space: hvcc.general_profile_space,
			profile_idc: hvcc.general_profile_idc,
			profile_compatibility_flags: hvcc.general_profile_compatibility_flags,
			tier_flag: hvcc.general_tier_flag,
			level_idc: hvcc.general_level_idc,
			constraint_flags: hvcc.general_constraint_indicator_flags,
		});
		config.description = Some(description.freeze());
		config.coded_width = Some(visual.width as _);
		config.coded_height = Some(visual.height as _);
		config.container = container;
		Ok(config)
	}

	fn init_audio(&mut self, trak: &Trak, moov: &Moov) -> Result<AudioConfig> {
		let container = self.container(trak, moov)?;
		let stsd = &trak.mdia.minf.stbl.stsd;

		let codec = match stsd.codecs.len() {
			0 => return Err(Error::MissingCodec.into()),
			1 => &stsd.codecs[0],
			_ => return Err(Error::MultipleCodecs.into()),
		};

		let config = match codec {
			mp4_atom::Codec::Mp4a(mp4a) => {
				let desc = &mp4a.esds.es_desc.dec_config;

				// TODO Also support mp4a.67
				if desc.object_type_indication != 0x40 {
					return Err(Error::UnsupportedMpeg2.into());
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

				let mut config = AudioConfig::new(AAC { profile }, sample_rate, channel_count);
				config.bitrate = Some(bitrate.into());
				config.description = Some(description);
				config.container = container;
				config
			}
			mp4_atom::Codec::Opus(opus) => {
				let mut config = AudioConfig::new(
					AudioCodec::Opus,
					opus.audio.sample_rate.integer() as _,
					opus.audio.channel_count as _,
				);
				config.container = container;
				config
			}
			mp4_atom::Codec::Unknown(unknown) => return Err(Error::UnknownCodec(*unknown).into()),
			unsupported => return Err(Error::UnsupportedCodec(Box::new(unsupported.clone())).into()),
		};

		Ok(config)
	}

	// Extract all frames out of an mdat atom using CMAF passthrough.
	fn extract(&mut self, mdat: Mdat, mdat_raw: &[u8]) -> Result<()> {
		let moov = self.moov.as_ref().ok_or(Error::NoMoov)?;
		let moof = self.moof.take().ok_or(Error::NoMoof)?;
		let moof_size = self.moof_size;
		let header_size = mdat_raw.len() - mdat.data.len();

		// Loop over all of the traf boxes in the moof.
		for traf in &moof.traf {
			let track_id = traf.tfhd.track_id;
			let track = self.tracks.get_mut(&track_id).ok_or(Error::UnknownTrack(track_id))?;

			// Find the track information in the moov
			let trak = moov
				.trak
				.iter()
				.find(|trak| trak.tkhd.track_id == track_id)
				.ok_or(Error::UnknownTrack(track_id))?;
			let trex = moov
				.mvex
				.as_ref()
				.and_then(|mvex| mvex.trex.iter().find(|trex| trex.track_id == track_id));

			// The moov contains some defaults
			let default_sample_duration = trex.map(|trex| trex.default_sample_duration).unwrap_or_default();
			let default_sample_size = trex.map(|trex| trex.default_sample_size).unwrap_or_default();
			let default_sample_flags = trex.map(|trex| trex.default_sample_flags).unwrap_or_default();

			let tfdt = traf.tfdt.as_ref().ok_or(Error::MissingTfdt)?;
			let mut dts = tfdt.base_media_decode_time;
			let timescale = moq_net::Timescale::new(trak.mdia.mdhd.timescale as u64)?;

			let mut offset = traf.tfhd.base_data_offset.unwrap_or_default() as usize;
			let mut track_data_start: Option<usize> = None;

			if traf.trun.is_empty() {
				return Err(Error::MissingTrun.into());
			}

			// Keep track of the minimum and maximum timestamp for this track to compute the jitter.
			let mut min_timestamp = None;
			let mut max_timestamp = None;
			let mut contains_keyframe = false;
			let total_samples: usize = traf.trun.iter().map(|t| t.entries.len()).sum();
			let mut sample_index = 0usize;

			for trun in &traf.trun {
				let tfhd = &traf.tfhd;

				if let Some(data_offset) = trun.data_offset {
					let base_offset = tfhd.base_data_offset.unwrap_or_default() as usize;
					let data_offset: usize = data_offset.try_into().map_err(|_| Error::InvalidDataOffset)?;

					let relative_offset = data_offset
						.checked_sub(moof_size)
						.and_then(|v| v.checked_sub(header_size))
						.ok_or(Error::InvalidDataOffset)?;

					offset = base_offset
						.checked_add(relative_offset)
						.ok_or(Error::InvalidDataOffset)?;
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
						.or(tfhd.default_sample_duration)
						.or(Some(default_sample_duration))
						.filter(|duration| *duration != 0);
					let size = entry
						.size
						.unwrap_or(tfhd.default_sample_size.unwrap_or(default_sample_size)) as usize;

					// A non-final sample with no resolvable duration leaves the rest of the
					// fragment's DTS ambiguous, so reject it rather than collapse timestamps.
					if duration.is_none() && sample_index + 1 != total_samples {
						return Err(Error::MissingSampleDuration.into());
					}

					let pts = (dts as i64 + entry.cts.unwrap_or_default() as i64) as u64;
					// Preserve the fmp4 track's native timescale so a passthrough re-emit
					// doesn't go through a lossy microsecond detour.
					let timestamp = moq_net::Timestamp::new(pts, timescale)?;

					if offset + size > mdat.data.len() {
						return Err(Error::InvalidDataOffset.into());
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

					if max_timestamp.is_none_or(|max| timestamp >= max) {
						max_timestamp = Some(timestamp);
					}
					if min_timestamp.is_none_or(|min| timestamp <= min) {
						min_timestamp = Some(timestamp);
					}

					if let Some(last_timestamp) = track.last_timestamp
						&& let Ok(duration) = timestamp.checked_sub(last_timestamp)
						&& track.min_duration.is_none_or(|min| duration < min)
					{
						track.min_duration = Some(duration);
					}

					track.last_timestamp = Some(timestamp);

					if let Some(duration) = duration {
						dts += duration as u64;
					}
					offset += size;
					sample_index += 1;
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
			if !(track_data_start <= track_data_end && track_data_end <= mdat.data.len()) {
				return Err(Error::SampleRangeOutOfBounds {
					start: track_data_start,
					end: track_data_end,
					len: mdat.data.len(),
				}
				.into());
			}
			let track_mdat_data = &mdat.data[track_data_start..track_data_end];

			let mut adjusted_moof = single_traf_moof;

			// Apply structural (flag-presence) changes BEFORE measuring the encoded
			// size, otherwise the rewritten data_offset values are computed from a
			// stale size and the resulting fragment misaddresses sample data.
			// In particular: clearing tfhd.base_data_offset removes 8 bytes per traf,
			// and ensuring trun.data_offset is Some(...) reserves 4 bytes per trun.
			for traf_mut in &mut adjusted_moof.traf {
				traf_mut.tfhd.base_data_offset = None;
				// A zero default/sample duration is "unknown", not "instantaneous": drop it so
				// the re-emitted fragment carries no bogus zero that a decoder would honor.
				if traf_mut.tfhd.default_sample_duration == Some(0) {
					traf_mut.tfhd.default_sample_duration = None;
				}
				for trun_mut in &mut traf_mut.trun {
					// Reserve the data_offset field; the real value is filled in below.
					trun_mut.data_offset = Some(0);
					for entry in &mut trun_mut.entries {
						if entry.duration == Some(0) {
							entry.duration = None;
						}
					}
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
				match track.pending_sequence.take() {
					Some(sequence) => track.track.create_group(moq_net::GroupInfo { sequence })?,
					None => track.track.append_group()?,
				}
			} else {
				track.group.take().ok_or(Error::NoKeyframe)?
			};

			// Carry the fragment's earliest presentation time as the frame timestamp,
			// in the track's native timescale. The relay reads it off the wire; the
			// consumer still drives playback from the fragment's internal timing.
			let timestamp = min_timestamp.ok_or(Error::MissingTrun)?;
			let mut frame = g.create_frame(moq_net::FrameInfo {
				size: fragment_bytes.len() as u64,
				timestamp,
			})?;
			frame.write(fragment_bytes)?;
			frame.finish()?;

			track.group = Some(g);

			if let (Some(min), Some(max), Some(min_duration)) = (min_timestamp, max_timestamp, track.min_duration) {
				let jitter = max - min + min_duration;

				if track.jitter.is_none_or(|j| jitter < j) {
					track.jitter = Some(jitter);

					let mut catalog = self.catalog.lock();

					match track.kind {
						TrackKind::Video => {
							let config = catalog
								.video
								.renditions
								.get_mut(track.track.name())
								.ok_or_else(|| Error::MissingVideoTrack(track.track.name().to_string()))?;
							config.jitter = Some(jitter.into());
						}
						TrackKind::Audio => {
							let config = catalog
								.audio
								.renditions
								.get_mut(track.track.name())
								.ok_or_else(|| Error::MissingAudioTrack(track.track.name().to_string()))?;
							config.jitter = Some(jitter.into());
						}
					}
				}
			}
		}

		Ok(())
	}
}

impl<E: crate::catalog::hang::CatalogExt> Import<E> {
	/// Finish all tracks, flushing current groups.
	pub fn finish(&mut self) -> Result<()> {
		for track in self.tracks.values_mut() {
			if let Some(mut g) = track.group.take() {
				g.finish()?;
			}
			track.track.finish()?;
		}
		Ok(())
	}

	/// Close the current group on every track and open the next one at `sequence`.
	///
	/// Broadcast-wide: every track inside this fMP4 import advances together; per-track
	/// control is intentionally not exposed.
	pub fn seek(&mut self, sequence: u64) -> Result<()> {
		for track in self.tracks.values_mut() {
			if let Some(mut g) = track.group.take() {
				g.finish()?;
			}
			track.pending_sequence = Some(sequence);
		}
		Ok(())
	}
}

impl<E: crate::catalog::hang::CatalogExt> Drop for Import<E> {
	fn drop(&mut self) {
		let mut catalog = self.catalog.lock();

		for track in self.tracks.values() {
			match track.kind {
				TrackKind::Video => {
					catalog.video.renditions.remove(track.track.name());
				}
				TrackKind::Audio => {
					catalog.audio.renditions.remove(track.track.name());
				}
			}
		}
	}
}
