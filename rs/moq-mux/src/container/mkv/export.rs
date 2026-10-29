use std::collections::HashMap;
use std::io::Cursor;
use std::task::Poll;
use std::time::Duration;

use bytes::{BufMut, Bytes, BytesMut};
use hang::catalog::{AudioCodec, AudioConfig, Catalog, Container, VideoCodec, VideoConfig};
use webm_iterable::matroska_spec::{Master, MatroskaSpec};
use webm_iterable::{WebmWriter, WriteOptions};

use crate::Result;
use crate::catalog::Stream;
use crate::container::ExportSource;
use crate::container::Frame;
use crate::container::consumer::Event;
use crate::container::decode::{DecodeClock, Settle};
use crate::container::mkv::Error;

/// Matroska TimestampScale: 1 ms (in nanoseconds).
const TIMESTAMP_SCALE_NS: u64 = 1_000_000;

/// Subscribe to a moq broadcast and produce a single Matroska / WebM byte stream.
///
/// Built from a [`Source`](crate::Source), `Export` subscribes to the hang catalog,
/// (un)subscribes per-rendition tracks, decodes them via a per-track source, and
/// re-encodes everything as EBML + Segment + Tracks + Cluster/SimpleBlock tags ready
/// for any Matroska-aware consumer (ffplay, libwebm, browser MSE for WebM).
///
/// Use [`next`](Self::next) to pull byte chunks. The first chunk is the file
/// header (EBML + unknown-size Segment + Info + Tracks); subsequent chunks are
/// complete Cluster elements. By default a Cluster contains one GOP (rolled
/// over on each video keyframe, or on i16 timestamp overflow);
/// [`with_fragment_duration`](Self::with_fragment_duration) caps Cluster
/// duration for downstream consumers that throttle by fragment rate.
/// Returns `None` when the broadcast ends.
///
/// ## Avc3 / Hev1 sources
///
/// `Export` accepts Annex-B sources (`H264 { inline: true }`, `H265 { in_band: true }`,
/// catalog `description` empty) by attaching a [`crate::codec::h264::Avc1`] /
/// [`crate::codec::h265::Hvc1`] to each affected track. The transform caches
/// parameter sets, builds the out-of-band `AVCDecoderConfigurationRecord` /
/// `HEVCDecoderConfigurationRecord`, and length-prefixes sample NALs. Header
/// emission is deferred until every such track has produced its codec config
/// (typically the first keyframe).
///
/// Only Legacy-container tracks (raw codec payloads) are supported. CMAF tracks
/// (moof+mdat passthrough) are rejected with a clear error.
///
/// A rendition the catalog moves to another track (a new `track` or `broadcast`) goes on
/// under its track number from the new track's first sync frame past what was read, when
/// the header's TrackEntry still describes it; otherwise, or when it goes back in time, the
/// export fails.
pub struct Export<S: Stream> {
	source: crate::Source,
	catalog: Option<S>,
	delay: Duration,
	jitter: crate::jitter::Buffer<String, Frame>,
	/// An already released frame waiting for the previous cluster to be returned.
	held: Option<(String, Frame)>,
	fragment_duration: Option<Duration>,

	tracks: HashMap<String, MkvTrack>,
	/// Catalog snapshot used to build the header. Retained until header emission;
	/// subsequent catalog updates only (un)subscribe tracks.
	catalog_snapshot: Option<Catalog>,

	/// Whether the file header has been emitted.
	header_emitted: bool,

	/// Currently-open cluster, accumulating frames until it's time to flush.
	cluster: Option<ClusterBuilder>,
}

struct MkvTrack {
	/// The inferred decode clock of a rendition that declares no `reorder`.
	clock: DecodeClock<crate::jitter::Arrival<Frame>>,
	/// The current group's clock, when its rendition declared `reorder` at the group's start.
	declared: Option<crate::container::declared::DeclaredClock<crate::jitter::Arrival<Frame>>>,
	config: Option<VideoConfig>,
	restart: u64,
	reorder: Option<(u64, u64)>,
	source: ExportSource,
	pending: Option<crate::jitter::Arrival<Frame>>,
	// When the source was subscribed: nothing it delivers reached the export before then.
	subscribed: moq_clock::Instant,
	// The latest arrival read from the source, which settles the frames held for their DTS.
	latest: moq_clock::Instant,
	finished: bool,
	track_number: u64,
	kind: TrackKind,
	/// The TrackEntry the header declared.
	entry: Option<MatroskaSpec>,
	/// The source replaced the one the header was built from, so its frames wait until it
	/// can be checked against `entry`.
	moved: bool,
}

impl MkvTrack {
	/// A track numbered `track_number` reading `source`, with the video `config` it was added under.
	fn new(source: ExportSource, kind: TrackKind, config: Option<VideoConfig>, track_number: u64) -> Self {
		Self {
			clock: Default::default(),
			declared: None,
			config,
			restart: 0,
			reorder: None,
			source,
			pending: None,
			subscribed: moq_clock::now(),
			latest: moq_clock::Instant::ZERO,
			finished: false,
			track_number,
			kind,
			entry: None,
			moved: false,
		}
	}

