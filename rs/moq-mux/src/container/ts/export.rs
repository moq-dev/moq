//! MPEG-TS muxer.
//!
//! [`Export`] subscribes to a MoQ broadcast and produces MPEG-TS: PAT/PMT program
//! tables and PES packets, packetized into 188-byte TS packets, with the PCR
//! riding its own adaptation-field-only packets on a fixed media-time grid.
//! Output is sliced on that grid rather than per media frame ([`Export::emit`]):
//! each [`Frame`] is one slot's clock packet plus the bytes belonging to it,
//! stamped at the slot boundary, so the clock a receiver recovers from byte
//! position agrees with the values, and a pacing caller releases each slot at
//! the instant it asserts. Video is carried as Annex-B, audio as ADTS AAC.
//!
//! Video flows through [`ExportSource`], which normalizes every H.264/H.265
//! source to length-prefixed NALU plus a resolved avcC/hvcC (parsing in-band
//! avc3/hev1 parameter sets out of the bitstream, or taking the catalog
//! `description` for out-of-band avc1/hvc1). The muxer then does one
//! length-prefixed -> Annex-B conversion, re-injecting the parameter sets as
//! inline NALs on every keyframe. CMAF tracks are rejected with a clear error.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::task::Poll;
use std::time::Duration;

use anyhow::Context;
use bytes::Bytes;
use hang::catalog::{AudioCodec, AudioConfig, Container, VideoCodec, VideoConfig};
use mpeg2ts::es::StreamId;
use mpeg2ts::es::StreamType;
use mpeg2ts::time::Timestamp as TsTimestamp;
use mpeg2ts::ts::payload::{Bytes as TsBytes, Pat, Pes, Pmt, Section};
use mpeg2ts::ts::{
	AdaptationField, ContinuityCounter, Descriptor, EsInfo, Pid, ProgramAssociation, TransportScramblingControl,
	TsHeader, TsPacket, TsPacketWriter, TsPayload, VersionNumber, WriteTsPacket,
};

use moq_net::Timestamp;

use crate::catalog::hang::Catalog;
use crate::catalog::{CatalogFormat, Stream};
use crate::codec::annexb;
use crate::container::{ExportSource, Frame};

use super::adts;
use super::catalog;

/// PID of the single program's PMT.
const PMT_PID: u16 = 0x1000;
/// First elementary-stream PID; each track gets the next one.
const FIRST_ES_PID: u16 = 0x1001;
/// Re-emit PAT/PMT at least this often (wall-clock of the media) for tune-in.
const PSI_INTERVAL: Duration = Duration::from_millis(500);
/// Emit a PCR on every crossing of this media-time grid ([`Export::emit`]).
/// TR 101 290 flags a gap over 40 ms; broadcast muxes emit every 25-40 ms.
pub(super) const PCR_INTERVAL: Duration = Duration::from_millis(25);
/// How many missed PCR slots to backfill at most: one second's worth. Frames
/// coarser than the grid cross several slots at a time and every one is filled so
/// the ramp stays uniform, down to a 1 fps cadence. Past this cap the media
/// didn't have a coarse cadence, it had an outage, and reconstructing a dense
/// clock history for a span that carried no bytes only stalls anything pacing on
/// the asserted values.
const PCR_BACKFILL: u128 = 40;

/// Subscribe to a broadcast and produce an MPEG-TS byte stream.
///
/// Use [`next`](Self::next) to pull one [`Frame`] per PCR grid slot: its `payload`
/// is the TS packets belonging to that slot, stamped at the slot's boundary. The
/// leading PAT/PMT rides on the first frame (so it inherits a real timestamp), and
/// is re-emitted at video keyframes and periodically for mid-stream tune-in.
/// Returns `None` when the broadcast ends.
pub struct Export<E: catalog::Catalog = ()> {
	source: crate::Source,
	catalog: Option<crate::catalog::Consumer<E>>,
	latency: Duration,

	tracks: HashMap<String, Track>,
	/// Continuity counter per PID (PAT, PMT, and each elementary stream).
	counters: HashMap<u16, ContinuityCounter>,
	/// Counter state before the uncommitted span, restored if a rewind discards it.
	span_counters: Option<HashMap<u16, ContinuityCounter>>,
	/// PMT program-level descriptors captured on import, re-emitted in the PMT.
	program_descriptors: Vec<catalog::Descriptor>,
	/// Transport/service identity captured on import, used to rebuild a consistent
	/// PAT/PMT. `None` for a media-only source, so a minimal identity is synthesized.
	program: Option<catalog::Program>,
	/// Standalone SI sections captured on import, keyed by PID and re-emitted verbatim
	/// on their own cadence. Opaque: export never parses a table it carries.
	si: BTreeMap<u16, catalog::Si>,
	/// When each SI PID was last emitted, so each honors its own interval ([`due`]).
	last_si: HashMap<u16, Timestamp>,

	/// Program tables, built once the track layout is known.
	psi: Option<Psi>,
	/// Media timestamp of the last PAT/PMT emission ([`due`]).
	last_psi: Option<Timestamp>,
	/// Grid slot of the last PCR emission ([`Self::emit`]).
	last_pcr: Option<u128>,
	/// Program generation being muxed; source counters are local to each rendition.
	epoch: u64,
	/// Generation of the last returned frame, updated only at the output boundary.
	emitted_epoch: u64,
	pcr_discontinuity: bool,
	/// TS packets muxed into the span that is still open.
	pending: Vec<u8>,
	/// Offsets into [`pending`](Self::pending) where a keyframe's packets begin, so
	/// the output frame carrying one keeps the flag.
	keyframes: Vec<usize>,
	/// Output frames ready to hand out, one per grid slot the last span covered.
	queue: VecDeque<Frame>,
	/// Continuity counter of the last packet emitted on the PCR PID. A clock packet
	/// carries no payload, so it repeats whatever preceded it on the wire rather
	/// than advancing the counter ([`Export::pcr_at`]).
	pcr_cc: Option<u8>,
	/// Media time the next span's bytes are transmitted from ([`Export::emit`]).
	clock: Option<Timestamp>,
	/// Earliest media timestamp muxed into the open span: the decode time its bytes
	/// have to arrive before, so it bounds how far the clock may run.
	low: Option<Timestamp>,
	/// Media timestamp that opened the current span. Reordered (B-frame) timestamps
	/// step backwards all the time, so a span closes on a timestamp passing this
	/// high-water mark rather than on every frame.
	watermark: Option<Timestamp>,
	/// Tune-in point: the first video keyframe's timestamp, captured when the program
	/// tables are built. Non-video frames before it are dropped so the keyframe leads
	/// the stream.
	///
	/// MPEG-TS carries the H.264/H.265 parameter sets in-band on the keyframe (unlike
	/// RTMP/CMAF, which carry the codec config out-of-band in the header). On a
	/// mid-stream join the audio source can start over a second before the oldest
	/// cached video keyframe; emitting that lead audio first would bury the parameter
	/// sets behind an audio-only preamble, and a live decoder probing the stream gives
	/// up before it ever configures video. `None` until the tables are built, and for
	/// programs with no video track (nothing to align to).
	video_start: Option<Timestamp>,
}

struct Pending {
	frame: Frame,
	discontinuity: u64,
}

struct Track {
	source: ExportSource,
	pending: Option<Pending>,
	/// Last consumed boundary count from this source. Never compared with peers.
	discontinuity: u64,
	/// Program generation this rendition has joined. Older generations are discarded.
	epoch: u64,
	finished: bool,
	pid: u16,
	kind: Kind,
	/// PMT ES-level descriptors to re-announce, captured verbatim on import (language,
	/// registration, ...). Empty for non-TS sources; AC-3/E-AC-3 then synthesize one.
	descriptors: Vec<catalog::Descriptor>,
	/// Last decode timestamp (continuous 90 kHz ticks) authored for this track, keeping the
	/// decode clock monotonic across reordered (B-frame) video. Only video uses it.
	last_dts: Option<u64>,
	/// High-water mark within this rendition, independent of cross-track skew.
	timeline: Option<Timestamp>,
	/// Decode-clock reserve (90 kHz ticks): how far ahead of its PTS each frame decodes. Taken
	/// from the catalog `jitter` (the reorder depth) so it is large enough for `DTS <= PTS`,
	/// or [`DEFAULT_DTS_RESERVE`] when the catalog declares none. Only video uses it.
	dts_reserve: u64,
}

#[derive(Clone)]
enum Kind {
	/// Video carries its TS stream type (H.264 = 0x1B, H.265 = 0x24).
	Video(StreamType),
	Aac {
		object_type: u8,
		sample_rate: u32,
		channel_count: u32,
	},
	/// Opus (private stream_type 0x06). Each frame is one Opus packet, prefixed with
	/// the Opus-in-TS access-unit control header and announced with the 'Opus'
	/// registration plus DVB extension descriptor.
	Opus { channel_count: u32 },
	/// MP2, carried verbatim. The sample rate picks the stream type on the way
	/// out (0x03 vs 0x04).
	Mp2 { sample_rate: u32 },
	/// AC-3 (ATSC stream_type 0x81), carried verbatim.
	Ac3,
	/// E-AC-3 (ATSC stream_type 0x87), carried verbatim.
	Eac3,
	/// An undecoded elementary stream carried verbatim (SCTE-35, private PES,
	/// teletext, ...). Re-announced in the PMT with its recorded `stream_type` and
	/// repacketized per its `framing`. `stream_id` is the original PES stream_id to
	/// re-emit (PES framing only; `None` falls back to `private_stream_1`).
	Verbatim {
		stream_type: u8,
		framing: catalog::Framing,
		stream_id: Option<u8>,
	},
}

/// The program tables plus the resolved PID layout.
struct Psi {
	pat: Pat,
	pmt: Pmt,
	pcr_pid: u16,
	/// PID the PMT rides on: the source's original (preserved from the service
	/// record) or the synthesized [`PMT_PID`] for a media-only source.
	pmt_pid: u16,
}

