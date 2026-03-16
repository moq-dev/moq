use anyhow::Context;
use base64::Engine;
use bytes::Bytes;
use hang::catalog::{Catalog, Container, VideoConfig};
use mp4_atom::{DecodeMaybe, Encode};

use super::OrderedFrame;

/// Produces fMP4 init segments and per-frame moof+mdat fragments from catalog info.
///
/// Used for exporting a broadcast to stdout as a playable fMP4 stream.
pub struct Fmp4 {
	tracks: Vec<Fmp4ExportTrack>,
}

struct Fmp4ExportTrack {
	name: String,
	track_id: u32,
	timescale: u64,
	sequence_number: u32,
}

impl Fmp4 {
	/// Build from catalog configuration.
	pub fn new(catalog: &Catalog) -> anyhow::Result<Self> {
		let mut tracks = Vec::new();
		let mut track_id = 1u32;

		for (name, config) in &catalog.video.renditions {
			let timescale = match &config.container {
				Container::Cmaf { init_data } => parse_timescale_from_init(init_data)?,
				Container::Legacy => guess_video_timescale(config),
			};

			tracks.push(Fmp4ExportTrack {
				name: name.clone(),
				track_id,
				timescale,
				sequence_number: 1,
			});
			track_id += 1;
		}

		for (name, config) in &catalog.audio.renditions {
			let timescale = match &config.container {
				Container::Cmaf { init_data } => parse_timescale_from_init(init_data)?,
				Container::Legacy => config.sample_rate as u64,
			};

			tracks.push(Fmp4ExportTrack {
				name: name.clone(),
				track_id,
				timescale,
				sequence_number: 1,
			});
			track_id += 1;
		}

		Ok(Self { tracks })
	}

	/// Generate the init segment (ftyp + moov) for all tracks.
	///
	/// For multi-track output, decodes each track's init_data, extracts trak+trex,
	/// and builds a merged ftyp+moov with renumbered track IDs.
	pub fn init(&self, catalog: &Catalog) -> anyhow::Result<Bytes> {
		let mut traks = Vec::new();
		let mut trexs = Vec::new();
		let mut ftyp_data = None;

		// Collect all track init data
		let mut track_inits: Vec<&str> = Vec::new();
		for config in catalog.video.renditions.values() {
			match &config.container {
				Container::Cmaf { init_data } => track_inits.push(init_data),
				Container::Legacy => anyhow::bail!("track is not CMAF"),
			}
		}
		for config in catalog.audio.renditions.values() {
			match &config.container {
				Container::Cmaf { init_data } => track_inits.push(init_data),
				Container::Legacy => anyhow::bail!("track is not CMAF"),
			}
		}

		for init_data_b64 in &track_inits {
			let data = base64::engine::general_purpose::STANDARD
				.decode(init_data_b64)
				.context("invalid base64 init_data")?;

			let mut cursor = std::io::Cursor::new(&data);
			while let Some(atom) = mp4_atom::Any::decode_maybe(&mut cursor)? {
				match atom {
					mp4_atom::Any::Ftyp(f) => {
						if ftyp_data.is_none() {
							ftyp_data = Some(f);
						}
					}
					mp4_atom::Any::Moov(moov) => {
						// Preserve original track IDs to match CMAF passthrough fragments
						for trak in moov.trak {
							traks.push(trak);
						}

						if let Some(mvex) = moov.mvex {
							for trex in mvex.trex {
								trexs.push(trex);
							}
						}
					}
					_ => {}
				}
			}
		}

		let ftyp = ftyp_data.context("no ftyp found in any init segment")?;

		let timescale = traks.first().map(|t| t.mdia.mdhd.timescale).unwrap_or(90000);

		let moov = mp4_atom::Moov {
			mvhd: mp4_atom::Mvhd {
				timescale,
				..Default::default()
			},
			trak: traks,
			mvex: if trexs.is_empty() {
				None
			} else {
				Some(mp4_atom::Mvex {
					trex: trexs,
					..Default::default()
				})
			},
			..Default::default()
		};

		let mut buf = Vec::new();
		ftyp.encode(&mut buf)?;
		moov.encode(&mut buf)?;
		Ok(Bytes::from(buf))
	}

	/// Encode a single frame as a moof+mdat fragment.
	pub fn frame(&mut self, track_name: &str, frame: &OrderedFrame) -> anyhow::Result<Bytes> {
		let track = self
			.tracks
			.iter_mut()
			.find(|t| t.name == track_name)
			.context("unknown track")?;

		let dts = frame.timestamp.as_micros() as u64 * track.timescale / 1_000_000;
		let payload: Vec<u8> = frame.payload.clone().into_iter().flat_map(|c| c.into_iter()).collect();
		let keyframe = frame.is_keyframe();

		let flags = if keyframe { 0x0200_0000 } else { 0x0001_0000 };

		let seq = track.sequence_number;
		track.sequence_number += 1;

		// First pass to get moof size (use Some(0) so trun includes the data_offset field)
		let moof = build_moof(seq, track.track_id, dts, payload.len() as u32, flags, Some(0));
		let mut buf = Vec::new();
		moof.encode(&mut buf)?;
		let moof_size = buf.len();

		// Second pass with data_offset
		let data_offset = (moof_size + 8) as i32;
		let moof = build_moof(seq, track.track_id, dts, payload.len() as u32, flags, Some(data_offset));
		buf.clear();
		moof.encode(&mut buf)?;

		let mdat = mp4_atom::Mdat { data: payload };
		mdat.encode(&mut buf)?;

		Ok(Bytes::from(buf))
	}
}

fn build_moof(seq: u32, track_id: u32, dts: u64, size: u32, flags: u32, data_offset: Option<i32>) -> mp4_atom::Moof {
	mp4_atom::Moof {
		mfhd: mp4_atom::Mfhd { sequence_number: seq },
		traf: vec![mp4_atom::Traf {
			tfhd: mp4_atom::Tfhd {
				track_id,
				..Default::default()
			},
			tfdt: Some(mp4_atom::Tfdt {
				base_media_decode_time: dts,
			}),
			trun: vec![mp4_atom::Trun {
				data_offset,
				entries: vec![mp4_atom::TrunEntry {
					size: Some(size),
					flags: Some(flags),
					..Default::default()
				}],
			}],
			..Default::default()
		}],
	}
}

fn parse_timescale_from_init(init_data_b64: &str) -> anyhow::Result<u64> {
	let data = base64::engine::general_purpose::STANDARD
		.decode(init_data_b64)
		.context("invalid base64")?;
	let mut cursor = std::io::Cursor::new(&data);
	while let Some(atom) = mp4_atom::Any::decode_maybe(&mut cursor)? {
		if let mp4_atom::Any::Moov(moov) = atom {
			let trak = moov.trak.first().context("no tracks in moov")?;
			return Ok(trak.mdia.mdhd.timescale as u64);
		}
	}
	anyhow::bail!("no moov in init data")
}

fn guess_video_timescale(config: &VideoConfig) -> u64 {
	if let Some(fps) = config.framerate {
		(fps * 1000.0) as u64
	} else {
		90000
	}
}