	/// Read `source`, the track the catalog moved this rendition to, after releasing the
	/// frames the old one left held for their DTS.
	fn rebind(
		&mut self,
		name: &str,
		source: ExportSource,
		jitter: &mut crate::jitter::Buffer<String, Frame>,
	) -> Result<()> {
		self.release(name, jitter, true)?;
		self.clock = Default::default();
		self.declared = None;
		self.reorder = None;
		self.source = source.continuing(&self.source);
		self.finished = false;
		self.subscribed = moq_clock::now();
		self.moved = true;
		Ok(())
	}

	/// The TrackEntry declaring this track under its catalog entry in `catalog`.
	fn track_entry(&self, catalog: &Catalog, name: &str) -> Result<MatroskaSpec> {
		match self.kind {
			TrackKind::Video => {
				let config = catalog
					.video
					.renditions
					.get(name)
					.ok_or_else(|| Error::MissingVideoTrack(name.to_string()))?;
				let config = self.source.video_config(config).unwrap_or_else(|| config.clone());
				build_video_track_entry(self.track_number, &config, self.source.description())
			}
			TrackKind::Audio => {
				let config = catalog
					.audio
					.renditions
					.get(name)
					.ok_or_else(|| Error::MissingAudioTrack(name.to_string()))?;
				build_audio_track_entry(self.track_number, config)
			}
		}
	}

	/// Start a group: release the last one's frames, then clock this one at the depth its
	/// rendition declares now, or on the inferred clock when it declares none.
	fn start_group(&mut self, name: &str, jitter: &mut crate::jitter::Buffer<String, Frame>) -> Result<()> {
		let depth = match &self.config {
			Some(config) => crate::container::declared::depth(config)?,
			None => None,
		};
		// The inferred clock spans groups, so it is only flushed when a declaration replaces it.
		if self.declared.is_some() || depth.is_some() {
			self.release(name, jitter, true)?;
		}
		self.declared = depth.map(crate::container::declared::DeclaredClock::new);
		Ok(())
	}

	/// Hold a video frame on its clock, then release what it settles.
	fn push(
		&mut self,
		name: &str,
		arrival: crate::jitter::Arrival<Frame>,
		jitter: &mut crate::jitter::Buffer<String, Frame>,
	) -> Result<()> {
		let pts = (arrival.item.timestamp.as_nanos() * 90_000 / 1_000_000_000) as u64;
		match &mut self.declared {
			Some(clock) => clock.push(arrival, pts)?,
			None => {
				let arrived = arrival.arrived;
				self.clock.push(arrival, pts, arrived)
			}
		}
		self.release(name, jitter, false)
	}

	fn release(&mut self, name: &str, jitter: &mut crate::jitter::Buffer<String, Frame>, flush: bool) -> Result<()> {
		if let Some(clock) = &mut self.declared {
			while let Some((mut arrival, dts)) = clock.pop(flush)? {
				if !self.finished && !flush {
					arrival.arrived = arrival.arrived.max(self.latest);
				}
				arrival.decode = crate::jitter::Decode::from_ticks(dts);
				jitter.push(name.to_owned(), arrival)?;
			}
		}
		let Some(config) = &self.config else { return Ok(()) };
		let (floor, lookahead) = *self.reorder.get_or_insert_with(|| {
			let declared = self.source.description().and_then(|description| match config.codec {
				VideoCodec::H264(_) => {
					crate::codec::h264::sps_reorder(crate::codec::h264::Avcc::parse(description).ok()?.sps.first()?)
				}
				VideoCodec::H265(_) => {
					crate::codec::h265::sps_reorder(crate::codec::h265::Hvcc::parse(description).ok()?.sps.first()?)
				}
				_ => None,
			});
			let (floor, lookahead) = declared.map_or((0, 0), |reorder| reorder.timing(config.framerate));
			let limit = crate::container::decode::MAX_REORDER;
			let lookahead = lookahead
				.max(
					config
						.jitter
						.map_or(0, |jitter| (jitter.as_nanos() * 90_000 / 1_000_000_000) as u64),
				)
				.min(limit);
			(floor.min(limit), lookahead)
		});
		loop {
			let settle = match flush {
				true => Settle::All,
				false => self.clock.settle(lookahead, self.latest, jitter.delay()),
			};
			let Some((mut arrival, dts)) = self.clock.pop(lookahead, floor, settle) else {
				break;
			};
			if !self.finished && !flush {
				arrival.arrived = arrival.arrived.max(self.latest);
			}
			arrival.decode = moq_net::Timestamp::from_scale(dts, 90_000)?.into();
			jitter.push(name.to_owned(), arrival)?;
		}
		Ok(())
	}
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TrackKind {
	Video,
	Audio,
}

struct ClusterBuilder {
	/// Cluster.Timestamp in ticks (ms).
	start_ticks: u64,
	body: BytesMut,
	/// Highest frame timestamp seen in this cluster.
	max_ticks: u64,
	/// Whether the cluster has at least one video frame appended. Audio-only
	/// content shouldn't trigger a GOP-boundary rollover on the first video
	/// keyframe (audio that arrived ahead of the keyframe gets folded into
	/// this cluster instead of a stale one).
	has_video: bool,
}

impl ClusterBuilder {
	fn new(start_ticks: u64) -> Self {
		let mut body = BytesMut::with_capacity(64 * 1024);
		// Cluster.Timestamp (id = 0xE7).
		write_tag_id(&mut body, ID_TIMESTAMP as u32);
		let ts_bytes = encode_uint(start_ticks);
		write_vint(&mut body, ts_bytes.len() as u64);
		body.extend_from_slice(&ts_bytes);
		Self {
			start_ticks,
			body,
			max_ticks: start_ticks,
			has_video: false,
		}
	}