/// Per-frame PES descriptor (everything but the payload bytes).
struct PesUnit {
	pid: u16,
	is_video: bool,
	keyframe: bool,
	timestamp: Timestamp,
	/// Authored decode timestamp for a reordered (B-frame) video frame, in continuous
	/// (unwrapped) 90 kHz ticks (wrapped to the wire field in `write_pes`). `Some` only when
	/// it differs from the PTS; the PES then carries both PTS and DTS.
	dts: Option<u64>,
	/// Explicit PES stream_id (verbatim PES); `None` derives it from `is_video`.
	stream_id: Option<u8>,
}

impl Export {
	/// Subscribe to `source`, using the default catalog format.
	pub async fn new(source: crate::Source) -> Result<Self, crate::Error> {
		Self::with_catalog_format(source, CatalogFormat::default()).await
	}

	/// Subscribe to `source`, selecting an explicit catalog format. Media only;
	/// any catalog extension (e.g. the `mpegts` verbatim streams) is ignored.
	pub async fn with_catalog_format(
		source: crate::Source,
		catalog_format: CatalogFormat,
	) -> Result<Self, crate::Error> {
		Self::build(source, catalog_format).await
	}
}

impl Export<catalog::Ext> {
	/// Subscribe to `source`, exporting its `mpegts` verbatim streams (SCTE-35,
	/// private data, ...) back to MPEG-TS alongside the media. The `Self` type pins
	/// the extension, so callers write `Export::with_ts(..)` with no turbofish (the
	/// plain constructors are media-only).
	pub async fn with_ts(source: crate::Source, catalog_format: CatalogFormat) -> Result<Self, crate::Error> {
		Self::build(source, catalog_format).await
	}
}

impl<E: catalog::Catalog> Export<E> {
	/// Shared constructor. The public entry points each live on a concrete
	/// `Export<E>` impl that pins `E`, so the extension is chosen by which one you call.
	async fn build(source: crate::Source, catalog_format: CatalogFormat) -> Result<Self, crate::Error> {
		let broadcast = source.broadcast().await?;
		let catalog = crate::catalog::Consumer::<E>::new(&broadcast, catalog_format).await?;
		Ok(Self {
			source,
			catalog: Some(catalog),
			latency: Duration::ZERO,
			tracks: HashMap::new(),
			counters: HashMap::new(),
			span_counters: None,
			program_descriptors: Vec::new(),
			program: None,
			si: BTreeMap::new(),
			last_si: HashMap::new(),
			psi: None,
			last_psi: None,
			last_pcr: None,
			epoch: 0,
			emitted_epoch: 0,
			pcr_discontinuity: false,
			pending: Vec::new(),
			keyframes: Vec::new(),
			queue: VecDeque::new(),
			pcr_cc: None,
			clock: None,
			low: None,
			watermark: None,
			video_start: None,
		})
	}

	/// Set the maximum buffering latency for each per-track source.
	pub fn with_latency(mut self, latency: Duration) -> Self {
		self.latency = latency;
		self
	}

	/// Get the next muxed frame.
	///
	/// Each [`Frame`] carries one slice of the PCR grid in `payload`: the clock
	/// packets that slice opens with, followed by the muxed bytes belonging to it.
	/// It is stamped with the media time that slice starts at, so a transport can
	/// pace delivery on the media clock, and `keyframe` marks the slice a video
	/// keyframe begins in. The leading PAT/PMT rides on the first slice, and is
	/// re-emitted at video keyframes and periodically for mid-stream tune-in.
	/// Returns `None` when the broadcast ends. `duration` is always `None`: the
	/// muxer has no use for it.
	pub async fn next(&mut self) -> crate::Result<Option<Frame>> {
		kio::wait(|waiter| self.poll_next(waiter)).await
	}

	pub fn poll_next(&mut self, waiter: &kio::Waiter) -> Poll<crate::Result<Option<Frame>>> {
		// 1. Drain catalog updates, discovering the track layout.
		while let Some(catalog) = self.catalog.as_mut() {
			match catalog.poll_next(waiter)? {
				Poll::Ready(Some(snapshot)) => self.update_catalog(snapshot)?,
				Poll::Ready(None) => {
					self.catalog = None;
					break;
				}
				Poll::Pending => break,
			}
		}

		// 2. Pull a frame into every idle track.
		self.fill(waiter)?;

		// 3. Build the program tables once the layout is resolved and every
		// track's codec config is ready. The tables aren't emitted here: PSI has
		// no media time of its own, so `mux` prepends them to the first frame's
		// packets instead, letting the leading PAT/PMT inherit a real timestamp.
		if self.psi.is_none() {
			if self.tracks.is_empty() {
				// No tracks yet. If the catalog is also done, the broadcast is empty.
				if self.catalog.is_none() {
					return Poll::Ready(Ok(None));
				}
				return Poll::Pending;
			}
			if !self.header_ready() || !self.video_ready() {
				// Hold all output (tables and audio alike) until codec configs resolve
				// and, when the program has a video rendition, its first keyframe is
				// buffered: the stream must begin on that keyframe so the in-band
				// parameter sets lead it. An audio-only program has nothing to wait for.
				// If every track finished without producing a config, it can't be muxed.
				if self.catalog.is_none() && self.tracks.values().all(|t| t.finished) {
					return Poll::Ready(Ok(None));
				}
				return Poll::Pending;
			}
			self.build_psi()?;
			// Anchor tune-in to the first video keyframe and drop any non-video frame
			// already buffered ahead of it (see `video_start`).
			self.video_start = self.first_video_pts();
			if let Some(start) = self.video_start {
				for track in self.tracks.values_mut() {
					if !matches!(track.kind, Kind::Video(_))
						&& track.pending.as_ref().is_some_and(|p| p.frame.timestamp < start)
					{
						track.pending = None;
					}
				}
			}
		}

		// 4. Mux the smallest-timestamp pending frame into the open span (the first
		// one carries the buffered PAT/PMT). Nothing goes out until a later
		// timestamp measures that span: only then is it known how many bytes it
		// carried, which is what puts the clock packets at the byte position their
		// own values imply and lets the caller's pacer release each at the instant
		// it asserts. See [`Self::advance`].
		loop {
			if let Some(out) = self.queue.pop_front() {
				self.emitted_epoch = self.epoch;
				return Poll::Ready(Ok(Some(out)));
			}
			let Some(name) = self.pick_next_track() else { break };
			let pending = self.tracks.get_mut(&name).unwrap().pending.take().unwrap();
			let changed = pending.discontinuity != self.tracks[&name].discontinuity;
			if changed {
				let joined = self.tracks[&name].epoch == self.epoch;
				if joined {
					let backwards = self.tracks[&name]
						.timeline
						.is_some_and(|last| pending.frame.timestamp < last);
					self.rewind(backwards);
				}
				let track = self.tracks.get_mut(&name).unwrap();
				track.discontinuity = pending.discontinuity;
				track.epoch = self.epoch;
				track.last_dts = None;
				track.timeline = None;
			}
			let frame = pending.frame;
			let track = self.tracks.get_mut(&name).unwrap();
			track.timeline = Some(track.timeline.map_or(frame.timestamp, |last| last.max(frame.timestamp)));
			self.advance(frame.timestamp)?;
			self.mux(&name, frame)?;
			// Refill the track we just drained: the next span is measured by its
			// successor, and without the refill nothing would be polling for it.
			self.fill(waiter)?;
		}

		// 5. Once every track has drained, no later timestamp is coming to measure
		// the open span, so its bytes go out whole. That's independent of the
		// catalog: a retained track finishes while the broadcast stays live, and
		// holding its tail until the catalog closed would strand it indefinitely.
		let drained = !self.tracks.is_empty() && self.tracks.values().all(|t| t.finished);
		if drained {
			self.emit(None)?;
			if let Some(out) = self.queue.pop_front() {
				self.emitted_epoch = self.epoch;
				return Poll::Ready(Ok(Some(out)));
			}
		}

		// End of stream once the catalog is closed too: nothing more can appear.
		if self.catalog.is_none() && (drained || self.tracks.is_empty()) {
			return Poll::Ready(Ok(None));
		}

		Poll::Pending
	}

	/// Pull a frame into every idle track.
	///
	/// [`ExportSource`] has already transformed Annex-B avc3/hev1 into
	/// length-prefixed form and resolved the avcC/hvcC. Before the program tables
	/// are written, drop slices that arrive before their codec config resolves: a
	/// receiver joining mid-GOP can't use them, and parking them would stop us
	/// polling for the keyframe that carries the parameter sets.
	fn fill(&mut self, waiter: &kio::Waiter) -> crate::Result<()> {
		let waiting_for_header = self.psi.is_none();
		let video_start = self.video_start;
		for track in self.tracks.values_mut() {
			if track.pending.is_some() || track.finished {
				continue;
			}
			let is_video = matches!(track.kind, Kind::Video(_));
			loop {
				match track.source.poll_read(waiter)? {
					Poll::Ready(Some(frame)) => {
						if waiting_for_header && !track.source.header_ready() {
							continue;
						}
						let discontinuity = track.source.discontinuity();
						let changed = discontinuity != track.discontinuity;
						// A peer has rewound the program. This rendition must cross its own
						// boundary before its old timeline can enter the mux again.
						if track.epoch != self.epoch && !changed {
							continue;
						}
						// Tune-in alignment: drop non-video frames before the first video
						// keyframe (see `video_start`) so the in-band SPS/PPS leads the stream.
						if let Some(start) = video_start
							&& !is_video && !changed
							&& frame.timestamp < start
						{
							continue;
						}
						track.pending = Some(Pending { frame, discontinuity });
						break;
					}
					Poll::Ready(None) => {
						track.finished = true;
						break;
					}
					Poll::Pending => break,
				}
			}
		}
		Ok(())
	}

