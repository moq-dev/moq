use std::collections::HashMap;
use std::task::Poll;
use std::time::Duration;

use bytes::Bytes;
use hang::catalog::{Catalog, Container, VideoConfig};
use mp4_atom::{DecodeMaybe, Encode};

use crate::Result;
use crate::catalog::CatalogFormat;
use crate::container::Frame;
use crate::container::fmp4::Error;

use crate::container::{CatalogSource, ExportSource};

/// Subscribe to a moq broadcast and produce a single fMP4 / CMAF byte stream.
///
/// Built from a [`moq_net::BroadcastConsumer`], `Export` subscribes to the hang catalog,
/// (un)subscribes per-rendition tracks as the catalog changes, decodes both Legacy and
/// CMAF tracks via a per-track source, and re-encodes everything as a merged init
/// segment + moof+mdat fragments in presentation-timestamp order across tracks. This
/// is what an fMP4 player (e.g. ffplay, MSE) expects.
///
/// Use [`next`](Self::next) to pull byte chunks: the first call returns the merged
/// init segment (ftyp + multi-track moov), subsequent calls return moof+mdat
/// fragments. By default each video fragment covers one GOP (rolled over on
/// keyframes); [`with_fragment_duration`](Self::with_fragment_duration) caps the
/// fragment duration for downstream consumers that throttle by fragment rate.
/// Returns `None` when the broadcast ends.
pub struct Export {
	broadcast: moq_net::BroadcastConsumer,
	catalog: Option<CatalogSource>,
	latency: Duration,
	fragment_duration: Option<Duration>,

	tracks: HashMap<String, Fmp4Track>,

	/// Most recent catalog snapshot. Used to build the init segment once every
	/// source's codec config is ready.
	catalog_snapshot: Option<Catalog>,

	/// Set after the init segment has been emitted; subsequent catalog updates only
	/// (un)subscribe tracks without re-emitting init.
	init_emitted: bool,
}

struct Fmp4Track {
	source: ExportSource,

	/// The next decoded frame from the source, used for cross-track timestamp ordering.
	pending: Option<Frame>,

	/// Frames accumulated for the current fragment. Flushed as a single
	/// moof+mdat on the next keyframe (video) or duration cap.
	buffer: Vec<Frame>,

	/// True if this track is video. Video tracks roll fragments on keyframes.
	is_video: bool,

	/// Whether the source has signalled end-of-track.
	finished: bool,

	track_id: u32,
	timescale: u64,
	sequence_number: u32,
}

impl Export {
	/// Subscribe to `broadcast` and produce fMP4 byte chunks, using the default
	/// catalog format ([`CatalogFormat::Hang`]).
	///
	/// Use [`with_catalog_format`](Self::with_catalog_format) to subscribe to a
	/// non-default catalog track (e.g. MSF).
	pub fn new(broadcast: moq_net::BroadcastConsumer) -> Result<Self> {
		Self::with_catalog_format(broadcast, CatalogFormat::default())
	}

	/// Subscribe to `broadcast` and produce fMP4 byte chunks, selecting an
	/// explicit `catalog_format` for track discovery.
	///
	/// Both formats drive the same internal `hang::Catalog`-based pipeline (MSF
	/// snapshots are converted on receipt), so the only observable difference
	/// is which wire catalog track is consumed.
	pub fn with_catalog_format(broadcast: moq_net::BroadcastConsumer, catalog_format: CatalogFormat) -> Result<Self> {
		let catalog = CatalogSource::new(&broadcast, catalog_format)?;

		Ok(Self {
			broadcast,
			catalog: Some(catalog),
			latency: Duration::ZERO,
			fragment_duration: None,
			tracks: HashMap::new(),
			catalog_snapshot: None,
			init_emitted: false,
		})
	}

	/// Set the maximum buffering latency for each per-track source.
	///
	/// See [`crate::container::Consumer::with_latency`] for the per-track skip behavior.
	/// Default is zero (skip aggressively).
	pub fn with_latency(mut self, latency: Duration) -> Self {
		self.latency = latency;
		self
	}

	/// Cap the fragment (moof+mdat) duration.
	///
	/// By default video fragments roll over on each keyframe (one fragment
	/// per GOP); audio-only tracks emit one fragment per sample. Setting this
	/// caps each fragment to roughly `duration` of frames, useful for
	/// downstream consumers that throttle by fragment rate. [`Duration::ZERO`]
	/// emits one fragment per frame (the historical behavior); otherwise the
	/// cap applies in addition to GOP rollover.
	///
	/// Accepts either `Duration` or `Option<Duration>` (where `None` restores
	/// the per-GOP default).
	pub fn with_fragment_duration(mut self, duration: impl Into<Option<Duration>>) -> Self {
		self.fragment_duration = duration.into();
		self
	}