	fn append(
		&mut self,
		track_number: u64,
		frame_ticks: u64,
		keyframe: bool,
		payload: &[u8],
		is_video: bool,
	) -> Result<()> {
		let rel = (frame_ticks as i64)
			.checked_sub(self.start_ticks as i64)
			.ok_or(Error::ClusterUnderflow)?;
		let rel: i16 = rel.try_into().map_err(|_| Error::BlockTimestampOverflow)?;

		let sb_body = encode_simple_block_body(track_number, rel, keyframe, payload);
		write_tag_id(&mut self.body, ID_SIMPLEBLOCK as u32);
		write_vint(&mut self.body, sb_body.len() as u64);
		self.body.extend_from_slice(&sb_body);

		if frame_ticks > self.max_ticks {
			self.max_ticks = frame_ticks;
		}
		if is_video {
			self.has_video = true;
		}
		Ok(())
	}

	/// Returns true if a frame at the given timestamp can still fit in this cluster
	/// without overflowing the i16 block-relative-timestamp field.
	fn fits(&self, frame_ticks: u64) -> bool {
		match (frame_ticks as i64).checked_sub(self.start_ticks as i64) {
			Some(rel) => i16::try_from(rel).is_ok(),
			None => false,
		}
	}

	/// Build the full Cluster element bytes.
	fn finish(self) -> Bytes {
		let mut out = BytesMut::with_capacity(self.body.len() + 16);
		write_tag_id(&mut out, ID_CLUSTER);
		write_vint(&mut out, self.body.len() as u64);
		out.extend_from_slice(&self.body);
		out.freeze()
	}
}

impl<S: Stream> Export<S> {
	/// Subscribe to `source` and produce MKV byte chunks, driving track
	/// (un)subscription from `catalog`.
	///
	/// `catalog` is any [`Stream`] of catalog snapshots, typically a
	/// [`catalog::Consumer`](crate::catalog::Consumer) directly, or narrowed to
	/// one rendition set via [`Stream::select`](crate::catalog::Stream::select).
	pub fn new(source: crate::Source, catalog: S) -> Self {
		Self {
			source,
			catalog: Some(catalog),
			delay: Duration::ZERO,
			jitter: crate::jitter::Buffer::new(Duration::ZERO),
			held: None,
			fragment_duration: None,
			tracks: HashMap::new(),
			catalog_snapshot: None,
			header_emitted: false,
			cluster: None,
		}
	}

	/// Release frames a fixed delay after their decode time, dropping late frames.
	///
	/// A source whose clock runs further off ours than the output's may follow fails the export
	/// once it has used half the delay. The same delay bounds each source's staleness. Zero (the default) releases each frame as
	/// soon as it is read and drops none as late, but a stalled group is still skipped once a
	/// newer one is ready.
	pub fn with_delay(mut self, delay: Duration) -> Self {
		self.delay = delay;
		self.jitter = crate::jitter::Buffer::new(delay);
		self
	}

	/// Cap the fragment (Cluster) duration.
	///
	/// By default Clusters roll over only on video keyframes (one cluster per GOP)
	/// or i16 timestamp overflow. Setting this caps each Cluster to roughly
	/// `duration` of frames, useful for downstream consumers that throttle by
	/// fragment rate. [`Duration::ZERO`] emits one Cluster per frame; otherwise
	/// the cap applies in addition to GOP / overflow rollover.
	///
	/// Accepts either `Duration` or `Option<Duration>` (where `None` restores
	/// the per-GOP default).
	pub fn with_fragment_duration(mut self, duration: impl Into<Option<Duration>>) -> Self {
		self.fragment_duration = duration.into();
		self
	}

	/// Get the next byte chunk.
	pub async fn next(&mut self) -> Result<Option<Bytes>> {
		kio::wait(|waiter| self.poll_next(waiter)).await
	}

