use std::collections::HashMap;
use std::task::Poll;
use std::time::Duration;

use bytes::Bytes;
use hang::catalog::{AudioCodec, Catalog, Container, VideoConfig};
use mp4_atom::DecodeMaybe;

use crate::Result;
use crate::catalog::Stream;
use crate::container::ExportSource;
use crate::container::Frame;
use crate::container::fmp4::Error;
use moq_net::Timestamp;

/// Subscribe to a moq broadcast and produce a single fMP4 / CMAF byte stream.
///
/// Built from a [`Source`](crate::Source), `Export` subscribes to the hang catalog,
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
/// [`next_chunk`](Self::next_chunk) returns the same bytes as a [`Chunk`], which
/// separates the init segment from a [`Fragment`] carrying whether it begins at a
/// sync sample and how long it lasts. A segmenting consumer (e.g. an HLS/LL-HLS
/// packager) needs that to map fragments onto segments and parts; narrow the catalog
/// to a single rendition with [`Stream::select`](crate::catalog::Stream::select) so the
/// fragments belong to one track.
pub struct Export<S: Stream> {
	source: crate::Source,
	catalog: Option<S>,
	max_age: Duration,
	fragment_duration: Option<Duration>,

	tracks: HashMap<String, Fmp4Track>,

	/// Most recent catalog snapshot. Used to build the init segment once every
	/// source's codec config is ready.
	catalog_snapshot: Option<Catalog>,

	/// Set after the init segment has been emitted; subsequent catalog updates only
	/// (un)subscribe tracks without re-emitting init.
	init_emitted: bool,
}

/// One emitted CMAF chunk: the init segment, then media fragments.
///
/// The two carry different metadata, so they are separate variants rather than one
/// struct with fields that are meaningless on the init. The set is closed by the
/// container format, so a `match` on it stays exhaustive.
#[derive(Clone, Debug)]
pub enum Chunk {
	/// The merged init segment (ftyp + multi-track moov), emitted once, first.
	Init(Bytes),

	/// One moof+mdat fragment.
	Fragment(Fragment),
}

impl Chunk {
	/// The encoded bytes, whichever kind of chunk this is.
	pub fn data(&self) -> &Bytes {
		match self {
			Chunk::Init(data) => data,
			Chunk::Fragment(fragment) => &fragment.data,
		}
	}

	/// Consume the chunk for its encoded bytes, dropping any metadata.
	pub fn into_data(self) -> Bytes {
		match self {
			Chunk::Init(data) => data,
			Chunk::Fragment(fragment) => fragment.data,
		}
	}

	/// The init segment's bytes, or `None` for a media fragment.
	pub fn init(self) -> Option<Bytes> {
		match self {
			Chunk::Init(data) => Some(data),
			Chunk::Fragment(_) => None,
		}
	}

	/// The media fragment, or `None` for the init segment.
	pub fn fragment(self) -> Option<Fragment> {
		match self {
			Chunk::Init(_) => None,
			Chunk::Fragment(fragment) => Some(fragment),
		}
	}
}

/// One moof+mdat fragment, with the metadata a segmenting consumer needs.
#[derive(Clone, Debug)]
pub struct Fragment {
	/// The encoded moof+mdat bytes.
	pub data: Bytes,

	/// A fragment that begins at a sync sample, so it can start a segment.
	/// Video fragments are independent only at a GOP boundary (keyframe); audio
	/// fragments are always independent.
	pub independent: bool,

	/// Presentation duration of the fragment.
	pub duration: Duration,
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

	/// True for Opus audio, whose packets carry their duration in the TOC byte.
	opus: bool,

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
	/// Subscribe to `source` and produce fMP4 byte chunks, driving track
	/// (un)subscription from `catalog`.
	///
	/// `catalog` is any [`Stream`] of catalog snapshots, typically a
	/// [`catalog::Consumer`](crate::catalog::Consumer) directly, or narrowed to
	/// one rendition set via [`Stream::select`](crate::catalog::Stream::select).
	pub fn new(source: crate::Source, catalog: S) -> Self {
		Self {
			source,
			catalog: Some(catalog),
			max_age: Duration::ZERO,
			fragment_duration: None,
			tracks: HashMap::new(),
			catalog_snapshot: None,
			init_emitted: false,
		}
	}

