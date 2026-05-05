use anyhow::Context;
use bytes::Bytes;
use hang::catalog::{Catalog, Container, VideoConfig};
use mp4_atom::{DecodeMaybe, Encode};

use crate::container::Frame;

/// Builds fMP4 / CMAF wire output: a merged init segment plus per-frame moof+mdat fragments.
///
/// `Fmp4` is a stateful encoder over the tracks in a hang catalog. [`Fmp4::init`] produces the
/// merged ftyp + moov (one trak per rendition, brands `isom`/`iso6`/`mp41`); [`Fmp4::frame`]
/// re-encodes a decoded [`Frame`] as a moof+mdat fragment for a named track. Per-track timescale
/// and sequence-number state lives inside `Fmp4`, so successive `frame()` calls on the same
/// track produce a valid fragment sequence.
///
/// Combine with [`Muxed`](crate::export::Muxed) to convert a moq broadcast (any container) into
/// a single fMP4 byte stream — typically for piping to a player like `ffplay` or for MSE.
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
				Container::Cmaf { init } => parse_timescale_from_init(init)?,
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
				Container::Cmaf { init } => parse_timescale_from_init(init)?,
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
		let mut track_inits: Vec<&Bytes> = Vec::new();
		for config in catalog.video.renditions.values() {
			match &config.container {
				Container::Cmaf { init } => track_inits.push(init),
				Container::Legacy => anyhow::bail!("track is not CMAF"),
			}
		}
		for config in catalog.audio.renditions.values() {
			match &config.container {
				Container::Cmaf { init } => track_inits.push(init),
				Container::Legacy => anyhow::bail!("track is not CMAF"),
			}
		}

		for init in &track_inits {
			let mut cursor = std::io::Cursor::new(init.as_ref());
			while let Some(atom) = mp4_atom::Any::decode_maybe(&mut cursor)? {
				match atom {
					mp4_atom::Any::Ftyp(f) if ftyp_data.is_none() => {
						ftyp_data = Some(f);
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

	/// Re-encode a decoded media frame as a CMAF moof+mdat fragment for the named track.
	///
	/// `track_name` must match a rendition in the catalog passed to [`Self::new`]. The frame's
	/// timestamp is rescaled to the track's timescale, and `frame.keyframe` controls the trun
	/// sample-flags (sync vs depends-on).
	pub fn frame(&mut self, track_name: &str, frame: &Frame) -> anyhow::Result<Bytes> {
		let track = self
			.tracks
			.iter_mut()
			.find(|t| t.name == track_name)
			.context("unknown track")?;

		let dts = frame.timestamp.as_micros() as u64 * track.timescale / 1_000_000;
		let size = frame.payload.len() as u32;
		let flags = if frame.keyframe { 0x0200_0000 } else { 0x0001_0000 };

		let seq = track.sequence_number;
		track.sequence_number += 1;

		// First pass to get moof size (use Some(0) so trun includes the data_offset field).
		let moof = build_moof(seq, track.track_id, dts, size, flags, Some(0));
		let mut buf = Vec::new();
		moof.encode(&mut buf)?;
		let moof_size = buf.len();

		// Second pass with data_offset pointing past moof + mdat header (8 bytes).
		let data_offset = (moof_size + 8) as i32;
		let moof = build_moof(seq, track.track_id, dts, size, flags, Some(data_offset));
		buf.clear();
		moof.encode(&mut buf)?;

		let mdat = mp4_atom::Mdat {
			data: frame.payload.to_vec(),
		};
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

fn parse_timescale_from_init(init: &[u8]) -> anyhow::Result<u64> {
	let mut cursor = std::io::Cursor::new(init);
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