	fn update_catalog(&mut self, mut catalog: Catalog<E>) -> anyhow::Result<()> {
		self.source.retain_valid(&mut catalog);

		// The MPEG-TS section lives in the extension. The trait only exposes
		// `mpegts_mut`, and this snapshot is owned, so clone it out (`()` yields the
		// empty default: no verbatim streams, no preserved PIDs/descriptors).
		let mpegts = catalog.mpegts_mut().cloned().unwrap_or_default();
		self.program_descriptors = mpegts.program_descriptors.clone();
		self.program = mpegts.program.clone();
		self.si = mpegts.si.clone();

		// The desired track set: media renditions plus the verbatim streams.
		let mut active: BTreeMap<String, ()> = BTreeMap::new();
		for name in catalog.video.renditions.keys() {
			active.insert(name.clone(), ());
		}
		for name in catalog.audio.renditions.keys() {
			active.insert(name.clone(), ());
		}
		for (name, track) in mpegts.tracks.iter() {
			if track.verbatim.is_some() {
				active.insert(name.clone(), ());
			}
		}

		// The program tables are written once; reject layout changes afterwards.
		if self.psi.is_some() {
			for name in active.keys() {
				anyhow::ensure!(
					self.tracks.contains_key(name),
					"TS track layout changed after PAT/PMT was emitted: '{name}' added"
				);
			}
			for name in self.tracks.keys() {
				anyhow::ensure!(
					active.contains_key(name),
					"TS track layout changed after PAT/PMT was emitted: '{name}' removed"
				);
			}
			return Ok(());
		}

		// Assign a PID to every desired track: prefer the original recorded in the
		// `mpegts` section, then fill the rest from FIRST_ES_PID. The importer fills
		// PIDs, descriptors, and stream_ids across several catalog publishes, so this
		// runs every snapshot until the PMT is built and the tracks below are
		// *refreshed*, not latched from the first (partial) snapshot.
		let mut used: Vec<u16> = vec![0x0000, self.pmt_pid(), 0x1FFF];
		let mut pids: BTreeMap<String, u16> = BTreeMap::new();
		for name in active.keys() {
			if let Some(pid) = mpegts.tracks.get(name).map(|t| t.pid)
				&& !used.contains(&pid)
			{
				used.push(pid);
				pids.insert(name.clone(), pid);
			}
		}
		for name in active.keys() {
			if !pids.contains_key(name) {
				let mut pid = FIRST_ES_PID;
				while used.contains(&pid) {
					pid += 1;
				}
				used.push(pid);
				pids.insert(name.clone(), pid);
			}
		}

		// Reuse each track's existing source (and any pending frame) by name; refresh
		// its PID, kind, and descriptors from this snapshot. Drop tracks no longer present.
		let mut old = std::mem::take(&mut self.tracks);
		for (name, config) in catalog.video.renditions.iter() {
			let kind = video_kind(config, name)?;
			let descriptors = track_descriptors(&mpegts, name);
			let pid = pids[name];
			// The catalog `jitter` carries the reorder depth (max PTS - DTS), so use it as the
			// decode-clock reserve; it may arrive in a later snapshot, so refresh it each time.
			let reserve = dts_reserve(config);
			match old.remove(name) {
				Some(mut track) => {
					track.pid = pid;
					track.kind = kind;
					track.descriptors = descriptors;
					track.dts_reserve = reserve;
					self.tracks.insert(name.clone(), track);
				}
				None => {
					let Some(source) = ExportSource::for_video(&self.source, name, config, self.latency)? else {
						continue;
					};
					self.insert_track(name, source, pid, kind, descriptors, reserve);
				}
			}
		}
		for (name, config) in catalog.audio.renditions.iter() {
			let kind = audio_kind(config, name)?;
			let descriptors = track_descriptors(&mpegts, name);
			let pid = pids[name];
			match old.remove(name) {
				Some(mut track) => {
					track.pid = pid;
					track.kind = kind;
					track.descriptors = descriptors;
					self.tracks.insert(name.clone(), track);
				}
				None => {
					let Some(source) = ExportSource::for_audio(&self.source, name, config, self.latency)? else {
						continue;
					};
					self.insert_track(name, source, pid, kind, descriptors, DEFAULT_DTS_RESERVE);
				}
			}
		}
		for (name, track) in mpegts.tracks.iter() {
			let Some(verbatim) = &track.verbatim else {
				continue;
			};
			let kind = Kind::Verbatim {
				stream_type: verbatim.stream_type,
				framing: verbatim.framing,
				stream_id: verbatim.stream_id,
			};
			let descriptors = track.descriptors.clone();
			let pid = pids[name];
			match old.remove(name) {
				Some(mut existing) => {
					existing.pid = pid;
					existing.kind = kind;
					existing.descriptors = descriptors;
					self.tracks.insert(name.clone(), existing);
				}
				None => {
					let source = ExportSource::for_stream(&self.source, name, self.latency)?;
					self.insert_track(name, source, pid, kind, descriptors, DEFAULT_DTS_RESERVE);
				}
			}
		}
		Ok(())
	}

	/// Insert a freshly created export track.
	fn insert_track(
		&mut self,
		name: &str,
		source: ExportSource,
		pid: u16,
		kind: Kind,
		descriptors: Vec<catalog::Descriptor>,
		dts_reserve: u64,
	) {
		self.tracks.insert(
			name.to_string(),
			Track {
				source,
				pending: None,
				discontinuity: 0,
				epoch: self.epoch,
				finished: false,
				pid,
				kind,
				descriptors,
				last_dts: None,
				timeline: None,
				dts_reserve,
			},
		);
	}

	/// The discontinuity counter of the most recently returned output frame.
	/// Compare across reads and re-anchor pacing when it changes. Renditions have
	/// independent source counters; this counter describes the emitted program.
	pub fn discontinuity(&self) -> u64 {
		self.emitted_epoch
	}

	/// Discard uncommitted bytes and restart the program clock. On a backwards
	/// boundary, peers must cross their own boundary before joining this generation.
	fn rewind(&mut self, backwards: bool) {
		self.epoch += 1;
		if let Some(counters) = self.span_counters.take() {
			self.counters = counters;
		}
		self.pending.clear();
		self.keyframes.clear();
		self.queue.clear();
		self.watermark = None;
		self.clock = None;
		self.low = None;
		self.last_pcr = None;
		self.last_psi = None;
		self.last_si.clear();
		self.video_start = None;
		self.pcr_discontinuity = true;
		for track in self.tracks.values_mut() {
			track.last_dts = None;
			if !backwards {
				track.epoch = self.epoch;
			} else if track
				.pending
				.as_ref()
				.is_some_and(|p| p.discontinuity == track.discontinuity)
			{
				track.pending = None;
			}
		}
	}

	/// Header is ready when every track's [`ExportSource`] has resolved its
	/// codec config (from the catalog `description`, or built by the transform).
	fn header_ready(&self) -> bool {
		self.tracks.values().all(|t| t.source.header_ready())
	}

	/// Every video track has buffered its first frame (the keyframe) or finished.
	/// The tables wait for this so the tune-in point ([`Self::video_start`]) can be
	/// read from the keyframe before any audio is emitted ahead of it. A program
	/// with no video track is trivially ready.
	fn video_ready(&self) -> bool {
		self.tracks
			.values()
			.filter(|t| matches!(t.kind, Kind::Video(_)))
			.all(|t| t.pending.is_some() || t.finished)
	}

	/// The smallest timestamp among the video tracks' buffered frames: the first
	/// video keyframe, since pre-keyframe video frames are dropped before the tables
	/// are built. `None` when no video track has a buffered frame (audio-only program).
	fn first_video_pts(&self) -> Option<Timestamp> {
		self.tracks
			.values()
			.filter(|t| matches!(t.kind, Kind::Video(_)))
			.filter_map(|t| t.pending.as_ref().map(|p| p.frame.timestamp))
			.min()
	}

	/// PID the PMT rides on: the source's original (preserved in the service record),
	/// or the synthesized [`PMT_PID`] for a media-only source or an invalid (zero) value.
	fn pmt_pid(&self) -> u16 {
		self.program
			.as_ref()
			.map(|s| s.pmt_pid)
			.filter(|&pid| pid != 0)
			.unwrap_or(PMT_PID)
	}