	/// Get the next byte chunk.
	///
	/// The first call returns the merged init segment (ftyp + multi-track moov); each
	/// subsequent call returns one moof+mdat fragment. Fragments arrive in ascending
	/// timestamp order across tracks. Returns `None` when the catalog and every track
	/// have ended.
	pub async fn next(&mut self) -> Result<Option<Bytes>> {
		conducer::wait(|waiter| self.poll_next(waiter)).await
	}

	/// Poll-based variant of [`Self::next`].
	pub fn poll_next(&mut self, waiter: &conducer::Waiter) -> Poll<Result<Option<Bytes>>> {
		// 1. Drain catalog updates and (un)subscribe tracks accordingly.
		while let Some(catalog) = self.catalog.as_mut() {
			match catalog.poll_next(waiter)? {
				Poll::Ready(Some(snapshot)) => self.update_catalog(&snapshot)?,
				Poll::Ready(None) => {
					self.catalog = None;
					break;
				}
				Poll::Pending => break,
			}
		}

		// 2. Fill any empty pending slots by polling each source. ExportSource
		// has already applied any codec-shape transform (Avc3 → avc1) and
		// absorbed parameter-only frames.
		//
		// Pre-init: drop slices that arrived before this track's codec config
		// is ready, so the source keeps polling for SPS/PPS-bearing frames
		// instead of parking.
		let waiting_for_init = !self.init_emitted;
		for track in self.tracks.values_mut() {
			if track.pending.is_some() || track.finished {
				continue;
			}
			loop {
				match track.source.poll_read(waiter) {
					Poll::Ready(Ok(Some(frame))) => {
						if waiting_for_init && !track.source.header_ready() {
							continue;
						}
						track.pending = Some(frame);
						break;
					}
					Poll::Ready(Ok(None)) => {
						track.finished = true;
						break;
					}
					Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
					Poll::Pending => break,
				}
			}
		}

		// 3. Build and emit the init segment once every source has resolved
		// its codec config (immediately for CMAF-passthrough sources;
		// after the first keyframe for Avc3/Hev1 sources).
		if !self.init_emitted {
			if self.init_ready() {
				let init = self.build_init()?;
				self.init_emitted = true;
				return Poll::Ready(Ok(Some(init)));
			}
			// Still waiting for codec configs. If every track is finished and
			// the init still isn't buildable, the source ended before producing
			// enough info.
			if self.catalog.is_none() && self.tracks.values().all(|t| t.finished) {
				return Poll::Ready(Ok(None));
			}
			return Poll::Pending;
		}

		// 4. Pick the track whose pending frame has the smallest timestamp and
		// decide whether to flush its buffer before appending the new frame.
		let chosen = self
			.tracks
			.iter()
			.filter_map(|(name, t)| t.pending.as_ref().map(|f| (name.clone(), f.timestamp)))
			.min_by_key(|(_, ts)| *ts)
			.map(|(name, _)| name);

		if let Some(name) = chosen {
			let frag = self.fragment_duration;
			let has_video_track = self.tracks.values().any(|t| t.is_video);
			let track = self.tracks.get_mut(&name).unwrap();
			let frame = track.pending.take().unwrap();
			let flush_before = should_flush(track, &frame, frag, has_video_track);
			if flush_before {
				let frames = std::mem::take(&mut track.buffer);
				let emit = encode_fragment(track, frames)?;
				track.buffer.push(frame);
				return Poll::Ready(Ok(Some(emit)));
			}
			track.buffer.push(frame);
			// Frame appended to buffer; loop again to look for more work or a flush.
			return self.poll_next(waiter);
		}

		// 5. No pending frames. Flush any finished tracks' remaining buffers,
		// in ascending first-frame-timestamp order.
		let flushable = self
			.tracks
			.iter()
			.filter_map(|(name, t)| {
				if t.finished && !t.buffer.is_empty() {
					Some((name.clone(), t.buffer.first().unwrap().timestamp))
				} else {
					None
				}
			})
			.min_by_key(|(_, ts)| *ts)
			.map(|(name, _)| name);

		if let Some(name) = flushable {
			let track = self.tracks.get_mut(&name).unwrap();
			let frames = std::mem::take(&mut track.buffer);
			let emit = encode_fragment(track, frames)?;
			return Poll::Ready(Ok(Some(emit)));
		}

		// 6. If catalog is closed and every track is finished and drained, we're done.
		if self.catalog.is_none() && self.tracks.values().all(|t| t.finished && t.buffer.is_empty()) {
			return Poll::Ready(Ok(None));
		}

		// 7. Drop finished tracks with empty buffers so the next catalog update can re-add a track of the same name.
		self.tracks
			.retain(|_, t| !(t.finished && t.pending.is_none() && t.buffer.is_empty()));

		Poll::Pending
	}

