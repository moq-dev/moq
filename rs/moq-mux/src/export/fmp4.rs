use std::collections::HashMap;
use std::task::Poll;
use std::time::Duration;

use anyhow::Context;
use bytes::Bytes;
use hang::catalog::{Catalog, Container, VideoConfig};
use mp4_atom::{DecodeMaybe, Encode};

use crate::container::{Consumer, Frame, Hang};

/// Subscribe to a moq broadcast and produce a single fMP4 / CMAF byte stream.
///
/// Built from a [`moq_lite::BroadcastConsumer`], `Fmp4` subscribes to the hang catalog,
/// (un)subscribes per-rendition tracks as the catalog changes, decodes both Legacy and
/// CMAF tracks via [`Consumer<Hang>`], and re-encodes everything as a merged init
/// segment + moof+mdat fragments in presentation-timestamp order across tracks. This
/// is what an fMP4 player (e.g. ffplay, MSE) expects.
///
/// Use [`next`](Self::next) to pull byte chunks: the first call returns the merged
/// init segment (ftyp + multi-track moov), subsequent calls return moof+mdat
/// fragments. Returns `None` when the broadcast ends.
pub struct Fmp4 {
	broadcast: moq_lite::BroadcastConsumer,
	catalog: Option<crate::catalog::Consumer>,
	latency: Duration,

	tracks: HashMap<String, Fmp4Track>,

	/// Queued init segment, emitted on the first [`next`](Self::next) call after the
	/// initial catalog snapshot has been processed.
	init_pending: Option<Bytes>,

	/// Set after the init segment has been emitted; subsequent catalog updates only
	/// (un)subscribe tracks without re-emitting init.
	init_emitted: bool,
}

struct Fmp4Track {
	consumer: Consumer<Hang>,

	/// The next decoded frame, used for cross-track timestamp ordering.
	pending: Option<Frame>,

	/// Whether the consumer has signalled end-of-track.
	finished: bool,

	track_id: u32,
	timescale: u64,
	sequence_number: u32,
}

impl Fmp4 {
	/// Subscribe to `broadcast` and produce fMP4 byte chunks.
	///
	/// The hang catalog is subscribed internally; per-rendition tracks are (un)subscribed
	/// as the catalog changes.
	pub fn new(broadcast: moq_lite::BroadcastConsumer) -> Result<Self, crate::Error> {
		let catalog_track = broadcast.subscribe_track(&hang::Catalog::default_track())?;
		let catalog = crate::catalog::Consumer::new(catalog_track);

		Ok(Self {
			broadcast,
			catalog: Some(catalog),
			latency: Duration::ZERO,
			tracks: HashMap::new(),
			init_pending: None,
			init_emitted: false,
		})
	}

	/// Set the maximum buffering latency for each per-track [`Consumer`].
	///
	/// See [`Consumer::with_latency`] for the per-track skip behavior. Default is zero
	/// (skip aggressively).
	pub fn with_latency(mut self, latency: Duration) -> Self {
		self.latency = latency;
		self
	}

	/// Get the next byte chunk.
	///
	/// The first call returns the merged init segment (ftyp + multi-track moov); each
	/// subsequent call returns one moof+mdat fragment. Fragments arrive in ascending
	/// timestamp order across tracks. Returns `None` when the catalog and every track
	/// have ended.
	pub async fn next(&mut self) -> anyhow::Result<Option<Bytes>> {
		conducer::wait(|waiter| self.poll_next(waiter)).await
	}