	/// Build the PAT/PMT once every track's PID and codec is known.
	fn build_psi(&mut self) -> anyhow::Result<()> {
		// Order tracks by PID for a stable layout; first video track carries the PCR.
		let mut tracks: Vec<&Track> = self.tracks.values().collect();
		tracks.sort_by_key(|t| t.pid);

		// Section-framed verbatim streams (SCTE-35, ...) are stamped on the video clock
		// and carry no PTS for the PCR, so they need a video track; audio alone would
		// leave them pinned to zero.
		let needs_clock = tracks.iter().any(|t| {
			matches!(
				&t.kind,
				Kind::Verbatim {
					framing: catalog::Framing::Section,
					..
				}
			)
		});
		let video = tracks.iter().find(|t| matches!(t.kind, Kind::Video(_)));
		anyhow::ensure!(
			!needs_clock || video.is_some(),
			"TS export of section-framed verbatim streams (e.g. SCTE-35) requires a video track for the program clock"
		);
		let pcr_pid = video
			.or_else(|| {
				tracks.iter().find(|t| {
					matches!(
						t.kind,
						Kind::Aac { .. } | Kind::Opus { .. } | Kind::Mp2 { .. } | Kind::Ac3 | Kind::Eac3
					)
				})
			})
			.map(|t| t.pid)
			.context("TS export requires a video or audio track for the PCR")?;

		let es_info = tracks
			.iter()
			.map(|t| {
				let stream_type = match &t.kind {
					Kind::Video(stream_type) => *stream_type,
					Kind::Aac { .. } => StreamType::AdtsAac,
					// Opus rides private-data PES; the registration + extension descriptors
					// below tell the demuxer it's Opus.
					Kind::Opus { .. } => StreamType::from_u8(0x06).map_err(anyhow::Error::msg)?,
					// Half-rate MPEG-2 BC audio (< 32 kHz) re-announces as 0x04; the full
					// rates are MPEG-1 (0x03). The catalog sample rate came from the frame
					// header, so the mapping is faithful.
					Kind::Mp2 { sample_rate } if *sample_rate < 32000 => StreamType::Mpeg2HalvedSampleRateAudio,
					Kind::Mp2 { .. } => StreamType::Mpeg1Audio,
					Kind::Ac3 => StreamType::DolbyDigitalUpToSixChannelAudio,
					Kind::Eac3 => StreamType::DolbyDigitalPlusUpTo16ChannelAudioForAtsc,
					Kind::Verbatim { stream_type, .. } => {
						StreamType::from_u8(*stream_type).map_err(anyhow::Error::msg)?
					}
				};
				// Prefer the descriptors captured verbatim on import; otherwise synthesize
				// the ATSC Dolby registration so a fresh (non-TS) AC-3/E-AC-3 track is
				// still announced the way the import path expects.
				let descriptors = if !t.descriptors.is_empty() {
					to_pmt_descriptors(&t.descriptors)
				} else {
					match &t.kind {
						Kind::Ac3 => vec![Descriptor {
							tag: 0x05,
							data: b"AC-3".to_vec(),
						}],
						Kind::Eac3 => vec![Descriptor {
							tag: 0x05,
							data: b"EAC3".to_vec(),
						}],
						Kind::Opus { channel_count } => opus_descriptors(*channel_count),
						_ => Vec::new(),
					}
				};
				Ok(EsInfo {
					stream_type,
					elementary_pid: Pid::new(t.pid)?,
					descriptors,
				})
			})
			.collect::<anyhow::Result<Vec<_>>>()?;

		// Re-emit the captured program-level descriptors. With none (a non-TS source),
		// derive the SCTE-35 'CUEI' registration when a 0x86 verbatim stream is present.
		let program_info = if !self.program_descriptors.is_empty() {
			to_pmt_descriptors(&self.program_descriptors)
		} else if tracks.iter().any(|t| {
			// Only derive CUEI for section-framed 0x86 (SCTE-35); a PES-framed 0x86
			// (e.g. DTS audio) must not advertise SCTE-35 section signaling.
			matches!(
				&t.kind,
				Kind::Verbatim {
					stream_type: 0x86,
					framing: catalog::Framing::Section,
					..
				}
			)
		}) {
			vec![Descriptor {
				tag: 0x05,
				data: b"CUEI".to_vec(),
			}]
		} else {
			Vec::new()
		};

		// Preserve the source's program identity so the rebuilt PAT/PMT stay consistent
		// with the carried SI; synthesize a minimal identity otherwise.
		let pmt_pid = self.pmt_pid();
		let transport_stream_id = self.program.as_ref().map(|s| s.transport_stream_id).unwrap_or(1);
		let program_number = self
			.program
			.as_ref()
			.map(|s| s.program_number)
			.filter(|&id| id != 0)
			.unwrap_or(1);

		let pat = Pat {
			transport_stream_id,
			version_number: VersionNumber::default(),
			table: vec![ProgramAssociation {
				program_num: program_number,
				program_map_pid: Pid::new(pmt_pid)?,
			}],
		};
		let pmt = Pmt {
			program_num: program_number,
			pcr_pid: Some(Pid::new(pcr_pid)?),
			version_number: VersionNumber::default(),
			program_info,
			es_info,
		};

		self.psi = Some(Psi {
			pat,
			pmt,
			pcr_pid,
			pmt_pid,
		});
		Ok(())
	}

	/// A boundary takes precedence over stale peer data; otherwise use timestamp order.
	fn pick_next_track(&self) -> Option<String> {
		self.tracks
			.iter()
			.filter_map(|(n, t)| t.pending.as_ref().map(|p| (p.frame.timestamp, t.pid, n)))
			.min_by_key(|(timestamp, pid, name)| {
				let track = &self.tracks[*name];
				let changed = track.pending.as_ref().unwrap().discontinuity != track.discontinuity;
				(!changed, *timestamp, *pid, *name)
			})
			.map(|(_, _, name)| name.clone())
	}

	/// Packetize one media frame into the open span, re-emitting PAT/PMT before
	/// video keyframes (and periodically) so receivers can tune in mid-stream.
	///
	/// The bytes are buffered rather than returned: which grid slots they belong
	/// to isn't known until a later timestamp measures the span (see
	/// [`Self::advance`]).
	fn mux(&mut self, name: &str, frame: Frame) -> anyhow::Result<()> {
		if self.span_counters.is_none() {
			self.span_counters = Some(self.counters.clone());
		}
		let track = self.tracks.get(name).context("missing track")?;
		let pid = track.pid;
		let kind = track.kind.clone();
		let is_video = matches!(kind, Kind::Video(_));
		let timestamp = frame.timestamp;
		let keyframe = frame.keyframe;

		// Build the elementary-stream payload for this frame. Video needs the
		// resolved avcC/hvcC to rewrite length-prefixed NALs as Annex-B. Section-framed
		// verbatim streams carry no PES payload; the section is written separately below.
		let es_payload = match &kind {
			Kind::Video(stream_type) => Some(video_es_payload(*stream_type, track.source.description(), &frame)?),
			Kind::Aac {
				object_type,
				sample_rate,
				channel_count,
			} => {
				let header = adts::write_header(*object_type, *sample_rate, *channel_count, frame.payload.len())?;
				let mut framed = Vec::with_capacity(7 + frame.payload.len());
				framed.extend_from_slice(&header);
				framed.extend_from_slice(&frame.payload);
				Some(framed)
			}
			// Each moq Opus frame is one packet; prefix the Opus-in-TS control header.
			Kind::Opus { .. } => Some(opus_es_payload(&frame.payload)),
			// Legacy audio frames were ingested whole (framing header included), so
			// they pass through untouched. PES-framed verbatim payloads likewise.
			Kind::Mp2 { .. } | Kind::Ac3 | Kind::Eac3 => Some(frame.payload.to_vec()),
			Kind::Verbatim {
				framing: catalog::Framing::Pes,
				..
			} => Some(frame.payload.to_vec()),
			Kind::Verbatim {
				framing: catalog::Framing::Section,
				..
			} => None,
		};

		// Author a monotonic decode timeline for reordered video (B-frames). Other kinds
		// never reorder, so DTS == PTS and the PES stays PTS-only.
		let dts = if is_video {
			let pts = to_ticks(frame.timestamp);
			let track = self.tracks.get_mut(name).context("missing track")?;
			author_dts(pts, track.dts_reserve, &mut track.last_dts)
		} else {
			None
		};

		let mut out = Vec::with_capacity(TsPacket::SIZE);

		// Refresh PSI at keyframes or after the interval lapses.
		if (is_video && frame.keyframe) || due(frame.timestamp, self.last_psi, PSI_INTERVAL) {
			let psi = self.psi.as_ref().context("PSI not built")?;
			let pmt_pid = psi.pmt_pid;
			let pat = TsPayload::Pat(psi.pat.clone());
			let pmt = TsPayload::Pmt(psi.pmt.clone());
			self.write_packet(&mut out, Pid::PAT, None, pat)?;
			self.write_packet(&mut out, pmt_pid, None, pmt)?;
			self.last_psi = Some(frame.timestamp);
		}

		// Re-emit each SI PID's sections verbatim on its own cadence, which is the
		// table's own repetition requirement rather than the PSI interval: an SDT wants
		// 2s where the PSI wants 500ms, and an EPG table would want far less again.
		// Unknown PIDs have no declared interval and fall back to the PSI cadence.
		// `Bytes` clones are refcount bumps, and only a due PID is collected at all.
		let pending: Vec<(u16, Vec<Bytes>)> = self
			.si
			.iter()
			.filter(|(pid, si)| {
				let interval = si.interval.unwrap_or(PSI_INTERVAL);
				due(frame.timestamp, self.last_si.get(*pid).copied(), interval)
			})
			.map(|(pid, si)| (*pid, si.sections.clone()))
			.collect();
		for (pid, sections) in pending {
			for section in &sections {
				self.write_section(&mut out, pid, section)?;
			}
			self.last_si.insert(pid, frame.timestamp);
		}

		match es_payload {
			// Section-framed verbatim (SCTE-35, ...) rides in private sections, not PES;
			// carry the bytes verbatim.
			None => self.write_section(&mut out, pid, &frame.payload)?,
			Some(es_payload) => {
				// Verbatim PES re-emits its original stream_id (falling back to
				// private_stream_1 for an undecoded stream with none recorded); media
				// derives it from is_video.
				let stream_id = match &kind {
					Kind::Verbatim { stream_id, .. } => Some(stream_id.unwrap_or(StreamId::PRIVATE_STREAM_1)),
					// Opus is private-data PES, carried under private_stream_1 like ffmpeg.
					Kind::Opus { .. } => Some(StreamId::PRIVATE_STREAM_1),
					_ => None,
				};
				let unit = PesUnit {
					pid,
					is_video,
					keyframe: frame.keyframe,
					timestamp: frame.timestamp,
					dts,
					stream_id,
				};
				self.write_pes(&mut out, &unit, &es_payload)?;
			}
		}
		if keyframe {
			self.keyframes.push(self.pending.len());
		}
		self.pending.extend_from_slice(&out);
		self.low = Some(self.low.map_or(timestamp, |low| low.min(timestamp)));
		Ok(())
	}

	/// Close the open span if `ts` passes the watermark, laying its bytes out.
	///
	/// `ts` belongs to the frame about to be muxed. Passing the watermark means the
	/// span that timestamp opened is done: everything buffered since is exactly the
	/// bytes it carried, and the distance from it measures how long it ran. A
	/// reordered (B-frame) timestamp that trails the watermark closes nothing, and
	/// its bytes join the open span, which is where they are transmitted anyway.
	///
	/// This is why nothing goes out on arrival, and why it can't. A span's bytes
	/// have to reach a receiver before the units in it decode, so they ride the
	/// grid slots *preceding* the span, which are only known once the span has
	/// closed. That is the mux buffer: the exporter runs two spans behind the media
	/// clock, by a constant amount, so a caller pacing on the stamps still releases
	/// every slot at the interval its own PCR value asserts.
	fn advance(&mut self, ts: Timestamp) -> anyhow::Result<()> {
		let Some(watermark) = self.watermark else {
			self.watermark = Some(ts);
			return Ok(());
		};
		if ts <= watermark {
			return Ok(());
		}
		self.watermark = Some(ts);
		let span = ts.as_nanos().saturating_sub(watermark.as_nanos());
		self.emit(Some(span))
	}