	/// Poll for the next MKV byte chunk, or `None` once the export ends.
	pub fn poll_next(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<Bytes>>> {
		// 1. Drain catalog updates.
		while let Some(catalog) = self.catalog.as_mut() {
			match catalog.poll_next(waiter)? {
				Poll::Ready(Some(snapshot)) => self.update_catalog(snapshot.media())?,
				Poll::Ready(None) => {
					self.catalog = None;
					break;
				}
				Poll::Pending => break,
			}
		}

		self.fill(waiter)?;

		// 3. Before the header is emitted: keep pulling until every video
		// transform has produced its codec config. Frames arriving in this
		// phase land in `pending` already transformed; once the header lands
		// we drain them like any other pending frame.
		if !self.header_emitted {
			if self.header_ready() {
				let header = self.build_header()?;
				self.header_emitted = true;
				return Poll::Ready(Ok(Some(header)));
			}

			// Still waiting on codec configs. If every track is finished and
			// the header still isn't ready, the source never produced enough
			// info to build it.
			if self.catalog.is_none() && self.tracks.values().all(|t| t.finished) {
				return Poll::Ready(Ok(None));
			}

			return Poll::Pending;
		}

		loop {
			let (name, frame) = match self.held.take() {
				Some(held) => held,
				None => match self.jitter.poll_next(waiter) {
					Poll::Ready(ready) => (ready.track, ready.item),
					Poll::Pending => break,
				},
			};
			if let Some(chunk) = self.feed_frame(&name, frame)? {
				return Poll::Ready(Ok(Some(chunk)));
			}
		}
		if !self.jitter.is_empty() {
			return Poll::Pending;
		}

		// 6. End-of-stream: flush any open cluster when every subscribed track
		// has finished. We don't require catalog EOS here — a long-lived
		// producer may keep the catalog open even after every active track has
		// ended, and we'd rather flush the cluster than hold the last frames.
		if !self.tracks.is_empty() && self.tracks.values().all(|t| t.finished) {
			if let Some(cluster) = self.cluster.take() {
				return Poll::Ready(Ok(Some(cluster.finish())));
			}
			if self.catalog.is_none() {
				return Poll::Ready(Ok(None));
			}
		} else if self.catalog.is_none() && self.tracks.is_empty() {
			return Poll::Ready(Ok(None));
		}

		// A finished track keeps its place: the header declared it, and the catalog may still
		// list it or move it to another track.
		Poll::Pending
	}

	#[cfg(test)]
	pub(super) fn next_deadline(&self) -> Option<moq_clock::Instant> {
		self.jitter.next_deadline()
	}

	/// How many frames the export holds between its sources and the output.
	#[cfg(test)]
	pub(super) fn buffered(&self) -> usize {
		self.jitter.len() + self.tracks.values().map(|track| track.clock.len()).sum::<usize>()
	}

	/// How many video frames wait on a declared clock for their DTS.
	#[cfg(test)]
	pub(super) fn held(&self) -> usize {
		self.tracks
			.values()
			.filter_map(|track| track.declared.as_ref())
			.map(|clock| clock.len())
			.sum()
	}

	#[cfg(test)]
	pub(super) fn with_replay(mut self) -> Self {
		self.jitter = self.jitter.replay();
		self
	}

	/// Read each track until it has a frame due, preserving ingress timing before buffering.
	fn fill(&mut self, waiter: &kio::Waiter) -> Result<()> {
		for (name, track) in &mut self.tracks {
			loop {
				if self.header_emitted && self.jitter.full(name) {
					break;
				}
				if !self.header_emitted && track.pending.is_some() {
					break;
				}
				let arrival = if let Some(pending) = track.pending.take() {
					pending
				} else {
					if track.finished {
						break;
					}
					let (frame, arrived) = match track.source.poll_event(waiter)? {
						Poll::Ready(Some(Event::Frame { frame, arrived })) => (frame, arrived.max(track.subscribed)),
						// A group that ended or was cut short holds frames no later one can precede.
						Poll::Ready(Some(Event::GroupEnd | Event::Cut { .. })) => {
							if track.declared.is_some() {
								track.release(name, &mut self.jitter, true)?;
							}
							continue;
						}
						Poll::Ready(Some(Event::FrameEnd { .. })) => continue,
						Poll::Ready(None) => {
							track.finished = true;
							break;
						}
						Poll::Pending => break,
					};
					track.latest = track.latest.max(arrived);
					let geometry_ready = track.kind == TrackKind::Audio
						|| self
							.catalog_snapshot
							.as_ref()
							.and_then(|catalog| catalog.video.renditions.get(name))
							.is_some_and(|config| track.source.video_geometry_ready(config));
					let declaring = !self.header_emitted || track.moved;
					if declaring && (!track.source.header_ready() || !geometry_ready) {
						continue;
					}
					// A moved rendition's track must be the one the header declared, before any of
					// its frames go out.
					if track.moved
						&& let Some(catalog) = self.catalog_snapshot.as_ref()
					{
						if Some(track.track_entry(catalog, name)?) != track.entry {
							return Err(Error::TrackChanged(name.clone()).into());
						}
						track.moved = false;
					}
					crate::jitter::Arrival {
						arrived,
						read: moq_clock::now(),
						decode: frame.timestamp.into(),
						restart: track.source.markers(),
						skip: track.source.skips(),
						sync: track.kind == TrackKind::Audio || frame.keyframe,
						leading: false,
						item: frame,
					}
				};
				if !self.header_emitted {
					track.pending = Some(arrival);
					break;
				}
				let video = track.kind == TrackKind::Video;
				if video && arrival.restart != track.restart {
					track.release(name, &mut self.jitter, true)?;
					track.clock = Default::default();
					track.declared = None;
					track.restart = arrival.restart;
				}
				if video && arrival.item.keyframe {
					track.start_group(name, &mut self.jitter)?;
				}
				// A declared depth is checked even when nothing is scheduled by decode time.
				if video && (track.declared.is_some() || !self.delay.is_zero()) {
					track.push(name, arrival, &mut self.jitter)?;
				} else {
					self.jitter.push(name.clone(), arrival)?;
				}
			}
			if track.finished {
				track.release(name, &mut self.jitter, true)?;
			}
		}
		Ok(())
	}

	fn update_catalog(&mut self, mut catalog: Catalog) -> Result<()> {
		self.source.retain_selectable(&mut catalog);

		// Checked on every listing, so a rendition moved to a CMAF track is refused too.
		let mut active: HashMap<String, ()> = HashMap::new();
		for (name, config) in &catalog.video.renditions {
			ensure_legacy(&config.container, "video", name)?;
			active.insert(name.clone(), ());
		}
		for (name, config) in &catalog.audio.renditions {
			ensure_legacy(&config.container, "audio", name)?;
			active.insert(name.clone(), ());
		}

		// The MKV `Tracks` element is written once and can't be amended, so reject a
		// rendition added once the header has been emitted.
		//
		// A declared rendition that leaves the catalog (disabled, or retired) keeps its
		// track and is read to its own end: a disabled rendition idles until it re-enables,
		// and a retired one finishes on its own.
		if self.header_emitted {
			for name in active.keys() {
				if !self.tracks.contains_key(name) {
					return Err(Error::HeaderAddedTrack(name.clone()).into());
				}
			}
			for (name, config) in &catalog.audio.renditions {
				if let Some(track) = self.tracks.get_mut(name)
					&& !track.source.reads(&self.source, name, config)
					&& let Some(source) = ExportSource::for_audio(&self.source, name, config, self.delay)?
				{
					track.rebind(name, source, &mut self.jitter)?;
				}
			}
			for (name, config) in &catalog.video.renditions {
				if let Some(track) = self.tracks.get_mut(name) {
					if !track.source.reads(&self.source, name, config)
						&& let Some(source) = ExportSource::for_video(&self.source, name, config, self.delay)?
					{
						track.rebind(name, source, &mut self.jitter)?;
					}
					if track
						.config
						.as_ref()
						.is_some_and(|old| old.jitter != config.jitter || old.framerate != config.framerate)
					{
						track.reorder = None;
					}
					track.config = Some(config.clone());
				}
			}
			self.catalog_snapshot = Some(catalog);
			return Ok(());
		}

		// Nothing went out yet, so a rendition that left the catalog is dropped, and one the
		// catalog moved to another track starts over.
		self.tracks.retain(|name, track| {
			match (catalog.video.renditions.get(name), catalog.audio.renditions.get(name)) {
				(Some(config), _) => track.source.reads(&self.source, name, config),
				(None, Some(config)) => track.source.reads(&self.source, name, config),
				(None, None) => false,
			}
		});
		let mut next_track_number: u64 = self.tracks.values().map(|t| t.track_number).max().unwrap_or(0) + 1;

		for (name, config) in catalog.video.renditions.iter() {
			if self.tracks.contains_key(name) {
				continue;
			}
			let Some(source) = ExportSource::for_video(&self.source, name, config, self.delay)? else {
				continue;
			};
			let track = MkvTrack::new(source, TrackKind::Video, Some(config.clone()), next_track_number);
			self.tracks.insert(name.clone(), track);
			next_track_number += 1;
		}

		for (name, config) in catalog.audio.renditions.iter() {
			if self.tracks.contains_key(name) {
				continue;
			}
			let Some(source) = ExportSource::for_audio(&self.source, name, config, self.delay)? else {
				continue;
			};
			let track = MkvTrack::new(source, TrackKind::Audio, None, next_track_number);
			self.tracks.insert(name.clone(), track);
			next_track_number += 1;
		}

		// Tracks change only before the header, so the clock waits on this set from here on.
		self.jitter.expect(self.tracks.keys().cloned());
		self.catalog_snapshot = Some(catalog);
		Ok(())
	}

	/// Header is ready when the catalog snapshot has arrived, at least one track
	/// is subscribed, and every track's [`ExportSource`] has resolved its codec
	/// config (from the catalog `description` or built by the transform).
	///
	/// The non-empty guard matters for a mid-stream subscriber: before the
	/// catalog arrives `tracks` is empty, and `all()` would otherwise be
	/// vacuously true and send us into `build_header` with no snapshot.
	fn header_ready(&self) -> bool {
		let Some(catalog) = self.catalog_snapshot.as_ref() else {
			return false;
		};
		!self.tracks.is_empty()
			&& self.tracks.values().all(|t| t.source.header_ready())
			&& catalog.video.renditions.iter().all(|(name, config)| {
				self.tracks
					.get(name)
					.is_some_and(|track| track.source.video_geometry_ready(config))
			})
	}

	fn build_header(&mut self) -> Result<Bytes> {
		let catalog = self.catalog_snapshot.as_ref().ok_or(Error::NoCatalogSnapshot)?;

		// Decide DocType: webm only if every codec is WebM-allowed.
		let webm_only = catalog
			.video
			.renditions
			.values()
			.all(|c| matches!(c.codec, VideoCodec::VP8 | VideoCodec::VP9(_) | VideoCodec::AV1(_)))
			&& catalog
				.audio
				.renditions
				.values()
				.all(|c| matches!(c.codec, AudioCodec::Opus));
		let doc_type = if webm_only { "webm" } else { "matroska" };

		let mut entries: Vec<MatroskaSpec> = Vec::new();
		let video = catalog.video.renditions.keys().map(|name| (name, TrackKind::Video));
		let audio = catalog.audio.renditions.keys().map(|name| (name, TrackKind::Audio));
		for (name, kind) in video.chain(audio) {
			let track = self.tracks.get_mut(name).ok_or_else(|| match kind {
				TrackKind::Video => Error::MissingVideoTrack(name.clone()),
				TrackKind::Audio => Error::MissingAudioTrack(name.clone()),
			})?;
			let entry = track.track_entry(catalog, name)?;
			track.entry = Some(entry.clone());
			entries.push(entry);
		}

		let mut dest = Cursor::new(Vec::new());
		{
			let mut writer = WebmWriter::new(&mut dest);
			writer
				.write(&MatroskaSpec::Ebml(Master::Full(vec![
					MatroskaSpec::DocType(doc_type.to_string()),
					MatroskaSpec::DocTypeVersion(4),
					MatroskaSpec::DocTypeReadVersion(2),
				])))
				.map_err(Error::from)?;
			writer
				.write_advanced(
					&MatroskaSpec::Segment(Master::Start),
					WriteOptions::is_unknown_sized_element(),
				)
				.map_err(Error::from)?;
			writer
				.write(&MatroskaSpec::Info(Master::Full(vec![
					MatroskaSpec::TimestampScale(TIMESTAMP_SCALE_NS),
					MatroskaSpec::MuxingApp("moq-mux".to_string()),
					MatroskaSpec::WritingApp("moq-mux".to_string()),
				])))
				.map_err(Error::from)?;
			writer
				.write(&MatroskaSpec::Tracks(Master::Full(entries)))
				.map_err(Error::from)?;
			writer.flush().map_err(Error::from)?;
		}

		Ok(Bytes::from(dest.into_inner()))
	}

	/// Route an already-transformed frame through the cluster builder. Returns
	/// a chunk if the cluster rolled over (the returned chunk is the
	/// *previous* cluster; the new frame becomes the first block of a new
	/// open cluster).
	fn feed_frame(&mut self, name: &str, frame: Frame) -> Result<Option<Bytes>> {
		let track = self.tracks.get_mut(name).ok_or(Error::MissingTrack)?;
		let track_number = track.track_number;
		let is_video = track.kind == TrackKind::Video;
		let keyframe = frame.keyframe;

		// MKV's wire scale is ms (TIMESTAMP_SCALE_NS = 1_000_000). Re-express the
		// frame's timestamp directly at MILLI rather than going through micros.
		let frame_ticks: u64 = frame
			.timestamp
			.as_millis()
			.try_into()
			.map_err(|_| Error::TimestampU64)?;

		// A zero cap means one cluster per frame. Emit immediately instead of
		// holding the frame until its successor rolls the cluster.
		if self.fragment_duration == Some(Duration::ZERO) {
			// A reconfiguration can leave a cluster open. Roll it out first and
			// retry this frame on the next poll.
			if let Some(open) = self.cluster.take() {
				self.held = Some((name.to_string(), frame));
				return Ok(Some(open.finish()));
			}
			let mut cluster = ClusterBuilder::new(frame_ticks);
			cluster.append(track_number, frame_ticks, keyframe, &frame.payload, is_video)?;
			return Ok(Some(cluster.finish()));
		}

		let payload = &frame.payload;
		let roll_over = match &self.cluster {
			None => true,
			Some(cluster) => {
				let overflow = !cluster.fits(frame_ticks);
				// Roll on a video keyframe only once the cluster already has video
				// frames in it — otherwise audio that arrived before the first
				// keyframe would split into its own (un-renderable) cluster.
				let gop_boundary = is_video && keyframe && cluster.has_video;
				let too_long = self
					.fragment_duration
					.is_some_and(|d| frame_ticks.saturating_sub(cluster.start_ticks) >= d.as_millis() as u64);
				overflow || gop_boundary || too_long
			}
		};

		let emit = if roll_over {
			let finished = self.cluster.take().map(|c| c.finish());
			self.cluster = Some(ClusterBuilder::new(frame_ticks));
			finished
		} else {
			None
		};

		self.cluster
			.as_mut()
			.unwrap()
			.append(track_number, frame_ticks, keyframe, payload, is_video)?;

		Ok(emit)
	}
}

fn ensure_legacy(container: &Container, kind: &str, name: &str) -> Result<()> {
	match container {
		// MKV emits raw codec payloads, so it accepts both wire formats whose
		// frames are raw codec bitstreams (Legacy varint, LOC properties).
		Container::Legacy | Container::Loc => Ok(()),
		Container::Cmaf { .. } => Err(Error::UnsupportedCmafTrack {
			kind: kind.to_string(),
			name: name.to_string(),
		}
		.into()),
		Container::Unknown(unknown) => Err(crate::Error::unsupported_container(unknown)),
	}
}

fn build_video_track_entry(
	track_number: u64,
	config: &VideoConfig,
	description: Option<&Bytes>,
) -> Result<MatroskaSpec> {
	// The description came from either the catalog (avc1/hvc1 sources) or
	// the codec transform (Avc3/Hev1 sources synthesizing it from inline params).
	let codec_private = description.map(|b| b.to_vec());

	let (codec_id, codec_private) = match &config.codec {
		VideoCodec::VP8 => ("V_VP8", None),
		VideoCodec::VP9(_) => ("V_VP9", None),
		VideoCodec::AV1(_) => ("V_AV1", codec_private),
		VideoCodec::H264(_) => {
			let avcc = codec_private.ok_or(Error::MissingH264Avcc)?;
			("V_MPEG4/ISO/AVC", Some(avcc))
		}
		VideoCodec::H265(_) => {
			let hvcc = codec_private.ok_or(Error::MissingH265Hvcc)?;
			("V_MPEGH/ISO/HEVC", Some(hvcc))
		}
		other => return Err(Error::UnsupportedVideoExport(format!("{:?}", other)).into()),
	};

	let mut video_children: Vec<MatroskaSpec> = Vec::new();
	if let Some(w) = config.coded_width {
		video_children.push(MatroskaSpec::PixelWidth(w as u64));
	}
	if let Some(h) = config.coded_height {
		video_children.push(MatroskaSpec::PixelHeight(h as u64));
	}
	if let Some(colour) = config.color.as_ref().and_then(matroska_colour) {
		video_children.push(colour);
	}

	let mut entry: Vec<MatroskaSpec> = vec![
		MatroskaSpec::TrackNumber(track_number),
		MatroskaSpec::TrackUID(track_number),
		MatroskaSpec::TrackType(1),
		MatroskaSpec::CodecID(codec_id.to_string()),
	];
	if let Some(cp) = codec_private {
		entry.push(MatroskaSpec::CodecPrivate(cp));
	}
	if !video_children.is_empty() {
		entry.push(MatroskaSpec::Video(Master::Full(video_children)));
	}

	Ok(MatroskaSpec::TrackEntry(Master::Full(entry)))
}

/// The Matroska `Colour` element for a catalog colour, carrying only the fields it sets.
pub(super) fn matroska_colour(color: &hang::catalog::Color) -> Option<MatroskaSpec> {
	let mut children = Vec::new();
	if let Some(v) = color.matrix {
		children.push(MatroskaSpec::MatrixCoefficients(v.into()));
	}
	if let Some(range) = color.range {
		children.push(MatroskaSpec::Range(match range {
			hang::catalog::Range::Limited => 1,
			hang::catalog::Range::Full => 2,
		}));
	}
	if let Some(v) = color.transfer {
		children.push(MatroskaSpec::TransferCharacteristics(v.into()));
	}
	if let Some(v) = color.primaries {
		children.push(MatroskaSpec::Primaries(v.into()));
	}
	if let Some(light) = color.content_light {
		children.push(MatroskaSpec::MaxCLL(light.max_cll.into()));
		children.push(MatroskaSpec::MaxFALL(light.max_fall.into()));
	}
	if let Some(m) = color.mastering {
		children.push(MatroskaSpec::MasteringMetadata(Master::Full(vec![
			MatroskaSpec::PrimaryRChromaticityX(m.red.x),
			MatroskaSpec::PrimaryRChromaticityY(m.red.y),
			MatroskaSpec::PrimaryGChromaticityX(m.green.x),
			MatroskaSpec::PrimaryGChromaticityY(m.green.y),
			MatroskaSpec::PrimaryBChromaticityX(m.blue.x),
			MatroskaSpec::PrimaryBChromaticityY(m.blue.y),
			MatroskaSpec::WhitePointChromaticityX(m.white.x),
			MatroskaSpec::WhitePointChromaticityY(m.white.y),
			MatroskaSpec::LuminanceMax(m.max_luminance),
			MatroskaSpec::LuminanceMin(m.min_luminance),
		])));
	}
	(!children.is_empty()).then_some(MatroskaSpec::Colour(Master::Full(children)))
}

pub(super) fn build_audio_track_entry(track_number: u64, config: &AudioConfig) -> Result<MatroskaSpec> {
	let (codec_id, codec_private) = match &config.codec {
		AudioCodec::Opus => (
			"A_OPUS",
			Some(match &config.description {
				Some(description) => description.to_vec(),
				None => crate::codec::opus::Config::new(config.sample_rate, config.channel_count)
					.encode()?
					.to_vec(),
			}),
		),
		AudioCodec::AAC(_) => (
			"A_AAC",
			Some(
				config
					.description
					.as_ref()
					.ok_or(Error::MissingAacDescription)?
					.to_vec(),
			),
		),
		AudioCodec::Flac => (
			"A_FLAC",
			// A_FLAC CodecPrivate is the FLAC header (the `fLaC` marker plus metadata
			// blocks), which is exactly the catalog description.
			Some(
				config
					.description
					.as_ref()
					.ok_or(Error::MissingFlacDescription)?
					.to_vec(),
			),
		),
		// MP3 carries its config in band, so there's no codec private.
		AudioCodec::Mp3 => ("A_MPEG/L3", None),
		other => return Err(Error::UnsupportedAudioExport(format!("{:?}", other)).into()),
	};

	let mut entry = vec![
		MatroskaSpec::TrackNumber(track_number),
		MatroskaSpec::TrackUID(track_number),
		MatroskaSpec::TrackType(2),
		MatroskaSpec::CodecID(codec_id.to_string()),
	];
	// The catalog names the output rate. Under explicit SBR, Matroska puts the core's rate first
	// and the output after it, as a separate field.
	let core_rate = match (&config.codec, &codec_private) {
		(AudioCodec::AAC(aac), Some(asc)) if matches!(aac.profile, 5 | 29) => {
			Some(crate::codec::aac::in_band(asc)?.sample_rate)
		}
		_ => None,
	};
	if let Some(cp) = codec_private {
		entry.push(MatroskaSpec::CodecPrivate(cp));
	}
	let mut audio = vec![
		MatroskaSpec::SamplingFrequency(core_rate.unwrap_or(config.sample_rate) as f64),
		MatroskaSpec::Channels(config.channel_count as u64),
	];
	if core_rate.is_some_and(|core| core != config.sample_rate) {
		audio.push(MatroskaSpec::OutputSamplingFrequency(config.sample_rate as f64));
	}
	entry.push(MatroskaSpec::Audio(Master::Full(audio)));

	Ok(MatroskaSpec::TrackEntry(Master::Full(entry)))
}

/// EBML tag IDs we hand-encode.
const ID_CLUSTER: u32 = 0x1F43B675;
const ID_TIMESTAMP: u16 = 0xE7;
const ID_SIMPLEBLOCK: u16 = 0xA3;

/// Encode the body of a SimpleBlock element. The on-wire format is:
///   <track-number VINT> <timestamp i16 BE> <flags u8> <frame data>
fn encode_simple_block_body(track_number: u64, rel_ts: i16, keyframe: bool, payload: &[u8]) -> Bytes {
	let mut data = BytesMut::with_capacity(payload.len() + 11);
	write_vint(&mut data, track_number);
	data.put_i16(rel_ts);
	let mut flags: u8 = 0;
	if keyframe {
		flags |= 0x80;
	}
	data.put_u8(flags);
	data.extend_from_slice(payload);
	data.freeze()
}

/// Write an EBML tag ID (the canonical encoding has the high bit of the leading byte set).
fn write_tag_id(buf: &mut BytesMut, id: u32) {
	let bytes = id.to_be_bytes();
	let start = bytes.iter().position(|&b| b != 0).unwrap_or(3);
	buf.extend_from_slice(&bytes[start..]);
}

/// Encode a u64 as a big-endian byte sequence using the minimum number of bytes.
fn encode_uint(value: u64) -> Vec<u8> {
	if value == 0 {
		return vec![0];
	}
	let leading_zero_bytes = (value.leading_zeros() / 8) as usize;
	let bytes = value.to_be_bytes();
	bytes[leading_zero_bytes..].to_vec()
}

/// Encode an unsigned integer as an EBML variable-length integer (VINT).
fn write_vint(buf: &mut BytesMut, value: u64) {
	let mut width = 1;
	while width < 8 && value >= (1u64 << (7 * width)) - 1 {
		width += 1;
	}
	let marker = 1u8 << (8 - width);
	let mut bytes = [0u8; 8];
	for i in 0..width {
		bytes[width - 1 - i] = (value >> (8 * i)) as u8;
	}
	bytes[0] |= marker;
	buf.extend_from_slice(&bytes[..width]);
}
