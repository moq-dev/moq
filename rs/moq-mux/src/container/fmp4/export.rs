use std::collections::HashMap;
use std::task::Poll;
use std::time::Duration;

use bytes::Bytes;
use hang::catalog::{Catalog, Container, VideoConfig};
use mp4_atom::{DecodeMaybe, Encode};

use crate::Result;
use crate::catalog::Stream;
use crate::container::ExportSource;
use crate::container::Frame;
use crate::container::fmp4::Error;
use moq_net::Timestamp;

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
///
/// [`next_fragment`](Self::next_fragment) returns the same bytes wrapped in a
/// [`Fragment`] that also carries whether the chunk is the init segment, whether
/// a media fragment begins at a sync sample, and its presentation duration. A
/// segmenting consumer (e.g. an HLS/LL-HLS packager) needs that to map fragments
/// onto segments and parts; narrow the catalog to a single rendition with
/// [`catalog::Filter`](crate::catalog::Filter) so the fragments belong to one track.
pub struct Export<S: Stream> {
	broadcast: moq_net::BroadcastConsumer,
	catalog: Option<S>,
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

/// One emitted CMAF chunk: either the init segment or a moof+mdat fragment,
/// with the metadata a segmenting consumer needs.
#[derive(Clone, Debug)]
pub struct Fragment {
	/// The encoded bytes: ftyp+moov for the init, otherwise one moof+mdat.
	pub data: Bytes,

	/// True only for the first emit (the init segment).
	pub init: bool,

	/// A media fragment that begins at a sync sample, so it can start a segment.
	/// Video fragments are independent only at a GOP boundary (keyframe); audio
	/// fragments are always independent. Always false for the init segment.
	pub independent: bool,

	/// Presentation duration of the fragment in seconds (0 for the init segment).
	pub duration: f64,
}

struct Fmp4Track {
	source: ExportSource,

	/// The next decoded frame from the source, used for cross-track timestamp ordering.
	pending: Option<Frame>,

	/// Frames accumulated for the current fragment. Flushed as a single
	/// moof+mdat on the next keyframe (video) or duration cap.
	buffer: Vec<Frame>,

	/// Whether the first frame of the current `buffer` was a keyframe, i.e. the
	/// fragment it produces can start an HLS segment. Meaningless for audio.
	buffer_independent: bool,

	/// True if this track is video. Video tracks roll fragments on keyframes.
	is_video: bool,

	/// Fallback duration for a trailing frame that carries no per-sample duration
	/// (Legacy / LOC sources). Derived from the catalog framerate / sample rate.
	default_frame: Duration,

	/// Whether the source has signalled end-of-track.
	finished: bool,

	track_id: u32,
	timescale: u64,
	sequence_number: u32,
}

impl<S: Stream> Export<S> {
	/// Subscribe to `broadcast` and produce fMP4 byte chunks, driving track
	/// (un)subscription from `catalog`.
	///
	/// `catalog` is any [`Stream`] of catalog snapshots, typically a
	/// [`catalog::Consumer`](crate::catalog::Consumer) directly, or wrapped in
	/// [`catalog::Filter`](crate::catalog::Filter) /
	/// [`catalog::Target`](crate::catalog::Target) to narrow the rendition set.
	pub fn new(broadcast: moq_net::BroadcastConsumer, catalog: S) -> Self {
		Self {
			broadcast,
			catalog: Some(catalog),
			latency: Duration::ZERO,
			fragment_duration: None,
			tracks: HashMap::new(),
			catalog_snapshot: None,
			init_emitted: false,
		}
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
		Ok(self.next_fragment().await?.map(|f| f.data))
	}

