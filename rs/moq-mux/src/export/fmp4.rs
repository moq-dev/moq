use anyhow::Context;
use base64::Engine;
use bytes::Bytes;
use hang::catalog::{Catalog, Container, VideoConfig};
use hang::container::OrderedFrame;
use mp4_atom::{DecodeMaybe, Encode};

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
	///
	/// Currently only supports single-track exports. Multi-track fMP4 requires
	/// merging moov atoms which is not yet implemented.
	pub fn new(catalog: &Catalog) -> anyhow::Result<Self> {
		let total_tracks = catalog.video.renditions.len() + catalog.audio.renditions.len();
		anyhow::ensure!(
			total_tracks <= 1,
			"multi-track fMP4 export is not yet supported ({total_tracks} tracks); \
			 init segment merging is required for multi-track moov construction"
		);

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
	/// For CMAF tracks, the init data is already in the catalog; for multi-track
	/// output we'd need to merge them. For now, returns the first track's init data
	/// if all tracks are CMAF, or builds one from scratch.
	pub fn init(&self, catalog: &Catalog) -> anyhow::Result<Bytes> {
		// For single-track CMAF, just decode and return the init data
		// For multi-track, we'd need to merge moov atoms (complex, deferred)
		// For now, find the first CMAF track and use its init data
		for config in catalog.video.renditions.values() {
			if let Container::Cmaf { init_data } = &config.container {
				let data = base64::engine::general_purpose::STANDARD
					.decode(init_data)
					.context("invalid base64 init_data")?;
				return Ok(Bytes::from(data));
			}
		}
		for config in catalog.audio.renditions.values() {
			if let Container::Cmaf { init_data } = &config.container {
				let data = base64::engine::general_purpose::STANDARD
					.decode(init_data)
					.context("invalid base64 init_data")?;
				return Ok(Bytes::from(data));
			}
		}

		anyhow::bail!("no CMAF tracks found in catalog")
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