	/// Lay the open span's bytes across the grid slots that run up to its decode
	/// time, one [`Frame`] per slot. `span` is how long the span ran, or `None` at
	/// end of stream, where nothing measured it.
	///
	/// Each frame opens with the clock packets whose slot boundary it starts at,
	/// then carries the share of the bytes its slice of the interval earns. Three
	/// things follow, and they are the whole point of muxing this way. The packet
	/// count between consecutive PCRs is proportional to the difference between
	/// their values, so a consumer holding only the byte stream (which is every
	/// MPEG-TS tool) recovers the same clock from byte position that the values
	/// assert. Each frame is stamped at its own slot boundary, so a pacing caller
	/// releases the clock at the instant it asserts rather than when the media that
	/// revealed it arrived. And the interval ends at the span's earliest media
	/// timestamp, so every byte precedes the decode time of the unit it belongs to.
	///
	/// The PES units cannot carry the clock themselves. Frames arrive in decode
	/// order, so the authored DTS is a saw: a reference frame leaps a whole reorder
	/// span ahead and each B-frame nudges one tick past it. A PCR sampled from it
	/// freezes and jumps, and no downstream CBR stage can repair that, because a
	/// groomer can only place the clock samples it receives. So the PCR asserts its
	/// own uniform ramp instead: absolute grid slots on the media timeline (shared by
	/// every exporter of the broadcast, like [`due`]), each backed off by the largest
	/// decode-clock reserve of any track so every PES unit, whichever rendition it
	/// belongs to, decodes at or after the clock that precedes it.
	fn emit(&mut self, span: Option<u128>) -> anyhow::Result<()> {
		self.span_counters = None;
		let bytes = std::mem::take(&mut self.pending);
		let keyframes = std::mem::take(&mut self.keyframes);
		let Some(to) = self.low.take() else { return Ok(()) };
		let packets = bytes.len() / TsPacket::SIZE;

		// Transmit from where the last span stopped up to this one's decode time, so
		// the intervals abut and the clock neither repeats nor reverses. The first
		// has nothing to abut, so it takes the span's own measured length.
		//
		// The clock only ever moves forward. A track skewed far enough behind that it
		// decodes before the clock already reached it can't be placed ahead of its own
		// decode time, and `with_latency` owns that skew, so its bytes go out at the
		// clock rather than dragging it backwards.
		let start = match self.clock {
			Some(clock) => clock.as_nanos(),
			None => to.as_nanos().saturating_sub(span.unwrap_or(0)),
		};
		let end = to.as_nanos().max(start);
		let from = stamp(start)?;
		self.clock = Some(stamp(end)?);

		// The slot `start` sits in; its boundary is at or before every byte here.
		let open = start / PCR_INTERVAL.as_nanos();
		// The last boundary strictly inside the interval: `end` opens the next one's.
		let last = slot_before(end, PCR_INTERVAL).max(open);
		// Slots still owed a clock packet. Backfill every missed one so the ramp
		// stays uniform when frames are coarser than the grid, but cap it: past the
		// cap the media itself gapped, and a dense clock history for a span that
		// carried no bytes helps nobody.
		let first = self
			.last_pcr
			.map_or(open, |l| l + 1)
			.max((last + 1).saturating_sub(PCR_BACKFILL));
		// Spread the bytes only across an interval the grid can describe. Past the
		// cap they were muxed before a media gap, so they belong at its start rather
		// than smeared across silence.
		let spread = last - open < PCR_BACKFILL;
		let width = end - start;

		// The first frame opens at `from` rather than a boundary, and carries every
		// clock packet whose slot has already begun.
		let pcr_pid = self.psi.as_ref().context("PSI not built")?.pcr_pid;
		let mut payload = Vec::new();
		for index in first..=open {
			let before = counter_before(&bytes, 0, pcr_pid, self.pcr_cc);
			payload.extend_from_slice(&self.pcr_at(index, before)?);
		}
		let mut cut = 0;
		let mut at = from;

		// One frame per boundary inside the interval. `first` can only exceed
		// `open + 1` when the backfill cap skipped the slots below it, and that cap
		// bounds this range to [`PCR_BACKFILL`] iterations however long the gap was.
		for index in (open + 1).max(first)..=last {
			let boundary = slot_stamp(index)?;
			let next = if spread && width > 0 {
				(packets as u128 * (boundary.as_nanos() - start) / width) as usize
			} else {
				packets
			};
			payload.extend_from_slice(&bytes[cut * TsPacket::SIZE..next * TsPacket::SIZE]);
			self.push(at, payload, &keyframes, cut, next);
			cut = next;
			at = boundary;
			let before = counter_before(&bytes, cut * TsPacket::SIZE, pcr_pid, self.pcr_cc);
			payload = self.pcr_at(index, before)?;
		}

		payload.extend_from_slice(&bytes[cut * TsPacket::SIZE..]);
		self.push(at, payload, &keyframes, cut, packets);
		if let Some(cc) = counter_before(&bytes, bytes.len(), pcr_pid, None) {
			self.pcr_cc = Some(cc);
		}
		Ok(())
	}

	/// Queue one output frame, unless it would be empty. `from`..`to` are the packet
	/// indices it carries, which decide whether a keyframe begins in it.
	fn push(&mut self, timestamp: Timestamp, payload: Vec<u8>, keyframes: &[usize], from: usize, to: usize) {
		if payload.is_empty() {
			return;
		}
		let (from, to) = (from * TsPacket::SIZE, to * TsPacket::SIZE);
		let keyframe = keyframes.iter().any(|&at| at >= from && at < to);
		self.queue.push_back(Frame {
			timestamp,
			duration: None,
			payload: Bytes::from(payload),
			keyframe,
		});
	}

	/// The clock packet for grid slot `index`, and record that the slot is served.
	///
	/// The value backs off by the largest reserve of any track, not just the PCR
	/// track's: every rendition's PES must decode at or after the clock, and each
	/// video track backs its DTS off by its own catalog jitter. Back off through the
	/// 33-bit wrap rather than saturating: a timeline that starts inside the reserve
	/// would otherwise clamp its first slots to zero and break the uniform step. The
	/// wire field is a circular clock, so the masked wrapped value is the correct
	/// mod-2^33 back-off.
	fn pcr_at(&mut self, index: u128, before: Option<u8>) -> anyhow::Result<Vec<u8>> {
		let pcr_pid = self.psi.as_ref().context("PSI not built")?.pcr_pid;
		let reserve = self
			.tracks
			.values()
			.map(|t| t.dts_reserve)
			.max()
			.unwrap_or(DEFAULT_DTS_RESERVE);
		let ticks = slot_ticks(index, PCR_INTERVAL).wrapping_sub(reserve);
		// Nothing has gone out on this PID yet, so there is no counter to repeat and
		// any value starts a valid run; take the one before the next to be used.
		let cc = match before {
			Some(cc) => cc,
			None => self.counters.entry(pcr_pid).or_default().as_u8().wrapping_sub(1),
		};
		self.last_pcr = Some(index);
		let mut packet = pcr_packet(pcr_pid, ticks, cc)?;
		if std::mem::take(&mut self.pcr_discontinuity) {
			packet[5] |= 0x80;
		}
		Ok(packet)
	}

	/// Packetize a PES payload into 188-byte TS packets.
	fn write_pes(&mut self, out: &mut Vec<u8>, unit: &PesUnit, payload: &[u8]) -> anyhow::Result<()> {
		let pts = to_ts_timestamp(unit.timestamp)?;
		// A reordered video frame carries DTS alongside PTS; else PTS-only. The decode clock
		// is continuous ticks, so wrap into the 33-bit wire field here, like the PTS.
		let dts = unit
			.dts
			.map(|t| TsTimestamp::new(t & TS_TIMESTAMP_MASK).map_err(anyhow::Error::msg))
			.transpose()?;
		let stream_id = match unit.stream_id {
			Some(id) => StreamId::new(id),
			None if unit.is_video => StreamId::new(StreamId::VIDEO_MIN),
			None => StreamId::new(StreamId::AUDIO_MIN),
		};
		let header = mpeg2ts::pes::PesHeader {
			stream_id,
			priority: false,
			data_alignment_indicator: true,
			copyright: false,
			original_or_copy: false,
			pts: Some(pts),
			dts,
			escr: None,
		};

		// The optional PES header grows by 5 bytes when it also carries a DTS.
		let optional_len = PES_OPTIONAL_LEN + if dts.is_some() { PES_DTS_LEN } else { 0 };

		// `pes_packet_len` counts the optional header plus the payload (not the
		// 6-byte fixed prefix). Unbounded for video (0); bounded for audio when
		// it fits a u16.
		let pes_packet_len = if unit.is_video {
			0
		} else {
			u16::try_from(optional_len + payload.len()).unwrap_or(0)
		};

		let mut offset = 0;
		let mut first = true;
		loop {
			let adaptation = if first && unit.keyframe {
				Some(AdaptationField {
					discontinuity_indicator: false,
					random_access_indicator: true,
					es_priority_indicator: false,
					pcr: None,
					opcr: None,
					splice_countdown: None,
					transport_private_data: Vec::new(),
					extension: None,
				})
			} else {
				None
			};

			let header_len = if first { 6 + optional_len } else { 0 };
			let af_len = adaptation.as_ref().map(adaptation_size).unwrap_or(0);
			let avail = TsBytes::MAX_SIZE - header_len - af_len;
			let take = avail.min(payload.len() - offset);
			let chunk = &payload[offset..offset + take];

			let ts_payload = if first {
				TsPayload::PesStart(Pes {
					header: header.clone(),
					pes_packet_len,
					data: TsBytes::new(chunk).map_err(anyhow::Error::msg)?,
				})
			} else {
				TsPayload::PesContinuation(TsBytes::new(chunk).map_err(anyhow::Error::msg)?)
			};

			self.write_packet(out, unit.pid, adaptation, ts_payload)?;

			offset += take;
			first = false;
			if offset >= payload.len() {
				break;
			}
		}
		Ok(())
	}