	/// Set the max age for each per-track source.
	///
	/// See [`Consumer`](crate::container::Consumer) for the per-track skip behavior.
	/// Defaults to
	/// [`Duration::ZERO`] (skip aggressively).
	pub fn with_max_age(mut self, max_age: Duration) -> Self {
		self.max_age = max_age;
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
		Ok(self.next_chunk().await?.map(Chunk::into_data))
	}

	/// Poll-based variant of [`Self::next`].
	pub fn poll_next(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<Bytes>>> {
		Poll::Ready(Ok(
			std::task::ready!(self.poll_next_chunk(waiter)?).map(Chunk::into_data)
		))
	}

	/// Like [`next`](Self::next) but returns a [`Chunk`], which tells the init segment
	/// apart from a [`Fragment`] and carries that fragment's segmenting metadata.
	pub async fn next_chunk(&mut self) -> Result<Option<Chunk>> {
		kio::wait(|waiter| self.poll_next_chunk(waiter)).await
	}

	/// Poll-based variant of [`Self::next_chunk`].
	pub fn poll_next_chunk(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<Chunk>>> {
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
		for (name, track) in &mut self.tracks {
			if track.pending.is_some() || track.finished {
				continue;
			}
			loop {
				match track.source.poll_read(waiter) {
					Poll::Ready(Ok(Some(frame))) => {
						let geometry_ready = !track.is_video
							|| self
								.catalog_snapshot
								.as_ref()
								.and_then(|catalog| catalog.video.renditions.get(name))
								.is_some_and(|config| {
									matches!(config.container, Container::Cmaf { .. })
										|| track.source.video_geometry_ready(config)
								});
						if waiting_for_init && (!track.source.header_ready() || !geometry_ready) {
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
				return Poll::Ready(Ok(Some(Chunk::Init(init))));
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
			// One fragment per frame: a zero cap, or the audio-only default where
			// no keyframe will ever roll the fragment. These never depend on the
			// successor, so emit immediately instead of buffering the frame until
			// the next one flushes it.
			let has_video = self.tracks.values().any(|t| t.is_video);
			let per_frame = frag == Some(Duration::ZERO) || (frag.is_none() && !has_video);
			let track = self.tracks.get_mut(&name).unwrap();
			let frame = track.pending.take().unwrap();
			if per_frame {
				// A catalog change can leave buffered frames behind. Drain them
				// first and retry this frame on the next poll.
				if !track.buffer.is_empty() {
					let frames = std::mem::take(&mut track.buffer);
					let fragment = emit_fragment(track, frames, Some(&frame))?;
					track.pending = Some(frame);
					return Poll::Ready(Ok(Some(Chunk::Fragment(fragment))));
				}
				track.buffer_independent = frame.keyframe;
				let fragment = emit_fragment(track, vec![frame], None)?;
				return Poll::Ready(Ok(Some(Chunk::Fragment(fragment))));
			}
			if should_flush(track, &frame, frag) {
				let frames = std::mem::take(&mut track.buffer);
				let fragment = emit_fragment(track, frames, Some(&frame))?;
				// The flushed run is done; the incoming frame opens the next buffer.
				track.buffer_independent = frame.keyframe;
				track.buffer.push(frame);
				return Poll::Ready(Ok(Some(Chunk::Fragment(fragment))));
			}
			if track.buffer.is_empty() {
				track.buffer_independent = frame.keyframe;
			}
			track.buffer.push(frame);
			// Frame appended to buffer; loop again to look for more work or a flush.
			return self.poll_next_chunk(waiter);
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
			return Poll::Ready(Ok(Some(Chunk::Fragment(fragment))));
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
		// A rendition we can't parse is ignored rather than failing the whole export. Drop it
		// before the snapshot is cached, since the init segment expects a track for every
		// rendition in it. (An escaping `broadcast` reference is already gone: the catalog
		// stream drops those.)
		let mut catalog = catalog.clone();
		catalog
			.video
			.renditions
			.retain(|name, config| crate::catalog::hang::supported(name, &config.container));
		catalog
			.audio
			.renditions
			.retain(|name, config| crate::catalog::hang::supported(name, &config.container));
		self.source.retain_valid_media(&mut catalog);
		let catalog = &catalog;

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
			let Some(source) = ExportSource::for_video(&self.source, name, config, self.max_age)? else {
				continue;
			};
			let timescale = catalog_timescale_video(config)?;
			let framerate = super::usable_video_framerate(config).unwrap_or(30.0);
			self.tracks.insert(
				name.clone(),
				Fmp4Track {
					source,
					pending: None,
					buffer: Vec::new(),
					buffer_independent: false,
					is_video: true,
					opus: false,
					default_frame: Duration::from_secs_f64(1.0 / framerate),
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
			let Some(source) = ExportSource::for_audio(&self.source, name, config, self.max_age)? else {
				continue;
			};
			let timescale = catalog_timescale_audio(config)?;
			self.tracks.insert(
				name.clone(),
				Fmp4Track {
					source,
					pending: None,
					buffer: Vec::new(),
					buffer_independent: false,
					is_video: false,
					opus: matches!(config.codec, AudioCodec::Opus),
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
		let Some(catalog) = self.catalog_snapshot.as_ref() else {
			return false;
		};
		self.tracks.values().all(|t| t.source.header_ready())
			&& catalog.video.renditions.iter().all(|(name, config)| {
				matches!(config.container, Container::Cmaf { .. })
					|| self
						.tracks
						.get(name)
						.is_some_and(|track| track.source.video_geometry_ready(config))
			})
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
					extract_init(init, track.track_id, &mut ftyp_data, &mut traks, &mut trexs)?;
				}
				Container::Legacy | Container::Loc => {
					// H.264/H.265 need a synthesized config record here; VP8 has none.
					let description = track.source.description();
					let config = track.source.video_config(config).unwrap_or_else(|| config.clone());
					let trak = crate::container::fmp4::synthesize_video_trak(
						track.track_id,
						track.timescale,
						&config,
						description.map(|d| d.as_ref()),
					)?;
					trexs.push(mp4_atom::Trex {
						track_id: trak.tkhd.track_id,
						default_sample_description_index: 1,
						..Default::default()
					});
					traks.push(trak);
				}
				Container::Unknown(unknown) => return Err(crate::Error::unsupported_container(unknown)),
			}
		}

		for (name, config) in &catalog.audio.renditions {
			let track = self
				.tracks
				.get(name)
				.ok_or_else(|| Error::MissingAudioTrack(name.clone()))?;
			match &config.container {
				Container::Cmaf { init, .. } => {
					extract_init(init, track.track_id, &mut ftyp_data, &mut traks, &mut trexs)?;
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
				Container::Unknown(unknown) => return Err(crate::Error::unsupported_container(unknown)),
			}
		}

		Ok(crate::container::fmp4::encode_init(ftyp_data, traks, trexs)?)
	}
}

/// Pull ftyp + moov from a single-track CMAF init segment and merge into the
/// caller's accumulators, rewriting the track id to `track_id`.
///
/// The source init carries whatever id the track had in ITS broadcast (e.g. an
/// audio track that was second in the source is `2`), but our fragments are always
/// re-encoded with the exporter's own `track.track_id` (see [`encode_fragment`]) --
/// nothing is passed through. So the moov MUST adopt that same id, or a player
/// loads an init declaring track N and then a fragment claiming a different track
/// and rejects it ("no tfhd for track"). It also keeps a merged multi-track moov
/// from colliding when two single-track source inits both used id `1`.
pub(crate) fn extract_init(
	init: &Bytes,
	track_id: u32,
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
				for mut trak in moov.trak {
					trak.tkhd.track_id = track_id;
					// Drop the source edit list. CMAF carries timing via tfdt +
					// composition offsets, so an edit list is redundant here, and a
					// browser applying an empty-edit media_time would shift the track
					// off the others (a black screen in Media Source Extensions).
					trak.edts = None;
					// tkhd.duration is in the *movie* timescale, and the merged moov
					// picks its own (the first trak's media scale), so whatever the
					// source stated is now read at a scale it wasn't written in. A
					// merged multi-track moov can't hold one scale that suits every
					// source anyway, so declare it unknown like the rest of the init.
					trak.tkhd.duration = super::UNKNOWN_DURATION;
					traks.push(trak);
				}
				if let Some(mvex) = moov.mvex {
					for mut trex in mvex.trex {
						trex.track_id = track_id;
						trexs.push(trex);
					}
				}
			}
			_ => {}
		}
	}
	Ok(())
}

/// Should we flush `track.buffer` before appending the incoming `frame`?
/// Triggers on a video keyframe (one fragment per GOP) or the duration cap.
/// Per-frame modes never buffer and are handled before this check.
fn should_flush(track: &Fmp4Track, frame: &Frame, fragment_duration: Option<Duration>) -> bool {
	if track.buffer.is_empty() {
		return false;
	}
	if track.is_video && frame.keyframe {
		return true;
	}
	let Some(cap) = fragment_duration else {
		return false;
	};
	// Frames within a track are in *decode* order; B-frames have non-monotonic
	// PTS, so the span of the buffer is min..max of all PTS.
	let mut min = Duration::from(frame.timestamp);
	let mut max = min;
	for f in &track.buffer {
		let pts = Duration::from(f.timestamp);
		min = min.min(pts);
		max = max.max(pts);
	}
	max.saturating_sub(min) >= cap
}

/// Encode a buffered run of samples as a single CMAF moof+mdat fragment.
fn encode_fragment(track: &mut Fmp4Track, frames: Vec<Frame>) -> Result<Bytes> {
	if frames.is_empty() {
		return Err(Error::NoFrames.into());
	}
	let seq = track.sequence_number;
	track.sequence_number += 1;
	let timescale = moq_net::Timescale::new(track.timescale)?;
	let info = crate::container::fmp4::FragmentInfo {
		track_id: track.track_id,
		timescale,
		sequence_number: seq,
	};
	Ok(crate::container::fmp4::encode_fragment(info, &frames)?)
}

/// Encode a buffered run and wrap it with the metadata a segmenting consumer needs.
fn emit_fragment(track: &mut Fmp4Track, mut frames: Vec<Frame>, successor: Option<&Frame>) -> Result<Fragment> {
	apply_codec_durations(&mut frames, track.opus);
	// Audio has no keyframes, so every audio fragment is independent; video is
	// independent only when its buffer opened on a keyframe (a GOP boundary).
	let independent = !track.is_video || track.buffer_independent;
	let timescale = moq_net::Timescale::new(track.timescale)?;
	infer_missing_durations(&mut frames, successor, track.default_frame, timescale)?;
	let duration = fragment_duration(&frames, track.default_frame);
	let data = encode_fragment(track, frames)?;
	Ok(Fragment {
		data,
		independent,
		duration,
	})
}

/// Presentation duration of a fragment.
///
/// When every sample carries a duration (the CMAF case) the per-sample durations
/// tile the timeline, so their sum is exact. Legacy / LOC sources carry none, so
/// fall back to the presentation span plus one `default_frame` for the trailing
/// sample (which has no successor to bound it).
fn fragment_duration(frames: &[Frame], default_frame: Duration) -> Duration {
	if frames.is_empty() {
		return Duration::ZERO;
	}
	if frames
		.iter()
		.all(|f| f.duration.is_some_and(|duration| !duration.is_zero()))
	{
		return frames
			.iter()
			.map(|f| Duration::from(f.duration.unwrap()))
			.sum::<Duration>();
	}
	let mut min = Duration::MAX;
	let mut max = Duration::ZERO;
	for f in frames {
		let pts = Duration::from(f.timestamp);
		min = min.min(pts);
		max = max.max(pts);
	}
	(max - min) + default_frame
}

/// Fill in the durations the codec states outright, before anything has to be inferred
/// from a neighbouring frame.
///
/// Opus packets carry their duration in the TOC byte (at 48 kHz), so an Opus track never
/// has to look at its neighbours at all -- which matters most at a group boundary, where
/// the neighbour is off limits (see [`infer_missing_durations`]) and the fallback would
/// otherwise mis-time every packet of a one-packet-per-group audio track.
pub(crate) fn apply_codec_durations(frames: &mut [Frame], opus: bool) {
	if !opus {
		return;
	}
	for frame in frames {
		if frame.duration.is_none() {
			frame.duration = crate::codec::opus::packet_samples(&frame.payload)
				.and_then(|samples| Timestamp::from_scale(samples as u64, 48_000).ok());
		}
	}
}

/// Fill in the duration of every remaining frame from the timestamp of the frame that
/// follows it, as long as that frame is in the same group.
///
/// The gap between two frames is only a duration while they sit on one continuous
/// timeline, and a group boundary is where that stops holding. Groups are independently
/// decodable, so a publisher may pause and resume across one (moq-boy keeps its PTS on a
/// clock that runs through the pause), a subscriber may join mid-stream or skip to a
/// newer group, and a fetch may span a hole. Consecutive sequence numbers do not rule any
/// of that out, so the gap across a boundary is treated as a discontinuity, never a
/// duration. Reading it as one makes a single sample last as long as the gap: a recording
/// that woke a paused publisher got a 2405 second video sample, whose `EXTINF` put the HLS
/// video timeline 9620 seconds ahead of audio and stalled the player outright
/// (moq-dev/moq.pro#814).
///
/// The last frame before a boundary falls back to `default_frame` (the catalog framerate /
/// sample rate), which is what the gap works out to anyway on a continuous constant-rate
/// source -- the two answers only diverge when there IS a gap.
///
/// The publisher is the one that actually knows where its group's content ends, and
/// [`Producer::cut`](crate::container::Producer::cut) is how it says so: the durations it
/// writes arrive already set and are left alone here.
pub(crate) fn infer_missing_durations(
	frames: &mut [Frame],
	successor: Option<&Frame>,
	default_frame: Duration,
	timescale: moq_net::Timescale,
) -> Result<()> {
	let infer_from_pts = pts_monotonic(frames, successor);

	for i in 0..frames.len() {
		let successor = infer_from_pts
			.then(|| duration_bound(frames, successor, i))
			.flatten()
			.cloned();
		infer_missing_duration(&mut frames[i], successor.as_ref(), default_frame, timescale)?;
	}

	Ok(())
}

/// Infer one frame's duration from a known same-group successor, else the catalog cadence.
pub(crate) fn infer_missing_duration(
	frame: &mut Frame,
	successor: Option<&Frame>,
	default_frame: Duration,
	timescale: moq_net::Timescale,
) -> Result<()> {
	if frame.duration.is_some_and(|duration| !duration.is_zero()) {
		return Ok(());
	}

	frame.duration = match successor {
		Some(next) => timestamp_gap(frame.timestamp, next.timestamp, timescale)?,
		None => None,
	}
	.or_else(|| fallback_duration(default_frame, timescale))
	.filter(|duration| !duration.is_zero());
	Ok(())
}

/// Snap floating-point catalog cadence to an output tick only when the mismatch is no more
/// than the nanosecond precision used by `Duration`.
fn fallback_duration(default_frame: Duration, timescale: moq_net::Timescale) -> Option<Timestamp> {
	if let Some(ticks) = super::rounded_duration_ticks(default_frame, timescale.as_u64()) {
		Timestamp::new(ticks, timescale).ok()
	} else {
		Timestamp::try_from(default_frame).ok()
	}
}

/// Quantize two absolute instants at the output scale, then subtract their tick positions.
///
/// Quantizing endpoints instead of each gap independently carries fractional ticks forward:
/// a microsecond clock alternating 33,333 and 33,334 microsecond steps at 30 kHz produces
/// consecutive 1,000-tick samples instead of either drifting or failing as inexact.
fn timestamp_gap(start: Timestamp, end: Timestamp, timescale: moq_net::Timescale) -> Result<Option<Timestamp>> {
	let start_scale = u128::from(start.scale().as_u64());
	let end_scale = u128::from(end.scale().as_u64());
	let end_numerator = u128::from(end.value()) * start_scale;
	let start_numerator = u128::from(start.value()) * end_scale;
	let Some(numerator) = end_numerator.checked_sub(start_numerator) else {
		return Ok(None);
	};
	if numerator == 0 {
		return Ok(None);
	}

	let start_ticks = super::timestamp_ticks(start, timescale)?;
	let end_ticks = super::timestamp_ticks(end, timescale)?;
	let Some(ticks) = end_ticks.checked_sub(start_ticks) else {
		return Ok(None);
	};
	if ticks == 0 {
		return Err(Error::SampleDurationTooSmall(timescale.as_u64()).into());
	}

	Ok(Some(Timestamp::new(ticks, timescale)?))
}

fn pts_monotonic(frames: &[Frame], successor: Option<&Frame>) -> bool {
	let frames_monotonic = frames.windows(2).all(|pair| pair[1].timestamp >= pair[0].timestamp);
	// Only a successor we'd actually read from gets a say: one in the next group bounds
	// nothing here, so a rewind across that boundary must not veto inference either.
	let successor_monotonic = match (frames.last(), successor.filter(|next| !next.keyframe)) {
		(Some(last), Some(successor)) => successor.timestamp >= last.timestamp,
		_ => true,
	};
	frames_monotonic && successor_monotonic
}

/// The frame that bounds `frames[index]`'s duration, or `None` when the only candidate is
/// in the next group.
///
/// Every group starts with a keyframe -- the wire may carry the flag, and
/// [`Consumer`](crate::container::Consumer) asserts it on each group's first frame
/// regardless -- so a keyframe is where a group boundary can be. Video only ever meets one
/// as the successor (a keyframe flushes the fragment before it can be appended), but audio
/// has no keyframe roll, so a boundary can also fall interior to `frames`; both land here.
fn duration_bound<'a>(frames: &'a [Frame], successor: Option<&'a Frame>, index: usize) -> Option<&'a Frame> {
	frames.get(index + 1).or(successor).filter(|next| !next.keyframe)
}

pub(crate) fn catalog_timescale_video(config: &VideoConfig) -> Result<u64> {
	Ok(match &config.container {
		Container::Cmaf { init, .. } => {
			parse_timescale_from_init(init).unwrap_or_else(|_| crate::container::fmp4::default_video_timescale(config))
		}
		Container::Loc | Container::Legacy => crate::container::fmp4::default_video_timescale(config),
		Container::Unknown(unknown) => return Err(crate::Error::unsupported_container(unknown)),
	})
}

pub(crate) fn catalog_timescale_audio(config: &hang::catalog::AudioConfig) -> Result<u64> {
	Ok(match &config.container {
		Container::Cmaf { init, .. } => parse_timescale_from_init(init).unwrap_or(config.sample_rate as u64),
		Container::Loc | Container::Legacy => config.sample_rate as u64,
		Container::Unknown(unknown) => return Err(crate::Error::unsupported_container(unknown)),
	})
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

	fn duration_micros(frame: &Frame) -> u128 {
		frame.duration.unwrap().as_micros()
	}

	fn frame(timestamp_us: u64, duration_us: Option<u64>) -> Frame {
		Frame {
			timestamp: ts(timestamp_us),
			duration: duration_us.map(ts),
			payload: Bytes::from_static(&[0xDE, 0xAD]),
			keyframe: false,
		}
	}

	/// A frame that opens a group. Every group starts with a keyframe, so this is what
	/// the far side of a group boundary looks like to `infer_missing_durations`.
	fn group_start(timestamp_us: u64) -> Frame {
		Frame {
			keyframe: true,
			..frame(timestamp_us, None)
		}
	}

	#[test]
	fn infer_missing_durations_uses_default_for_trailing_sample() {
		let mut frames = vec![frame(0, Some(0)), frame(41_667, None), frame(83_334, None)];
		infer_missing_durations(&mut frames, None, Duration::from_millis(33), moq_net::Timescale::MICRO).unwrap();

		assert_eq!(frames[0].duration, Some(ts(41_667)));
		assert_eq!(frames[1].duration, Some(ts(41_667)));
		assert_eq!(frames[2].duration, Some(ts(33_000)));
		assert_eq!(
			fragment_duration(&frames, Duration::from_millis(33)),
			Duration::from_micros(116_334)
		);
	}

	#[test]
	fn infer_missing_duration_uses_default_for_single_frame() {
		let mut frames = vec![frame(83_333, Some(0))];
		infer_missing_durations(&mut frames, None, Duration::from_millis(40), moq_net::Timescale::MICRO).unwrap();

		assert_eq!(frames[0].duration, Some(ts(40_000)));
		assert_eq!(
			fragment_duration(&frames, Duration::from_millis(40)),
			Duration::from_millis(40)
		);
	}

	#[test]
	fn infer_trailing_duration_from_successor_frame() {
		let successor = frame(83_334, None);
		let mut frames = vec![frame(41_667, None)];
		infer_missing_durations(
			&mut frames,
			Some(&successor),
			Duration::from_millis(33),
			moq_net::Timescale::MICRO,
		)
		.unwrap();

		assert_eq!(frames[0].duration, Some(ts(41_667)));
		assert_eq!(
			fragment_duration(&frames, Duration::from_millis(33)),
			Duration::from_micros(41_667)
		);
	}

	#[test]
	fn inferred_microsecond_clock_carries_fractional_ticks_forward() {
		let timescale = moq_net::Timescale::new(30_000).unwrap();
		let mut frames = vec![frame(0, None), frame(33_333, None), frame(66_667, None)];
		infer_missing_durations(&mut frames, None, Duration::from_nanos(33_333_333), timescale).unwrap();

		let durations: Vec<_> = frames
			.iter()
			.map(|frame| {
				let duration = frame.duration.unwrap();
				(duration.value(), duration.scale())
			})
			.collect();
		assert_eq!(durations, vec![(1_000, timescale); 3]);
	}

	/// The regression for moq-dev/moq.pro#814: a subscriber that got a stale cached group
	/// and then jumped to live sees a huge gap across the boundary. That gap is a
	/// discontinuity, not a duration, so the frame before it must NOT swallow it -- it
	/// takes `default_frame` and the fragment stays one frame long.
	#[test]
	fn infer_stops_at_a_group_boundary() {
		let next_group = group_start(2_405_070_000);
		let mut frames = vec![frame(63_244, None)];
		infer_missing_durations(
			&mut frames,
			Some(&next_group),
			Duration::from_millis(33),
			moq_net::Timescale::MICRO,
		)
		.unwrap();

		assert_eq!(frames[0].duration, Some(ts(33_000)));
		assert_eq!(
			fragment_duration(&frames, Duration::from_millis(33)),
			Duration::from_millis(33)
		);
	}

	/// Audio never rolls a fragment on a keyframe, so its buffer can span whole groups.
	/// The boundary is then interior to `frames`, and bounds the frame before it just
	/// the same.
	#[test]
	fn infer_stops_at_an_interior_group_boundary() {
		let mut frames = vec![frame(0, None), frame(21_333, None), group_start(600_000_000)];
		infer_missing_durations(&mut frames, None, Duration::from_millis(21), moq_net::Timescale::MICRO).unwrap();

		assert_eq!(frames[0].duration, Some(ts(21_333)), "same group, real delta");
		assert_eq!(duration_micros(&frames[1]), 21_000, "bounded by the next group");
		assert_eq!(duration_micros(&frames[2]), 21_000, "nothing after it at all");
	}

	/// The duration cap splits a GOP across fragments, so the successor is a delta frame
	/// in the SAME group. That one is a real bound and is still used.
	#[test]
	fn infer_crosses_a_mid_group_fragment_boundary() {
		let successor = frame(83_334, None);
		let mut frames = vec![frame(41_667, None)];
		infer_missing_durations(
			&mut frames,
			Some(&successor),
			Duration::from_millis(33),
			moq_net::Timescale::MICRO,
		)
		.unwrap();

		assert_eq!(frames[0].duration, Some(ts(41_667)));
	}

	/// The HLS fetch origin concatenates a segment's groups, and for audio those are often
	/// one packet each -- so EVERY packet sits at a boundary and none of them may be timed
	/// by the next. Opus states its own duration, which is what keeps that exact instead of
	/// dropping each packet onto the ~21.3 ms `1024/sample_rate` fallback.
	#[test]
	fn codec_durations_keep_one_packet_groups_exact() {
		// 20 ms of 48 kHz stereo Opus: TOC config 15 (SILK/WB 20 ms), one frame.
		let packet = Bytes::from_static(&[0x78, 0x00, 0x00, 0x00]);
		let mut frames: Vec<Frame> = (0..3)
			.map(|i| Frame {
				payload: packet.clone(),
				..group_start(i * 20_000)
			})
			.collect();

		apply_codec_durations(&mut frames, true);
		infer_missing_durations(
			&mut frames,
			None,
			Duration::from_micros(21_333),
			moq_net::Timescale::MICRO,
		)
		.unwrap();

		for f in &frames {
			assert_eq!(
				f.duration.unwrap().as_micros(),
				20_000,
				"TOC duration, not the fallback"
			);
		}
	}

	/// A rewind across a group boundary bounds nothing here, so it must not veto
	/// inference for the frames that do have an in-group successor.
	#[test]
	fn infer_ignores_a_rewound_group_boundary() {
		let next_group = group_start(0);
		let mut frames = vec![frame(1_000_000, None), frame(1_033_000, None)];
		infer_missing_durations(
			&mut frames,
			Some(&next_group),
			Duration::from_millis(50),
			moq_net::Timescale::MICRO,
		)
		.unwrap();

		assert_eq!(
			frames[0].duration,
			Some(ts(33_000)),
			"in-group delta survives the rewind"
		);
		assert_eq!(duration_micros(&frames[1]), 50_000, "last in group falls back");
	}

	#[test]
	fn infer_missing_durations_avoids_non_monotonic_pts() {
		let successor = frame(66_000, None);
		let mut frames = vec![frame(0, None), frame(99_000, None), frame(33_000, None)];
		infer_missing_durations(
			&mut frames,
			Some(&successor),
			Duration::from_millis(33),
			moq_net::Timescale::MICRO,
		)
		.unwrap();

		assert_eq!(frames[0].duration, Some(ts(33_000)));
		assert_eq!(frames[1].duration, Some(ts(33_000)));
		assert_eq!(frames[2].duration, Some(ts(33_000)));
		assert_eq!(
			fragment_duration(&frames, Duration::from_millis(33)),
			Duration::from_micros(99_000)
		);
	}

	// A source init whose trak carries an edit list must come out of extract_init with
	// the edit list dropped: CMAF carries timing via tfdt + composition offsets, and a
	// browser applying an edit list shifts the track off the others (a black screen).
	#[test]
	fn extract_init_strips_edit_lists() {
		use mp4_atom::Encode;

		let trak = mp4_atom::Trak {
			edts: Some(mp4_atom::Edts {
				elst: Some(mp4_atom::Elst {
					entries: vec![mp4_atom::ElstEntry::default()],
				}),
			}),
			..Default::default()
		};
		let moov = mp4_atom::Moov {
			trak: vec![trak],
			..Default::default()
		};
		let mut init = Vec::new();
		moov.encode(&mut init).unwrap();

		let mut traks = Vec::new();
		let mut trexs = Vec::new();
		let mut ftyp = None;
		extract_init(&Bytes::from(init), 1, &mut ftyp, &mut traks, &mut trexs).unwrap();

		assert_eq!(traks.len(), 1);
		assert!(traks[0].edts.is_none(), "CMAF init must not carry an edit list");
	}

	// tkhd.duration is in the movie timescale, which the merged moov replaces with its own. A
	// duration carried over from the source is then read at a scale it wasn't written in: a 30
	// second track authored at a 1 kHz movie scale reads as 0.625s once the moov says 48 kHz.
	#[test]
	fn extract_init_clears_a_stale_track_duration() {
		use mp4_atom::Encode;

		let moov = mp4_atom::Moov {
			mvhd: mp4_atom::Mvhd {
				timescale: 1_000,
				..Default::default()
			},
			trak: vec![mp4_atom::Trak {
				tkhd: mp4_atom::Tkhd {
					duration: 30_000, // 30s at the source's 1 kHz movie scale
					..Default::default()
				},
				mdia: mp4_atom::Mdia {
					mdhd: mp4_atom::Mdhd {
						timescale: 48_000,
						..Default::default()
					},
					..Default::default()
				},
				..Default::default()
			}],
			..Default::default()
		};
		let mut init = Vec::new();
		moov.encode(&mut init).unwrap();

		let mut traks = Vec::new();
		let mut trexs = Vec::new();
		let mut ftyp = None;
		extract_init(&Bytes::from(init), 1, &mut ftyp, &mut traks, &mut trexs).unwrap();

		assert_eq!(traks.len(), 1);
		assert_eq!(
			traks[0].tkhd.duration,
			u64::MAX,
			"a live fragmented init declares an unknown duration"
		);
	}
}