	/// Poll-based variant of [`Self::next`].
	pub fn poll_next(&mut self, waiter: &conducer::Waiter) -> Poll<anyhow::Result<Option<Bytes>>> {
		// 1. Drain catalog updates and (un)subscribe tracks accordingly.
		while let Some(catalog) = self.catalog.as_mut() {
			match catalog.poll_next(waiter).map_err(crate::Error::from)? {
				Poll::Ready(Some(snapshot)) => self.update_catalog(&snapshot)?,
				Poll::Ready(None) => {
					self.catalog = None;
					break;
				}
				Poll::Pending => break,
			}
		}

		// 2. Emit the init segment once it's been built.
		if !self.init_emitted
			&& let Some(init) = self.init_pending.take()
		{
			self.init_emitted = true;
			return Poll::Ready(Ok(Some(init)));
		}

		// 3. Fill any empty pending slots by polling each consumer.
		for track in self.tracks.values_mut() {
			if track.pending.is_some() || track.finished {
				continue;
			}
			match track.consumer.poll_read(waiter) {
				Poll::Ready(Ok(Some(frame))) => track.pending = Some(frame),
				Poll::Ready(Ok(None)) => track.finished = true,
				Poll::Ready(Err(e)) => return Poll::Ready(Err(e.into())),
				Poll::Pending => {}
			}
		}

		// 4. Pick the track whose pending frame has the smallest timestamp.
		let chosen = self
			.tracks
			.iter()
			.filter_map(|(name, t)| t.pending.as_ref().map(|f| (name.clone(), f.timestamp)))
			.min_by_key(|(_, ts)| *ts)
			.map(|(name, _)| name);

		if let Some(name) = chosen {
			let track = self.tracks.get_mut(&name).unwrap();
			let frame = track.pending.take().unwrap();
			let bytes = encode_fragment(track, &frame)?;
			return Poll::Ready(Ok(Some(bytes)));
		}

		// 5. If catalog is closed and every track is finished, we're done.
		if self.catalog.is_none() && self.tracks.values().all(|t| t.finished) {
			return Poll::Ready(Ok(None));
		}

		// 6. Drop finished tracks so the next catalog update can re-add a track of the same name.
		self.tracks.retain(|_, t| !(t.finished && t.pending.is_none()));

		Poll::Pending
	}

	fn update_catalog(&mut self, catalog: &Catalog) -> anyhow::Result<()> {
		// Build the init segment on the first catalog snapshot. We take a snapshot of
		// init_data + timescales now since the catalog can change later, but the init
		// segment is emitted only once.
		if !self.init_emitted && self.init_pending.is_none() {
			self.init_pending = Some(build_init(catalog)?);
		}

		let mut active: HashMap<String, &Container> = HashMap::new();
		for (name, config) in &catalog.video.renditions {
			active.insert(name.clone(), &config.container);
		}
		for (name, config) in &catalog.audio.renditions {
			active.insert(name.clone(), &config.container);
		}

		// Add any new tracks. We use the rendition's catalog index as the track_id so
		// fragment moof traf.tfhd.track_id matches the moov trak ids in the init segment.
		let mut next_track_id = self.tracks.values().map(|t| t.track_id).max().unwrap_or(0) + 1;

		for (name, container) in &active {
			if self.tracks.contains_key(name) {
				continue;
			}

			let media: Hang = (*container).try_into()?;
			let track = self.broadcast.subscribe_track(&moq_lite::Track::new(name.clone()))?;
			let consumer = Consumer::new(track, media).with_latency(self.latency);

			let timescale = catalog_timescale(catalog, name).context("track not in catalog")?;

			self.tracks.insert(
				name.clone(),
				Fmp4Track {
					consumer,
					pending: None,
					finished: false,
					track_id: next_track_id,
					timescale,
					sequence_number: 1,
				},
			);
			next_track_id += 1;
		}

		// Remove tracks no longer in the catalog.
		self.tracks.retain(|name, _| active.contains_key(name));

		Ok(())
	}
}

/// Build the merged ftyp + multi-track moov init segment from a catalog.
fn build_init(catalog: &Catalog) -> anyhow::Result<Bytes> {
	let mut traks = Vec::new();
	let mut trexs = Vec::new();
	let mut ftyp_data = None;

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

/// Encode one decoded frame as a CMAF moof+mdat fragment.
fn encode_fragment(track: &mut Fmp4Track, frame: &Frame) -> anyhow::Result<Bytes> {
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

fn catalog_timescale(catalog: &Catalog, name: &str) -> Option<u64> {
	if let Some(config) = catalog.video.renditions.get(name) {
		return Some(match &config.container {
			Container::Cmaf { init } => parse_timescale_from_init(init).ok()?,
			Container::Legacy => guess_video_timescale(config),
		});
	}
	if let Some(config) = catalog.audio.renditions.get(name) {
		return Some(match &config.container {
			Container::Cmaf { init } => parse_timescale_from_init(init).ok()?,
			Container::Legacy => config.sample_rate as u64,
		});
	}
	None
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