	/// Packetize a private section (SCTE-35 or other) verbatim. The first packet
	/// carries the pointer_field plus the section start as a `Section` payload (sets
	/// the unit-start bit so the receiver finds the pointer_field); continuations are
	/// `Raw`. The section bytes are opaque, so this round-trips byte-for-byte.
	fn write_section(&mut self, out: &mut Vec<u8>, pid: u16, section: &[u8]) -> anyhow::Result<()> {
		// The verbatim track is public; a non-importer producer could publish a frame
		// that isn't a complete section. Drop it (with a warning) rather than emit a
		// malformed section a downstream demuxer would choke on. One bad section must
		// not abort a live export, so this skips instead of erroring.
		if !is_complete_section(section) {
			tracing::warn!(pid, len = section.len(), "dropping malformed private section on export");
			return Ok(());
		}

		let mut offset = 0;
		let mut first = true;
		loop {
			let payload = if first {
				// pointer_field (1 byte, written by `Section`) eats one payload byte.
				let take = (TsBytes::MAX_SIZE - 1).min(section.len());
				let chunk = &section[..take];
				offset = take;
				TsPayload::Section(Section {
					pointer_field: 0,
					data: TsBytes::new(chunk).map_err(anyhow::Error::msg)?,
				})
			} else {
				let take = TsBytes::MAX_SIZE.min(section.len() - offset);
				let chunk = &section[offset..offset + take];
				offset += take;
				TsPayload::Raw(TsBytes::new(chunk).map_err(anyhow::Error::msg)?)
			};

			self.write_packet(out, pid, None, payload)?;
			first = false;
			if offset >= section.len() {
				break;
			}
		}
		Ok(())
	}

	/// Serialize one TS packet (with its continuity counter) into `out`.
	fn write_packet(
		&mut self,
		out: &mut Vec<u8>,
		pid: u16,
		adaptation_field: Option<AdaptationField>,
		payload: TsPayload,
	) -> anyhow::Result<()> {
		let counter = self.counters.entry(pid).or_default();
		let continuity_counter = *counter;
		counter.increment();

		let packet = TsPacket {
			header: TsHeader {
				transport_error_indicator: false,
				transport_priority: false,
				pid: Pid::new(pid)?,
				transport_scrambling_control: TransportScramblingControl::NotScrambled,
				continuity_counter,
			},
			adaptation_field,
			payload: Some(payload),
		};

		let mut writer = TsPacketWriter::new(out);
		writer.write_ts_packet(&packet).map_err(anyhow::Error::msg)?;
		Ok(())
	}
}

/// One adaptation-field-only TS packet carrying `ticks` (continuous 90 kHz) as its
/// PCR, laid out by hand: the `mpeg2ts` serializer writes the six reserved bits
/// between PCR base and extension as zeros where ISO 13818-1 requires ones, and
/// strict analyzers flag that.
///
/// There is no payload, so the field's stuffing fills the packet and the continuity
/// counter is not incremented (ISO 13818-1 2.4.3.3): `cc` is the counter of
/// whatever preceded this packet on the same PID, which it repeats. The clock rides
/// a PID that also carries media, and the packets around it were numbered when they
/// were muxed rather than when they go out, so this has to come from the wire order
/// rather than from the counter's current value.
fn pcr_packet(pid: u16, ticks: u64, cc: u8) -> anyhow::Result<Vec<u8>> {
	anyhow::ensure!(pid <= 0x1FFF, "PID out of range: {pid}");
	let cc = cc & ContinuityCounter::MAX;
	let base = ticks & TS_TIMESTAMP_MASK;
	let mut p = Vec::with_capacity(TsPacket::SIZE);
	p.push(0x47);
	p.push((pid >> 8) as u8);
	p.push(pid as u8);
	// adaptation_field_control = adaptation field only, no scrambling.
	p.push(0x20 | cc);
	// adaptation_field_length covers the rest of the packet.
	p.push(183);
	// PCR_flag alone.
	p.push(0x10);
	// program_clock_reference_base (33 bits), 6 reserved '1' bits, and a zero
	// 9-bit extension (the grid is 90 kHz-exact, so there is nothing sub-tick).
	p.push((base >> 25) as u8);
	p.push((base >> 17) as u8);
	p.push((base >> 9) as u8);
	p.push((base >> 1) as u8);
	p.push(((base as u8) << 7) | 0x7e);
	p.push(0x00);
	p.resize(TsPacket::SIZE, 0xff);
	Ok(p)
}

/// The continuity counter a payload-less packet inserted at byte offset `cut` has to
/// repeat: the last one before it on `pid`, else the one carried in from the last
/// span, else one behind the first packet that follows it, which is what keeps the
/// run continuous where the clock leads the stream and nothing precedes it.
fn counter_before(bytes: &[u8], cut: usize, pid: u16, carried: Option<u8>) -> Option<u8> {
	let on_pid = |p: &&[u8]| (u16::from(p[1] & 0x1f) << 8 | u16::from(p[2])) == pid;
	let counter = |p: &[u8]| p[3] & ContinuityCounter::MAX;
	bytes[..cut]
		.rchunks_exact(TsPacket::SIZE)
		.find(on_pid)
		.map(counter)
		.or(carried)
		.or_else(|| {
			bytes[cut..]
				.chunks_exact(TsPacket::SIZE)
				.find(on_pid)
				.map(|p| counter(p).wrapping_sub(1) & ContinuityCounter::MAX)
		})
}

/// Optional PES header region carrying PTS only: 2 flag bytes + 1 length byte + 5 PTS bytes.
const PES_OPTIONAL_LEN: usize = 3 + 5;
/// Extra bytes when the optional region also carries a DTS (5 DTS bytes).
const PES_DTS_LEN: usize = 5;
/// Fallback decode-clock reserve in 90 kHz ticks when the catalog declares no `jitter`. At
/// 16 ticks (~0.18 ms) it is just a strict-monotonic nudge: it keeps DTS strictly increasing
/// across reordered (B-frame) decode order (the `ffplay -fflags +igndts` fix) but does not
/// keep `DTS <= PTS`. When the catalog carries `jitter` (the reorder depth, populated on
/// import), the track uses that instead, which is large enough to keep `DTS <= PTS`. See
/// [`author_dts`] and [`Track::dts_reserve`].
const DEFAULT_DTS_RESERVE: u64 = 16;

/// Whether `timestamp` has crossed into a later repetition slot than `last`.
///
/// Slots are absolute on the media timeline (`floor(timestamp / interval)`) rather than
/// measured forward from the previous emission, so a table lands on the same frames no
/// matter when the exporter started. Two exporters of one broadcast then emit the tables
/// at the same points, which a redundant pair compares byte for byte. `None` (nothing
/// emitted yet) is always due, so a fresh exporter leads with the tables and a receiver
/// can tune in without waiting for the next slot.
///
/// A *later* slot, not merely a different one: video is emitted in decode order, so a
/// reordered (B-frame) PTS steps backwards all the time, and re-emitting on every
/// oscillation across a boundary buys nothing and costs a table each way.
fn due(timestamp: Timestamp, last: Option<Timestamp>, interval: Duration) -> bool {
	// "Every frame", which the slot arithmetic can't express (and would divide by zero on).
	if interval.is_zero() {
		return true;
	}
	let Some(last) = last else {
		return true;
	};
	slot(timestamp, interval) > slot(last, interval)
}

/// Index of `timestamp`'s repetition slot: how many whole `interval`s fit under it.
///
/// Nanoseconds, so the divisor is zero only for a genuinely zero `interval`, which [`due`]
/// takes before it gets here. Coarser units would floor a sub-unit interval to zero and
/// divide by it.
fn slot(timestamp: Timestamp, interval: Duration) -> u128 {
	Duration::from(timestamp).as_nanos() / interval.as_nanos()
}

/// Index of the last repetition slot to *begin* strictly before `nanos`.
///
/// [`slot`] rounds down, so a position sitting exactly on a boundary belongs to the slot that
/// boundary opens. When the position is the exclusive end of an interval, that slot is the
/// next interval's, not this one's, hence the offset.
fn slot_before(nanos: u128, interval: Duration) -> u128 {
	if nanos == 0 {
		return 0;
	}
	(nanos - 1) / interval.as_nanos()
}

/// A repetition slot's boundary as a media timestamp.
fn slot_stamp(index: u128) -> anyhow::Result<Timestamp> {
	stamp(index * PCR_INTERVAL.as_micros() * 1_000)
}

/// A nanosecond position on the media timeline as a microsecond [`Timestamp`], the
/// scale the exporter stamps its own output in.
fn stamp(nanos: u128) -> anyhow::Result<Timestamp> {
	let micros = (nanos / 1_000).try_into().context("media timeline out of range")?;
	Ok(Timestamp::from_micros(micros)?)
}

/// A repetition slot's boundary (`index * interval`) in 90 kHz ticks.
fn slot_ticks(index: u128, interval: Duration) -> u64 {
	(index * interval.as_nanos() * 90_000 / 1_000_000_000) as u64
}

/// External byte size of an adaptation field (manual mirror of the crate's
/// private `external_size`); only PCR is ever set.
fn adaptation_size(af: &AdaptationField) -> usize {
	2 + if af.pcr.is_some() { 6 } else { 0 }
}

/// The 33-bit wire timestamp field (90 kHz). DTS and PTS both wrap into it.
const TS_TIMESTAMP_MASK: u64 = (1 << 33) - 1;