	/// Poll-based variant of [`Self::next`].
	pub fn poll_next(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<Bytes>>> {
		Poll::Ready(Ok(std::task::ready!(self.poll_next_fragment(waiter)?).map(|f| f.data)))
	}

	/// Like [`next`](Self::next) but returns a [`Fragment`] carrying segment metadata
	/// (init flag, sync-sample independence, presentation duration).
	pub async fn next_fragment(&mut self) -> Result<Option<Fragment>> {
		kio::wait(|waiter| self.poll_next_fragment(waiter)).await
	}

	/// Poll-based variant of [`Self::next_fragment`].
	pub fn poll_next_fragment(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<Fragment>>> {
		// 1. Drain catalog updates and (un)subscribe tracks accordingly.
		while let Some(catalog) = self.catalog.as_mut() {
			match catalog.poll_next(waiter)? {
				Poll::Ready(Some(snapshot)) => self.update_catalog(&snapshot.media())?,
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
				return Poll::Ready(Ok(Some(Fragment {
					data: init,
					init: true,
					independent: false,
					duration: 0.0,
				})));
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
				let fragment = emit_fragment(track, frames, Some(&frame))?;
				// The flushed run is done; the incoming frame opens the next buffer.
				track.buffer_independent = frame.keyframe;
				track.buffer.push(frame);
				return Poll::Ready(Ok(Some(fragment)));
			}
			if track.buffer.is_empty() {
				track.buffer_independent = frame.keyframe;
			}
			track.buffer.push(frame);
			// Frame appended to buffer; loop again to look for more work or a flush.
			return self.poll_next_fragment(waiter);
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
			let fragment = emit_fragment(track, frames, None)?;
			return Poll::Ready(Ok(Some(fragment)));
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
					buffer_independent: false,
					is_video: true,
					default_frame: Duration::from_secs_f64(1.0 / config.framerate.unwrap_or(30.0)),
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
					buffer_independent: false,
					is_video: false,
					// Fallback for a duration-less trailing sample (~1024 samples/frame).
					default_frame: Duration::from_secs_f64(1024.0 / config.sample_rate.max(1) as f64),
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
					// H.264/H.265 need a synthesized config record here; VP8 has none.
					let description = track.source.description();
					let trak = crate::container::fmp4::synthesize_video_trak(
						track.track_id,
						track.timescale,
						config,
						description.map(|d| d.as_ref()),
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

/// Encode a buffered run and wrap it with the metadata a segmenting consumer needs.
fn emit_fragment(track: &mut Fmp4Track, frames: Vec<Frame>, successor: Option<&Frame>) -> Result<Fragment> {
	// Audio has no keyframes, so every audio fragment is independent; video is
	// independent only when its buffer opened on a keyframe (a GOP boundary).
	let independent = !track.is_video || track.buffer_independent;
	let frames = infer_missing_durations(frames, successor, track.default_frame);
	let duration = fragment_seconds(&frames, track.default_frame);
	let data = encode_fragment(track, frames)?;
	Ok(Fragment {
		data,
		init: false,
		independent,
		duration,
	})
}

/// Presentation duration of a fragment, in seconds.
///
/// When every sample carries a duration (the CMAF case) the per-sample durations
/// tile the timeline, so their sum is exact. Legacy / LOC sources carry none, so
/// fall back to the presentation span plus one `default_frame` for the trailing
/// sample (which has no successor to bound it).
fn fragment_seconds(frames: &[Frame], default_frame: Duration) -> f64 {
	if frames.is_empty() {
		return 0.0;
	}
	if frames
		.iter()
		.all(|f| f.duration.is_some_and(|duration| !duration.is_zero()))
	{
		return frames
			.iter()
			.map(|f| Duration::from(f.duration.unwrap()))
			.sum::<Duration>()
			.as_secs_f64();
	}
	let mut min = Duration::MAX;
	let mut max = Duration::ZERO;
	for f in frames {
		let pts = Duration::from(f.timestamp);
		min = min.min(pts);
		max = max.max(pts);
	}
	((max - min) + default_frame).as_secs_f64()
}

fn infer_missing_durations(mut frames: Vec<Frame>, successor: Option<&Frame>, default_frame: Duration) -> Vec<Frame> {
	let infer_from_pts = pts_monotonic(&frames, successor);
	// Express the fallback at the frames' own timescale so it matches the durations derived from
	// their timestamps (a `Timestamp` carries its scale, and `try_from(Duration)` is nanosecond-scale).
	let fallback = frames.first().map(|f| f.timestamp.scale()).and_then(|scale| {
		Timestamp::try_from(default_frame)
			.ok()
			.and_then(|t| t.convert(scale).ok())
	});

	for i in 0..frames.len() {
		if frames[i].duration.is_some_and(|duration| !duration.is_zero()) {
			continue;
		}

		frames[i].duration = infer_from_pts
			.then(|| next_timestamp(&frames, successor, i))
			.flatten()
			.and_then(|timestamp| timestamp.checked_sub(frames[i].timestamp).ok())
			.filter(|duration| !duration.is_zero())
			.or(fallback);
	}

	frames
}

fn pts_monotonic(frames: &[Frame], successor: Option<&Frame>) -> bool {
	let frames_monotonic = frames.windows(2).all(|pair| pair[1].timestamp >= pair[0].timestamp);
	let successor_monotonic = match (frames.last(), successor) {
		(Some(last), Some(successor)) => successor.timestamp >= last.timestamp,
		_ => true,
	};
	frames_monotonic && successor_monotonic
}

fn next_timestamp(frames: &[Frame], successor: Option<&Frame>, index: usize) -> Option<Timestamp> {
	frames
		.get(index + 1)
		.map(|next| next.timestamp)
		.or_else(|| successor.map(|next| next.timestamp))
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

#[cfg(test)]
mod tests {
	use bytes::Bytes;

	use super::*;

	fn ts(micros: u64) -> Timestamp {
		Timestamp::from_micros(micros).unwrap()
	}

	fn frame(timestamp_us: u64, duration_us: Option<u64>) -> Frame {
		Frame {
			timestamp: ts(timestamp_us),
			duration: duration_us.map(ts),
			payload: Bytes::from_static(&[0xDE, 0xAD]),
			keyframe: false,
		}
	}

	#[test]
	fn infer_missing_durations_uses_default_for_trailing_sample() {
		let frames = infer_missing_durations(
			vec![frame(0, Some(0)), frame(41_667, None), frame(83_334, None)],
			None,
			Duration::from_millis(33),
		);

		assert_eq!(frames[0].duration, Some(ts(41_667)));
		assert_eq!(frames[1].duration, Some(ts(41_667)));
		assert_eq!(frames[2].duration, Some(ts(33_000)));
		assert_eq!(fragment_seconds(&frames, Duration::from_millis(33)), 0.116334);
	}

	#[test]
	fn infer_missing_duration_uses_default_for_single_frame() {
		let frames = infer_missing_durations(vec![frame(83_333, Some(0))], None, Duration::from_millis(40));

		assert_eq!(frames[0].duration, Some(ts(40_000)));
		assert_eq!(fragment_seconds(&frames, Duration::from_millis(40)), 0.04);
	}

	#[test]
	fn infer_trailing_duration_from_successor_frame() {
		let successor = frame(83_334, None);
		let frames = infer_missing_durations(vec![frame(41_667, None)], Some(&successor), Duration::from_millis(33));

		assert_eq!(frames[0].duration, Some(ts(41_667)));
		assert_eq!(fragment_seconds(&frames, Duration::from_millis(33)), 0.041667);
	}

	#[test]
	fn infer_missing_durations_avoids_non_monotonic_pts() {
		let successor = frame(66_000, None);
		let frames = infer_missing_durations(
			vec![frame(0, None), frame(99_000, None), frame(33_000, None)],
			Some(&successor),
			Duration::from_millis(33),
		);

		assert_eq!(frames[0].duration, Some(ts(33_000)));
		assert_eq!(frames[1].duration, Some(ts(33_000)));
		assert_eq!(frames[2].duration, Some(ts(33_000)));
		assert_eq!(fragment_seconds(&frames, Duration::from_millis(33)), 0.099);
	}
}