	fn update_catalog(&mut self, catalog: &Catalog) -> Result<()> {
		let mut active: HashMap<String, ()> = HashMap::new();
		for name in catalog.video.renditions.keys() {
			active.insert(name.clone(), ());
		}
		for name in catalog.audio.renditions.keys() {
			active.insert(name.clone(), ());
		}

		// Add any new tracks. Subscribe via ExportSource which applies any
		// per-codec transform (Annex-B → length-prefixed) at pull time.
		let mut next_track_id = self.tracks.values().map(|t| t.track_id).max().unwrap_or(0) + 1;

		for (name, config) in &catalog.video.renditions {
			if self.tracks.contains_key(name) {
				continue;
			}
			let source = ExportSource::for_video(&self.broadcast, name, config, self.latency)?;
			let timescale = catalog_timescale_video(config);
			self.tracks.insert(
				name.clone(),
				Fmp4Track {
					source,
					pending: None,
					buffer: Vec::new(),
					is_video: true,
					finished: false,
					track_id: next_track_id,
					timescale,
					sequence_number: 1,
				},
			);
			next_track_id += 1;
		}

		for (name, config) in &catalog.audio.renditions {
			if self.tracks.contains_key(name) {
				continue;
			}
			let source = ExportSource::for_audio(&self.broadcast, name, config, self.latency)?;
			let timescale = catalog_timescale_audio(config);
			self.tracks.insert(
				name.clone(),
				Fmp4Track {
					source,
					pending: None,
					buffer: Vec::new(),
					is_video: false,
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
		self.catalog_snapshot = Some(catalog.clone());

		Ok(())
	}

	/// True once every source has resolved its codec config so we can build
	/// the merged init segment.
	fn init_ready(&self) -> bool {
		self.catalog_snapshot.is_some() && self.tracks.values().all(|t| t.source.header_ready())
	}

	/// Build the merged ftyp + multi-track moov init segment from the cached
	/// catalog snapshot. CMAF tracks pass their existing init segment through;
	/// Legacy tracks synthesize a `trak` from codec config + dimensions.
	fn build_init(&self) -> Result<Bytes> {
		let catalog = self.catalog_snapshot.as_ref().ok_or(Error::NoCatalogSnapshot)?;

		let mut traks: Vec<mp4_atom::Trak> = Vec::new();
		let mut trexs: Vec<mp4_atom::Trex> = Vec::new();
		let mut ftyp_data: Option<mp4_atom::Ftyp> = None;

		for (name, config) in &catalog.video.renditions {
			let track = self
				.tracks
				.get(name)
				.ok_or_else(|| Error::MissingVideoTrack(name.clone()))?;
			match &config.container {
				Container::Cmaf { init, .. } => {
					extract_init(init, &mut ftyp_data, &mut traks, &mut trexs)?;
				}
				Container::Legacy | Container::Loc => {
					let description = track.source.description().ok_or(Error::MissingVideoConfig)?;
					let trak = crate::container::fmp4::synthesize_video_trak(
						track.track_id,
						track.timescale,
						config,
						description.as_ref(),
					)?;
					trexs.push(mp4_atom::Trex {
						track_id: trak.tkhd.track_id,
						default_sample_description_index: 1,
						..Default::default()
					});
					traks.push(trak);
				}
			}
		}

		for (name, config) in &catalog.audio.renditions {
			let track = self
				.tracks
				.get(name)
				.ok_or_else(|| Error::MissingAudioTrack(name.clone()))?;
			match &config.container {
				Container::Cmaf { init, .. } => {
					extract_init(init, &mut ftyp_data, &mut traks, &mut trexs)?;
				}
				Container::Legacy | Container::Loc => {
					let trak = crate::container::fmp4::synthesize_audio_trak(track.track_id, track.timescale, config)?;
					trexs.push(mp4_atom::Trex {
						track_id: trak.tkhd.track_id,
						default_sample_description_index: 1,
						..Default::default()
					});
					traks.push(trak);
				}
			}
		}

		let ftyp = ftyp_data.unwrap_or(mp4_atom::Ftyp {
			major_brand: b"isom".into(),
			minor_version: 0x200,
			compatible_brands: vec![b"isom".into(), b"iso6".into(), b"mp41".into()],
		});
		let timescale = traks.first().map(|t| t.mdia.mdhd.timescale).unwrap_or(1000);

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
}

/// Pull ftyp + moov from a single-track CMAF init segment and merge into the
/// caller's accumulators. Original track ids are preserved so passthrough
/// fragments keep matching their moov entries.
fn extract_init(
	init: &Bytes,
	ftyp_data: &mut Option<mp4_atom::Ftyp>,
	traks: &mut Vec<mp4_atom::Trak>,
	trexs: &mut Vec<mp4_atom::Trex>,
) -> Result<()> {
	let mut cursor = std::io::Cursor::new(init.as_ref());
	while let Some(atom) = mp4_atom::Any::decode_maybe(&mut cursor)? {
		match atom {
			mp4_atom::Any::Ftyp(f) if ftyp_data.is_none() => {
				*ftyp_data = Some(f);
			}
			mp4_atom::Any::Moov(moov) => {
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
	Ok(())
}

/// Should we flush `track.buffer` *before* appending the incoming `frame`?
///
/// Triggers:
/// - Video keyframe and buffer non-empty (one fragment per GOP)
/// - Optional duration cap exceeded
/// - Per-frame mode (`Some(ZERO)`)
/// - Audio in an audio-only broadcast under default `None` mode (otherwise
///   the buffer would never flush — no keyframe boundary and no time cap)
fn should_flush(track: &Fmp4Track, frame: &Frame, fragment_duration: Option<Duration>, has_video_track: bool) -> bool {
	if track.buffer.is_empty() {
		return false;
	}
	if track.is_video && frame.keyframe {
		return true;
	}
	match fragment_duration {
		Some(d) if d.is_zero() => true,
		Some(d) => {
			// Frames within a track are in *decode* order; B-frames have
			// non-monotonic PTS, so the span of the buffer is min..max of all
			// PTS, not just first..incoming.
			let mut min = Duration::from(frame.timestamp);
			let mut max = min;
			for f in &track.buffer {
				let pts = Duration::from(f.timestamp);
				if pts < min {
					min = pts;
				}
				if pts > max {
					max = pts;
				}
			}
			max.saturating_sub(min) >= d
		}
		// No video keyframe will ever arrive to roll the fragment, so for
		// audio-only broadcasts in `None` mode we fall back to per-frame
		// fragments (matches the pre-batching default).
		None => !track.is_video && !has_video_track,
	}
}

/// Encode a buffered run of samples as a single CMAF moof+mdat fragment.
fn encode_fragment(track: &mut Fmp4Track, frames: Vec<Frame>) -> Result<Bytes> {
	if frames.is_empty() {
		return Err(Error::NoFrames.into());
	}
	let seq = track.sequence_number;
	track.sequence_number += 1;
	let timescale = moq_net::Timescale::new(track.timescale)?;
	Ok(crate::container::fmp4::encode_fragment(
		track.track_id,
		timescale,
		seq,
		&frames,
	)?)
}

fn catalog_timescale_video(config: &VideoConfig) -> u64 {
	match &config.container {
		Container::Cmaf { init, .. } => {
			parse_timescale_from_init(init).unwrap_or_else(|_| crate::container::fmp4::default_video_timescale(config))
		}
		Container::Loc | Container::Legacy => crate::container::fmp4::default_video_timescale(config),
	}
}

fn catalog_timescale_audio(config: &hang::catalog::AudioConfig) -> u64 {
	match &config.container {
		Container::Cmaf { init, .. } => parse_timescale_from_init(init).unwrap_or(config.sample_rate as u64),
		Container::Loc | Container::Legacy => config.sample_rate as u64,
	}
}

fn parse_timescale_from_init(init: &[u8]) -> Result<u64> {
	let mut cursor = std::io::Cursor::new(init);
	while let Some(atom) = mp4_atom::Any::decode_maybe(&mut cursor)? {
		if let mp4_atom::Any::Moov(moov) = atom {
			let trak = moov.trak.first().ok_or(Error::NoTracks)?;
			return Ok(trak.mdia.mdhd.timescale as u64);
		}
	}
	Err(Error::NoMoov.into())
}