/// Continuous (unwrapped) 90 kHz tick count for a media timestamp. The decode clock runs in
/// this domain so it never wraps mid-stream (the source timestamps are already unwrapped);
/// [`to_ts_timestamp`] masks to the 33-bit wire field only at emission.
fn to_ticks(timestamp: Timestamp) -> u64 {
	(timestamp.as_micros() * 90_000 / 1_000_000) as u64
}

fn to_ts_timestamp(timestamp: Timestamp) -> anyhow::Result<TsTimestamp> {
	// Continuous 90 kHz ticks, wrapped into the 33-bit field.
	TsTimestamp::new(to_ticks(timestamp) & TS_TIMESTAMP_MASK).map_err(anyhow::Error::msg)
}

fn video_kind(config: &VideoConfig, name: &str) -> anyhow::Result<Kind> {
	ensure_raw(&config.container, "video", name)?;
	// Both in-band (avc3/hev1) and out-of-band (avc1/hvc1) are accepted:
	// ExportSource normalizes both to length-prefixed NALU + avcC/hvcC, and the
	// muxer rewrites them to Annex-B.
	match &config.codec {
		VideoCodec::H264(_) => Ok(Kind::Video(StreamType::H264)),
		VideoCodec::H265(_) => Ok(Kind::Video(StreamType::H265)),
		other => anyhow::bail!("TS export does not support video codec {other:?} (track '{name}')"),
	}
}

/// Build the Annex-B elementary-stream payload for one video frame: rewrite the
/// length-prefixed NALs to start-code-delimited NALs, prepending the parameter
/// sets (SPS/PPS, plus VPS for H.265) from the avcC/hvcC on keyframes so a
/// receiver can tune in mid-stream.
fn video_es_payload(stream_type: StreamType, description: Option<&Bytes>, frame: &Frame) -> anyhow::Result<Vec<u8>> {
	let description = description.context("video codec config (avcC/hvcC) not resolved")?;
	let (length_size, params) = match stream_type {
		StreamType::H264 => crate::codec::h264::avcc_params(description)?,
		StreamType::H265 => crate::codec::h265::hvcc_params(description)?,
		other => anyhow::bail!("unsupported TS video stream type {other:?}"),
	};

	let mut out = Vec::with_capacity(frame.payload.len() + 64);
	if frame.keyframe {
		for nal in &params {
			out.extend_from_slice(&annexb::START_CODE);
			out.extend_from_slice(nal);
		}
	}
	annexb::length_prefixed_to_annexb(&frame.payload, length_size, &mut out)?;
	Ok(out)
}

fn audio_kind(config: &AudioConfig, name: &str) -> anyhow::Result<Kind> {
	ensure_raw(&config.container, "audio", name)?;
	match &config.codec {
		AudioCodec::AAC(aac) => Ok(Kind::Aac {
			object_type: aac.profile,
			sample_rate: config.sample_rate,
			channel_count: config.channel_count,
		}),
		AudioCodec::Mp2 => Ok(Kind::Mp2 {
			sample_rate: config.sample_rate,
		}),
		AudioCodec::Opus => Ok(Kind::Opus {
			channel_count: config.channel_count,
		}),
		AudioCodec::Ac3 => Ok(Kind::Ac3),
		AudioCodec::Ec3 => Ok(Kind::Eac3),
		other => anyhow::bail!("TS export does not support audio codec {other:?} (track '{name}')"),
	}
}

/// The two PMT descriptors for an Opus elementary stream: the `Opus` registration
/// descriptor (which sets the codec) and the DVB extension descriptor 0x80 carrying
/// the channel configuration. ffmpeg's demuxer requires both to recognize the stream.
fn opus_descriptors(channel_count: u32) -> Vec<Descriptor> {
	vec![
		Descriptor {
			tag: 0x05,
			data: b"Opus".to_vec(),
		},
		Descriptor {
			tag: 0x7f,
			// extension_descriptor_tag 0x80, then channel_config_code (1=mono, 2=stereo,
			// = channel count for the Vorbis mapping), clamped to the 1..=8 the demuxer reads.
			data: vec![0x80, channel_count.clamp(1, 8) as u8],
		},
	]
}

/// Wrap a raw Opus packet in the Opus-in-TS access-unit control header, producing one
/// PES access unit. Emits the 11-bit `0x3FF` sync (no trim, no control extension), then
/// the `0xFF`-run `au_size`, then the packet.
fn opus_es_payload(packet: &[u8]) -> Vec<u8> {
	let mut out = Vec::with_capacity(packet.len() + 4);
	// Sync 0x3FF over 11 bits: all of byte 0 (0x7F) plus the top 3 bits of byte 1. The
	// low 5 bits of byte 1 are the start-trim/end-trim/control-extension flags, all clear.
	out.push(0x7f);
	out.push(0xe0);
	// au_size: a run of 0xFF bytes summing toward the size, then a final byte < 0xFF. A
	// size that is an exact multiple of 255 still emits a terminating 0x00 byte.
	let mut n = packet.len();
	loop {
		out.push(n.min(255) as u8);
		if n < 255 {
			break;
		}
		n -= 255;
	}
	out.extend_from_slice(packet);
	out
}

/// The PMT descriptors recorded for `name` in the `mpegts` section, if any.
fn track_descriptors(mpegts: &catalog::Mpegts, name: &str) -> Vec<catalog::Descriptor> {
	mpegts
		.tracks
		.get(name)
		.map(|t| t.descriptors.clone())
		.unwrap_or_default()
}

/// Convert catalog descriptors (base64 bytes) to mpeg2ts PMT descriptors.
fn to_pmt_descriptors(descriptors: &[catalog::Descriptor]) -> Vec<Descriptor> {
	descriptors
		.iter()
		.map(|d| Descriptor {
			tag: d.tag,
			data: d.data.to_vec(),
		})
		.collect()
}

/// One section-framed verbatim frame must be exactly one section: at least the
/// 3-byte header and a total length matching the declared section_length.
/// Structural only (no table semantics); the bytes are still carried verbatim.
fn is_complete_section(section: &[u8]) -> bool {
	section.len() >= 3 && section.len() == 3 + ((((section[1] & 0x0f) as usize) << 8) | section[2] as usize)
}

fn ensure_raw(container: &Container, kind: &str, name: &str) -> anyhow::Result<()> {
	match container {
		// TS carries raw codec payloads, like the Legacy varint and LOC formats.
		Container::Legacy | Container::Loc => Ok(()),
		Container::Cmaf { .. } => anyhow::bail!("TS export does not support CMAF {kind} track '{name}'"),
		Container::Unknown(unknown) => anyhow::bail!(
			"TS export does not support container '{}' on {kind} track '{name}'",
			unknown.kind().unwrap_or("<missing>")
		),
	}
}

/// Author a monotonic decode timestamp (DTS) for a reordered (B-frame) video frame.
///
/// [`Frame`] carries only a presentation timestamp (PTS) and frames reach the muxer in
/// decode order (MoQ groups and frames are delivered in decode order), so a B-frame stream
/// arrives with valid but non-monotonic PTS and no decode time. MPEG-TS players need a
/// monotonic DTS to schedule decoding; without it they choke on the out-of-order PTS (the
/// `ffplay -fflags +igndts` workaround).
///
/// Since decode order is already the delivery order, the only job is to keep DTS strictly
/// increasing. The clock runs [`DTS_RESERVE`] ticks behind the PTS and never goes backwards:
/// a reordered frame whose PTS dips below the clock is nudged one tick past the last DTS. With
/// the small reserve this keeps DTS monotonic but lets it sit above a B-frame's own PTS; a
/// frame-scale reserve (or the faithful wire DTS) would be needed for `DTS <= PTS`.
///
/// `reserve` is how far behind the PTS to run the clock (the catalog reorder depth, or the
/// fallback). `pts` and `last` are continuous (unwrapped) 90 kHz ticks, so the clock never
/// wraps mid-stream; the 33-bit wire wrap happens once at emission in [`write_pes`]. `last` is
/// the previous DTS, updated in place. Returns `None` when the DTS equals the PTS (PES stays
/// PTS-only).
fn author_dts(pts: u64, reserve: u64, last: &mut Option<u64>) -> Option<u64> {
	let mut dts = pts.saturating_sub(reserve);
	if let Some(prev) = *last
		&& dts <= prev
	{
		dts = prev + 1;
	}
	*last = Some(dts);
	(dts != pts).then_some(dts)
}

/// The decode-clock reserve for a video rendition: its catalog `jitter` (the reorder depth)
/// in 90 kHz ticks, or [`DEFAULT_DTS_RESERVE`] when none is declared.
fn dts_reserve(config: &VideoConfig) -> u64 {
	config
		.jitter
		.map(|t| (t.as_micros() * 90_000 / 1_000_000) as u64)
		.filter(|&ticks| ticks > 0)
		.unwrap_or(DEFAULT_DTS_RESERVE)
}

#[cfg(test)]
mod tests {
	use std::time::Duration;

	use super::{
		DEFAULT_DTS_RESERVE, PCR_INTERVAL, PSI_INTERVAL, author_dts, due, is_complete_section, slot, slot_ticks,
	};
	use moq_net::Timestamp;

	fn ms(value: u64) -> Timestamp {
		Timestamp::from_millis(value).unwrap()
	}

	/// Push a decode-order PTS stream (90 kHz) through the decode clock with a given reserve and
	/// return the effective DTS per frame (the authored DTS, or the PTS when none is authored).
	fn run_clock(pts: &[u64], reserve: u64) -> Vec<u64> {
		let mut last = None;
		pts.iter()
			.map(|&p| author_dts(p, reserve, &mut last).unwrap_or(p))
			.collect()
	}

	/// Decode-order PTS for a constant-frame-rate display timeline with `b` B-frames between
	/// each pair of reference frames (the common broadcast structure: references pulled ahead
	/// of the B-frames they predict). `base` keeps the timeline off zero, like a real feed's
	/// initial PTS offset.
	fn decode_order(refs: usize, b: usize, dur: u64, base: u64) -> Vec<u64> {
		let pts = |display: usize| base + display as u64 * dur;
		let span = b + 1;
		let mut out = vec![pts(0)]; // first reference (keyframe) at display 0
		for g in 1..refs {
			let reference = g * span;
			out.push(pts(reference)); // reference, decoded before its B-frames
			for j in 1..=b {
				out.push(pts(reference - span + j)); // the B-frames between the two references
			}
		}
		out
	}

	#[test]
	fn dts_is_monotonic_across_reorder() {
		// 25 fps, 10 s offset. Even with the tiny fallback reserve the decode timeline is
		// strictly increasing (the `+igndts` fix); it just may sit above PTS for B-frames.
		for b in [1, 3, 5] {
			let pts = decode_order(40, b, 3_600, 10_000_000);
			let dts = run_clock(&pts, DEFAULT_DTS_RESERVE);

			// The fixture genuinely reorders (PTS dips in decode order).
			assert!(pts.windows(2).any(|w| w[1] < w[0]), "b={b}: stream must reorder PTS");
			for (i, win) in dts.windows(2).enumerate() {
				assert!(win[1] > win[0], "b={b}: DTS not strictly increasing at {i}: {win:?}");
			}
		}
	}

	#[test]
	fn sufficient_reserve_keeps_dts_under_pts() {
		// With a reserve covering the reorder span (the catalog `jitter` carries it), the decode
		// timeline is both strictly increasing and never after the PTS.
		let dur = 3_600;
		for b in [1, 3, 5] {
			let reserve = (b as u64 + 1) * dur; // one frame past the b-frame run
			let pts = decode_order(40, b, dur, 10_000_000);
			let dts = run_clock(&pts, reserve);

			for (i, win) in dts.windows(2).enumerate() {
				assert!(win[1] > win[0], "b={b}: DTS not strictly increasing at {i}: {win:?}");
			}
			for (i, (&d, &p)) in dts.iter().zip(pts.iter()).enumerate() {
				assert!(d <= p, "b={b}: DTS {d} after PTS {p} at {i}");
			}
		}
	}

	#[test]
	fn dts_clock_survives_33bit_wrap() {
		// The decode clock runs in continuous ticks, so it stays strictly increasing even as
		// the source timeline crosses the 33-bit wire boundary (~26.5 h). The wrap is applied
		// only at emission, so here the authored DTS keeps climbing past 1 << 33.
		let wrap = 1u64 << 33;
		let pts = decode_order(40, 3, 3_600, wrap - 20 * 3_600);
		let dts = run_clock(&pts, DEFAULT_DTS_RESERVE);

		assert!(pts.iter().any(|&p| p >= wrap), "test must cross the wrap boundary");
		for (i, win) in dts.windows(2).enumerate() {
			assert!(
				win[1] > win[0],
				"DTS not strictly increasing across wrap at {i}: {win:?}"
			);
		}
	}

	#[test]
	fn dts_without_reorder_trails_pts_by_the_reserve() {
		// A monotonic (no-B) stream stays strictly increasing and one reserve under its PTS.
		let pts: Vec<u64> = (0..40).map(|i| 10_000_000 + i * 3_600).collect();
		let dts = run_clock(&pts, DEFAULT_DTS_RESERVE);

		for (i, win) in dts.windows(2).enumerate() {
			assert!(win[1] > win[0], "DTS not strictly increasing at {i}: {win:?}");
		}
		for (i, (&d, &p)) in dts.iter().zip(pts.iter()).enumerate() {
			assert_eq!(d, p - DEFAULT_DTS_RESERVE, "DTS should trail PTS by the reserve at {i}");
		}
	}

	#[test]
	fn author_dts_is_join_independent_at_a_peak() {
		// An exporter that has been running and one that just joined author the same decode
		// timeline from any frame whose PTS leads everything decoded before it. A keyframe is
		// exactly that (export only ever tunes in on one), so the monotonic bump cannot fire
		// there and the state carried across the join stops mattering.
		for b in [1, 3, 5] {
			let pts = decode_order(40, b, 3_600, 10_000_000);
			let running = run_clock(&pts, DEFAULT_DTS_RESERVE);

			let mut peaks = 0;
			for k in 1..pts.len() {
				if pts[..k].iter().any(|&p| p >= pts[k]) {
					continue;
				}
				peaks += 1;
				let fresh = run_clock(&pts[k..], DEFAULT_DTS_RESERVE);
				assert_eq!(
					&running[k..],
					&fresh[..],
					"b={b}: joining at {k} authored a different clock"
				);
			}
			assert!(peaks > 10, "b={b}: fixture must have peaks to join at, got {peaks}");
		}
	}

	#[test]
	fn due_crosses_an_absolute_slot() {
		assert!(due(ms(1_000), None, PSI_INTERVAL));
		assert!(!due(ms(1_250), Some(ms(1_000)), PSI_INTERVAL));
		assert!(due(ms(1_500), Some(ms(1_000)), PSI_INTERVAL));

		// A backwards timestamp is a slot already served, not a new one. Emitting there would
		// fire on every B-frame that steps back across a boundary (see `due_ignores_reorder`).
		assert!(!due(ms(750), Some(ms(1_000)), PSI_INTERVAL));

		// A per-PID SI interval is honored independently of the PSI cadence: an SDT at
		// 2s is not due when the 500ms PSI would be.
		let sdt = Duration::from_millis(2_000);
		assert!(!due(ms(1_500), Some(ms(1_000)), sdt));
		assert!(due(ms(3_000), Some(ms(1_000)), sdt));
	}

	/// Drive a run of timestamps through the interval cadence, advancing the stored emission
	/// only when one fires, and return how many tables it emitted. That is the whole of the SI
	/// path; PSI additionally emits (and re-anchors) at every video keyframe, which is what
	/// keeps two exporters of a program *with* video in step even before this.
	fn run_cadence(stamps: &[Timestamp], interval: Duration) -> usize {
		let mut last = None;
		let mut emissions = 0;
		for &ts in stamps {
			if due(ts, last, interval) {
				emissions += 1;
				last = Some(ts);
			}
		}
		emissions
	}

	#[test]
	fn due_ignores_reorder() {
		// Video is emitted in decode order, so a B-frame stream steps its PTS backwards
		// constantly (measured at 39% of frames on real contribution content). The cadence has
		// to follow the slots the stream has *reached*, not fire on every crossing of one, or
		// each oscillation across a boundary re-sends the tables both ways.
		let ticks = decode_order(40, 3, 3_600, 10_000_000);
		let stamps: Vec<Timestamp> = ticks
			.iter()
			.map(|&t| Timestamp::from_micros(t * 1_000_000 / 90_000).unwrap())
			.collect();
		assert!(stamps.windows(2).any(|w| w[1] < w[0]), "fixture must reorder its PTS");

		// One table per slot the stream covers, however often the reorder revisits a boundary.
		let first = slot(stamps[0], PSI_INTERVAL);
		let last = slot(*stamps.iter().max().unwrap(), PSI_INTERVAL);
		assert_eq!(run_cadence(&stamps, PSI_INTERVAL), (last - first + 1) as usize);
	}

	#[test]
	fn due_zero_interval_emits_every_frame() {
		// A catalog is free to ask for a table on every frame. Slot arithmetic can't express
		// that (and would divide by zero), so it is handled before the division. Repeated
		// timestamps are the case a slot count gets wrong: two tracks can share one.
		let stamps: Vec<Timestamp> = [0, 0, 40, 40, 80].iter().map(|&t| ms(t)).collect();
		assert_eq!(run_cadence(&stamps, Duration::ZERO), stamps.len());
	}

	#[test]
	fn due_survives_a_sub_microsecond_interval() {
		// Only an exactly-zero interval short-circuits, so the slot divisor has to stay
		// non-zero for every other duration. Nanoseconds do; anything coarser floors a
		// sub-unit interval to zero and panics on the division.
		let interval = Duration::from_nanos(500);
		assert!(
			!interval.is_zero() && interval.as_micros() == 0,
			"fixture must be sub-microsecond"
		);
		assert!(due(ms(1), Some(ms(0)), interval));
		assert!(!due(ms(0), Some(ms(0)), interval));
	}

	#[test]
	fn due_ignores_when_the_last_emission_landed_in_its_slot() {
		// The whole point of the absolute grid: two exporters that emitted at different
		// points *within* the same slot agree on every later frame, so the emission points
		// belong to the broadcast rather than to whoever started when.
		for last in [1_000, 1_100, 1_499] {
			assert!(!due(ms(1_499), Some(ms(last)), PSI_INTERVAL), "last={last}");
			assert!(due(ms(1_500), Some(ms(last)), PSI_INTERVAL), "last={last}");
		}
	}

	#[test]
	fn pcr_slots_are_exact_ticks() {
		// 25 ms is exactly 2250 ticks at 90 kHz: consecutive slot boundaries differ by
		// exactly one grid step with no rounding drift, however far the timeline runs.
		// That is what makes the emitted PCR intervals uniform rather than merely bounded.
		let step = slot_ticks(1, PCR_INTERVAL);
		assert_eq!(step, 2250);
		for index in [0u128, 1, 7, 1_000_000, u32::MAX as u128] {
			assert_eq!(slot_ticks(index, PCR_INTERVAL), index as u64 * step);
		}
	}

	#[test]
	fn section_validation() {
		// section_length 27 (0x1b) -> 30 bytes total.
		let mut ok = vec![0xfc, 0x30, 0x1b];
		ok.resize(30, 0x00);
		assert!(is_complete_section(&ok));
		// minimal: section_length 0 -> exactly the 3-byte header.
		assert!(is_complete_section(&[0xfc, 0x00, 0x00]));
		// any table_id is accepted (verbatim carriage isn't SCTE-specific).
		assert!(is_complete_section(&[0x00, 0x00, 0x00]));

		// shorter than the 3-byte header.
		assert!(!is_complete_section(&[0xfc, 0x00]));
		// declared section_length (27) does not match the actual length (3).
		assert!(!is_complete_section(&[0xfc, 0x30, 0x1b]));
	}
}
