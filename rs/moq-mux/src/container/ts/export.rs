//! MPEG-TS muxer.
//!
//! [`Export`] subscribes to a MoQ broadcast and produces MPEG-TS: PAT/PMT program
//! tables and PES packets, packetized into 188-byte TS packets, with a PCR on a
//! fixed media-time grid. Output is sliced on that grid rather than per media
//! frame (the schedule): each [`Frame`] is one slot, opened by its PCR, plus the
//! bytes belonging to it. At a multiplex rate the PCR rides the adaptation field
//! of the clock PID's packet when one can open the slot; otherwise it takes an
//! adaptation-field-only packet of its own. Each frame is stamped at the slot
//! boundary, so the clock a receiver recovers from byte
//! position agrees with the values, and a pacing caller releases each slot at
//! the instant it asserts. Video is carried as Annex-B, audio as ADTS AAC.
//!
//! Every frame is muxed a fixed delay after its release time, its decode time less its
//! send-ahead ([`Export::with_delay`]), so tracks interleave in one order whatever the
//! arrival skew between them, and the output keeps the source's pace.
//!
//! Video flows through `ExportSource`, which normalizes every H.264/H.265
//! source to length-prefixed NALU plus a resolved avcC/hvcC (parsing in-band
//! avc3/hev1 parameter sets out of the bitstream, or taking the catalog
//! `description` for out-of-band avc1/hvc1). The muxer then does one
//! length-prefixed -> Annex-B conversion, re-injecting the parameter sets as
//! inline NALs on every keyframe. CMAF tracks are rejected with a clear error.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::task::Poll;
use std::time::Duration;

use anyhow::Context;
use bytes::Bytes;
use hang::catalog::{AudioCodec, AudioConfig, Container, VideoCodec, VideoConfig};
use mpeg2ts::es::StreamId;
use mpeg2ts::es::StreamType;
use mpeg2ts::time::Timestamp as TsTimestamp;
use mpeg2ts::ts::payload::{Bytes as TsBytes, Pat, Pes, Section};
use mpeg2ts::ts::{
	AdaptationField, ContinuityCounter, Pid, ProgramAssociation, TransportScramblingControl, TsHeader, TsPacket,
	TsPacketWriter, TsPayload, VersionNumber, WriteTsPacket,
};

use moq_net::Timestamp;
use rustc_hash::FxHashMap;

use super::catalog::Catalog as _;
use crate::catalog::Stream;
use crate::catalog::hang::Catalog;
use crate::codec::annexb::InBandCodec;
use crate::codec::video::{Hrd, Reorder};
use crate::codec::{aac, annexb, opus};
use crate::container::consumer::Event;
use crate::container::declared::DeclaredClock;
use crate::container::decode::{DecodeClock, MAX_REORDER, Settle};
use crate::container::{ExportSource, Frame};
use crate::jitter::{self, Arrival, Push};

use super::adts;
use super::catalog;
use super::psi;
use super::schedule::{self, Buffer, Schedule};

/// PID of the single program's PMT.
const PMT_PID: u16 = 0x1000;
/// First elementary-stream PID; each track gets the next one.
const FIRST_ES_PID: u16 = 0x1001;
/// Re-emit PAT/PMT at least this often (wall-clock of the media) for tune-in.
const PSI_INTERVAL: Duration = Duration::from_millis(500);
/// The floor between emissions of a *changed* SI snapshot, bounding how fast a
/// revising publisher can make the mux re-emit its tables. The import side
/// debounces its own snapshot cuts (`si::DEBOUNCE`), but export consumes any
/// catalog-named snapshot track, so the bound cannot live only there; this one is
/// enforced on the media timeline. Clamped to the entry's own interval, so a
/// table asking for faster repetition than this still gets it.
const SI_REVISION_INTERVAL: Duration = Duration::from_secs(1);
/// Emit a PCR on every crossing of this media-time grid ([`Schedule`]).
/// TR 101 290 V1.4.1 flags a gap over 100 ms; broadcast muxes emit every 25-40 ms.
pub(super) const PCR_INTERVAL: Duration = Duration::from_millis(25);
/// A null packet: PID 0x1FFF, payload only, all stuffing. Its continuity counter
/// is don't-care (ISO 13818-1), so one template serves every one.
pub(super) const NULL_PACKET: [u8; TsPacket::SIZE] = {
	let mut packet = [0xff; TsPacket::SIZE];
	packet[0] = 0x47;
	packet[1] = 0x1f;
	packet[2] = 0xff;
	packet[3] = 0x10;
	packet
};
/// Upper bound on an accepted multiplex rate, in bits per second: far above any
/// broadcast contribution multiplex, while bounding one slot's null allocation
/// to a few megabytes. Zero and anything past it are refused where they enter,
/// leaving the output unpadded.
const MAX_MUX_RATE: u64 = 1_000_000_000;

/// A multiplex rate from the builder override or the (untrusted) catalog is only
/// worth padding to when it is a real rate: zero pads nothing, and anything past
/// [`MAX_MUX_RATE`] would allocate unbounded nulls per slot. Invalid rates are
/// refused, leaving the output unpadded.
fn sanitize_mux_rate(rate: u64) -> Option<u64> {
	(1..=MAX_MUX_RATE).contains(&rate).then_some(rate)
}

/// Subscribe to a broadcast and produce an MPEG-TS byte stream.
///
/// Use [`next`](Self::next) to pull one [`Frame`] per PCR grid slot: its `payload`
/// is the TS packets belonging to that slot, stamped at the slot's boundary. The
/// leading PAT/PMT rides on the first frame (so it inherits a real timestamp), and
/// is re-emitted at video keyframes and periodically for mid-stream tune-in. SI and
/// whole PES or sections wait until a PCR interval has followed it, since a reader
/// places them on the program clock and the PMT is what names it.
/// Returns `None` when the broadcast ends. A publisher that leaves instead fails the
/// export with its cause once what was already buffered has gone out on schedule.
///
/// A rendition the catalog moves to another track (a new `track` or `broadcast`) goes on out
/// of its PID from the new track's first sync frame past what was read, when the PMT can
/// announce it unchanged; one that goes back in time fails the export.
pub struct Export<S: Stream>
where
	S::Ext: catalog::Catalog,
{
	program: Program<S>,
	jitter: jitter::Buffer<TrackKey, Queued>,
	held: Option<jitter::Ready<TrackKey, Queued>>,
	pending: Option<Box<Program<S>>>,
	next_program: u64,
	active_program: u64,
	follow_until: Option<moq_clock::Instant>,
	follow_timer: Option<std::pin::Pin<Box<moq_clock::Sleep>>>,
}

/// Input and mux state belonging to one publisher instance. The release clock
/// stays on the export so a replacement cannot discard the program still playing.
struct Program<S: Stream>
where
	S::Ext: catalog::Catalog,
{
	id: u64,
	retired: bool,
	/// Why the publisher left, reported once the output it buffered has gone out.
	left: Option<crate::Error>,
	expected: Vec<TrackKey>,
	expected_changed: bool,
	source: crate::Source,
	catalog: Option<S>,
	/// Keep the concrete follower's format after EOF without polling an ended stream again.
	catalog_ended: bool,
	/// A snapshot arrived on `catalog` since it was subscribed.
	cataloged: bool,
	/// The publisher epoch of the broadcast being exported, `None` for an epochless route.
	instance: Option<moq_net::Epoch>,
	/// Tracks [`Export::follow`] left on the ended broadcast, resubscribed as the returned
	/// catalog lists them.
	stale: HashSet<String>,
	/// The PAT and PMT `version_number` the program's first tables go out at, advanced by each
	/// switch to another instance so a demux that caches tables by version reads the new ones.
	version: VersionNumber,
	/// After a switch to another instance, the PIDs whose first packet is queued flagging the
	/// break. `None` for an export that never switched.
	flagged: Option<HashSet<u16>>,
	/// How long after its decode time each frame goes out.
	delay: Duration,
	/// Jitter-buffer generation of the program being muxed; a newer one rewinds it.
	generation: u64,

	tracks: HashMap<String, Track>,
	/// SI muxed with frames still packed, which rides ahead of the next unit queued, so it
	/// goes out in the order it was written.
	tables: Vec<u8>,
	/// The program tables are due ahead of the next unit queued.
	psi_due: bool,
	/// The program tables changed, so the next frame muxed is a tune-in point that leads with them.
	revised: bool,
	/// The next continuity counter per PID, numbered as packets go out.
	counters: FxHashMap<u16, u8>,
	/// PMT program-level descriptors captured on import, re-emitted in the PMT.
	program_descriptors: Vec<catalog::Descriptor>,
	/// Transport/service identity captured on import, used to rebuild a consistent
	/// PAT/PMT. `None` for a media-only source, so a minimal identity is synthesized.
	program: Option<catalog::Program>,
	/// Standalone SI subscriptions, keyed by `(PID, table_id)` from the catalog's
	/// `mpegts.si` map. Each reduces its snapshot track into the section set
	/// re-emitted verbatim on its PID at its own cadence. Opaque: export never
	/// parses a table it carries.
	si: BTreeMap<(u16, u8), SiTrack>,
	/// Timestamp of the last emitted frame, stamped onto the trailing SI flush.
	last_timestamp: Option<Timestamp>,
	/// The trailing SI flush ran (once, just before end of stream).
	si_flushed: bool,

	/// Program tables, built once the track layout is known and revised as it changes.
	psi: Option<Psi>,
	/// Media timestamp of the last PAT/PMT emission ([`due`]).
	last_psi: Option<Timestamp>,
	/// Program generation being muxed; source counters are local to each rendition.
	epoch: u64,
	/// Generation of the last returned frame, updated only at the output boundary.
	emitted_epoch: u64,
	pcr_discontinuity: bool,
	/// Lays the muxed packets onto the PCR grid.
	schedule: Schedule,
	/// Read each track from the oldest group the delay still reaches, rather than the live
	/// edge: a test exporting a broadcast it wrote whole.
	replay: bool,
	/// Wakes the export when the next grid slot is due on the jitter buffer's clock.
	slot_timer: Option<std::pin::Pin<Box<moq_clock::Sleep>>>,
	/// Output frames ready to hand out, one per grid slot, each with what it tells
	/// [`Export::stats`] once returned.
	queue: VecDeque<(Frame, Tally)>,
	/// The rate to pad the output to with null packets, in bits per second: the
	/// builder override when set, else the catalog's recorded multiplex rate, else
	/// none and the output is unpadded ([`Schedule`]).
	mux_rate: Option<u64>,
	mux_rate_override: Option<u64>,
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
	/// The jitter buffer key of the first video frame out since `video_start` was set.
	/// Until one has gone, what the other tracks release is dropped: a video track whose
	/// start group went stale before it went out starts at its next keyframe instead
	/// ([`jitter::Buffer`]).
	video_out: Option<Timestamp>,
	/// Frames dropped before the jitter buffer: ahead of the first video keyframe, or of their
	/// codec config.
	trimmed: u64,
	/// Each elementary stream's access units and silence on the PCR returned ([`Export::stats`]).
	liveness: super::import::Liveness,
}
/// What a queued output frame tells [`Export::stats`], applied only once the frame is
/// returned: a rewind drops the queue unwritten.
struct Tally {
	/// The frame's PCR in 27 MHz ticks, and whether it flags a new time base.
	pcr: (u64, bool),
	/// The PID of each access unit whose packets begin in the frame.
	units: Vec<u16>,
	/// The last program tables to finish in the frame.
	tables: Option<Laid>,
}

/// A video frame waiting on a decode clock, with the codec config it was read under.
type Held = (Pending, Option<Bytes>);

/// A frame read from its source.
struct Pending {
	frame: Frame,
	/// How many marker groups had declared a break in the track's timeline when the frame
	/// was read.
	restart: u64,
	/// How many times its playhead had jumped, markers included ([`jitter::Arrival::skip`]).
	skip: u64,
	/// How many of those jumps cut a group short ([`ExportSource::cuts`]).
	cut: u64,
	/// When it landed in this node's cache ([`moq_net::frame::Frame::arrived`]), so a stall
	/// that holds the export back too does not hide that it came late.
	arrived: moq_clock::Instant,
	/// When it was read.
	read: moq_clock::Instant,
}

/// A frame waiting in the jitter buffer, with what it needs from the moment it was read.
struct Queued {
	frame: Frame,
	/// When the frame decodes: its DTS, else its PTS.
	decode: Timestamp,
	/// Where the jitter buffer orders it ([`Track::release_key`]).
	key: Timestamp,
	/// Authored decode timestamp, as [`PesUnit::dts`].
	dts: Option<u64>,
	/// The avcC/hvcC the frame was read under, for video.
	description: Option<Bytes>,
	/// The PES it may share ([`Track::pack_start`]).
	pes: Option<PesStart>,
}

/// The PES an audio frame may share ([`Track::pack_start`]): the place of its first frame on
/// the media timeline, and its [`Track::psi_slot`].
type PesStart = (u128, Option<u128>);

#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
struct TrackKey {
	program: u64,
	name: String,
}
impl TrackKey {
	fn new(program: u64, name: &str) -> Self {
		Self {
			program,
			name: name.to_string(),
		}
	}
}

struct Track {
	key: TrackKey,
	source: ExportSource,
	/// The first frame, held until the program tables are built.
	pending: Option<Pending>,
	/// When the source was subscribed. Nothing it delivers reached the export before then,
	/// however long the cache held it.
	subscribed: moq_clock::Instant,
	/// The latest frame arrival or observed loss, which settles frames held for their DTS.
	latest: moq_clock::Instant,
	/// The marker count the decode clock runs under.
	restart: u64,
	/// The marker, skip and cut counts of the sources the track read before this one, so the
	/// counts keep rising across a resubscription.
	base: (u64, u64, u64),
	finished: bool,
	pid: u16,
	/// The PMT lists it: from its first frame out until its last has gone and the catalog has
	/// dropped it. Its PID, kind and descriptors stay as listed.
	listed: bool,
	/// The catalog lists it.
	cataloged: bool,
	/// Read no further because the catalog withdrew it before the PMT listed it. A return
	/// under the same name reads it on.
	withdrawn: bool,
	/// Frames released while its PID still carries the stream before it, which go out once
	/// that stream's last has.
	parked: VecDeque<Queued>,
	kind: Kind,
	/// PMT ES-level descriptors to re-announce, captured verbatim on import (language,
	/// registration, ...). Empty for non-TS sources; AC-3/E-AC-3 then synthesize one.
	descriptors: Vec<catalog::Descriptor>,
	/// Authors each video frame's DTS, holding the frames until it can, for a rendition that
	/// declares no `reorder`.
	clock: DecodeClock<Held>,
	/// The current group's clock, when its rendition declared `reorder` at the group's start.
	/// A trimmed leading picture holds its slot as `None`.
	declared: Option<DeclaredClock<Option<Held>>>,
	/// Loss counter at the last read, excluding declared ends.
	recovery_gap: Option<u64>,
	/// Leading pictures below the recovery keyframe have unavailable references, and whether
	/// the track queued frames ahead of that keyframe, so dropping them leaves a hole.
	leading_floor: Option<(Timestamp, bool)>,
	/// The source's cut count at the last frame queued. A cut since clears what a track that
	/// has not started holds.
	held: Option<u64>,
	/// The source ended a group, cleanly or cut. The next group presents after every picture
	/// of this one, its leading pictures included, so the end settles each DTS the clock holds:
	/// waiting on the next group instead would make what this one delivered late behind a hole
	/// upstream. Settled once the source has nothing more right now, so a track's end, read
	/// with its last group's, still takes the end of the stream's settlement.
	settle: bool,
	/// The PTS of the last keyframe queued, which leading pictures present before.
	keyframe: Option<Timestamp>,
	/// What the catalog and the SPS declare about the video's reordering.
	timing: Timing,
	/// The receiver's buffers for an audio or verbatim track; video's come from [`Timing`].
	buffer: Option<Buffer>,
	/// The audio frames muxed into a PES that has yet to go out.
	pack: Option<Pack>,
	/// The last frame's decode time, its jitter buffer key ([`Self::release_key`]) and the PES
	/// it may share.
	released: Option<(Timestamp, Timestamp, Option<PesStart>)>,
}

/// Shared program state needed while reading one track.
struct Input<'a> {
	waiting_for_header: bool,
	video_start: Option<Timestamp>,
	jitter: &'a mut jitter::Buffer<TrackKey, Queued>,
	/// Counts the frames dropped ahead of the tune-in point or their codec config.
	trimmed: &'a mut u64,
}

impl Track {
	/// A track reading `source`, its PID filled in by the catalog.
	fn new(source: ExportSource, kind: Kind) -> Self {
		Self {
			key: TrackKey::default(),
			source,
			pending: None,
			subscribed: moq_clock::now(),
			latest: moq_clock::Instant::ZERO,
			restart: 0,
			base: (0, 0, 0),
			finished: false,
			pid: 0,
			listed: false,
			cataloged: true,
			withdrawn: false,
			parked: VecDeque::new(),
			kind,
			descriptors: Vec::new(),
			clock: DecodeClock::default(),
			declared: None,
			recovery_gap: None,
			leading_floor: None,
			held: None,
			settle: false,
			keyframe: None,
			timing: Timing::default(),
			buffer: None,
			pack: None,
			released: None,
		}
	}

	/// Queue a frame read from the source in the jitter buffer, once its decode time is known.
	///
	/// The decode clock runs here, as frames are read, because the deadline is on decode time:
	/// reordered video arrives in decode order with PTS 0, 120, 40, 80, so a PTS deadline would
	/// strand a B-frame behind its reference or send it first.
	fn queue(
		&mut self,
		name: &str,
		pending: Pending,
		jitter: &mut jitter::Buffer<TrackKey, Queued>,
	) -> anyhow::Result<()> {
		if pending.restart != self.restart {
			// A marker declared a break in the timeline, so the decode clock restarts too.
			self.release(name, jitter, true)?;
			self.restart = pending.restart;
			self.clock = DecodeClock::default();
			self.declared = None;
		}
		let Kind::Video(stream_type) = self.kind else {
			return self.push(name, pending, None, None, jitter);
		};
		// A declared endpoint increments both counters; only delivery loss arms recovery.
		let gap = pending.skip.saturating_sub(pending.restart);
		let recover = self.recovery_gap.replace(gap).is_none_or(|previous| previous != gap);
		if pending.frame.keyframe {
			let ahead = self.held == Some(pending.cut);
			self.leading_floor = recover.then_some((pending.frame.timestamp, ahead));
		}
		if let Some((floor, ahead)) = self.leading_floor
			&& pending.frame.timestamp < floor
			&& !radl(stream_type, self.source.description(), &pending.frame)
		{
			jitter.lose(&self.key, ahead);
			// It still takes its slot, so the group's other frames decode where they do on an
			// exporter that kept it.
			return match &mut self.declared {
				Some(clock) => {
					clock.push(None, to_ticks(pending.frame.timestamp))?;
					self.release(name, jitter, false)
				}
				None => Ok(()),
			};
		}
		self.held = Some(pending.cut);
		if pending.frame.keyframe {
			self.timing.describe(stream_type, self.source.description(), name);
			// A group decodes on its own clock at the depth its rendition declares now. The
			// inferred clock spans groups, so it is only flushed when a declaration replaces it.
			let depth = self.timing.reorder;
			if self.declared.is_some() || depth.is_some() {
				self.release(name, jitter, true)?;
			}
			self.declared = depth.map(DeclaredClock::new);
		}
		let pts = to_ticks(pending.frame.timestamp);
		let arrived = pending.arrived;
		let item = (pending, self.source.description().cloned());
		match &mut self.declared {
			Some(clock) => clock.push(Some(item), pts)?,
			None => self.clock.push(item, pts, arrived),
		}
		self.release(name, jitter, false)
	}

	/// Read until a frame is due, or hold the first frame until the program tables are built.
	fn fill(&mut self, name: &str, waiter: &kio::Waiter, input: &mut Input<'_>) -> crate::Result<()> {
		if self.key.name.is_empty() {
			self.key.name = name.to_string();
		}
		if self.pending.is_some() || self.finished {
			return Ok(());
		}
		let is_video = matches!(self.kind, Kind::Video(_));
		// A placed frame is judged at release, against the keyframe's departure ([`Export::leads_video`]).
		let aligned = !is_video && !self.kind.placed();
		loop {
			if !input.waiting_for_header && input.jitter.full(&self.key) {
				break;
			}
			match self.source.poll_event(waiter)? {
				Poll::Ready(Some(Event::Frame { frame, arrived })) => {
					let arrived = arrived.max(self.subscribed);
					// The frame settles what its group's end left held, so they are ready as it lands.
					self.latest = self.latest.max(arrived);
					if std::mem::take(&mut self.settle) && !input.waiting_for_header {
						self.release(name, input.jitter, true)?;
					}
					// A track waits for its codec config before the PMT can list it.
					if (input.waiting_for_header || !self.listed) && !self.source.header_ready() {
						*input.trimmed += 1;
						continue;
					}
					let pending = Pending {
						frame,
						restart: self.base.0 + self.source.markers(),
						skip: self.base.1 + self.source.skips(),
						cut: self.base.2 + self.source.cuts(),
						arrived,
						read: moq_clock::now(),
					};
					// A new timeline must reach the reset before tune-in alignment can drop it.
					if let Some(start) = input.video_start
						&& aligned && pending.restart == self.restart
						&& pending.frame.timestamp < start
					{
						*input.trimmed += 1;
						continue;
					}
					if input.waiting_for_header {
						self.pending = Some(pending);
						break;
					}
					self.queue(name, pending, input.jitter)?;
				}
				// A cut settles what the decode clock holds of the group it left short now, not with
				// the next group, which may arrive after those frames were due, or with the stream's end.
				Poll::Ready(Some(Event::Cut { observed })) => {
					self.latest = self.latest.max(observed);
					self.settle = input.waiting_for_header;
					if !input.waiting_for_header {
						self.release(name, input.jitter, true)?;
					}
				}
				Poll::Ready(Some(Event::GroupEnd)) => self.settle = true,
				Poll::Ready(Some(Event::FrameEnd { .. })) => {}
				Poll::Ready(None) => {
					self.finished = true;
					break;
				}
				Poll::Pending => {
					if std::mem::take(&mut self.settle) && !input.waiting_for_header {
						self.release(name, input.jitter, true)?;
					}
					break;
				}
			}
		}
		// Nothing more can come in below the frames still held for their DTS.
		if self.finished && !input.waiting_for_header {
			self.release(name, input.jitter, true)?;
		}
		Ok(())
	}

	/// The samples an audio frame of `payload` lasts and their rate, if it may share a PES with
	/// the frames around it: one whole frame of audio whose receiver removes each frame from
	/// its buffer at the frame's own decode time.
	fn samples(&self, payload: &[u8]) -> Option<(u64, u32)> {
		let parse = match &self.kind {
			Kind::Aac { config, .. } => return (config.sample_rate > 0).then_some((1024, config.sample_rate)),
			Kind::Mp2 { .. } => crate::codec::mp2::parse_header,
			Kind::Ac3 => crate::codec::ac3::parse_header,
			Kind::Eac3 => crate::codec::eac3::parse_header,
			_ => return None,
		};
		let header = parse(payload).ok()?;
		(header.len == payload.len() && header.sample_rate > 0 && header.samples > 0)
			.then_some((header.samples, header.sample_rate))
	}

	/// The receiver's buffers for the track's PID, if the T-STD gives it any.
	fn buffer(&self) -> Option<Buffer> {
		match self.kind {
			Kind::Video(_) => self.timing.buffer(),
			_ => self.buffer,
		}
	}

	/// The bytes a PES of the track has for its frames, and how many frames of `len` bytes
	/// that holds: B holds the PES header with the frames (ISO 13818-1 2.4.2.3).
	fn fit(&self, len: usize) -> (usize, u128) {
		let size = self
			.buffer()
			.and_then(|buffer| buffer.size)
			.unwrap_or(0)
			.saturating_sub(PES_PREFIX_LEN + PES_OPTIONAL_LEN);
		(size, (size / len.max(1)).max(1) as u128)
	}

	/// The PSI repetition slot a PES of the track keeps to, if any. AAC with a program config
	/// element repeats it in the first frame it muxes after the tables, which go out on
	/// [`due`]'s grid; a PES that spanned a boundary of the grid could be muxed after the tables
	/// and still go out ahead of them, where a receiver tuning in at the tables misses it.
	fn psi_slot(&self, timestamp: Timestamp) -> Option<u128> {
		matches!(&self.kind, Kind::Aac { config, .. } if config.program_config.is_some())
			.then(|| slot(timestamp, PSI_INTERVAL))
	}

	/// The PES [`Export::pack`] may cut a frame of `payload` presented at `timestamp` into under
	/// `delay`, if it packs the frame: as many frames as B holds from a place on the media
	/// timeline that is a multiple of them, cut again where they would span more than half the
	/// delay and at each [`Self::psi_slot`].
	fn pack_start(&self, payload: &[u8], timestamp: Timestamp, delay: Duration) -> Option<PesStart> {
		let (samples, rate) = self.samples(payload).filter(|_| !delay.is_zero())?;
		let len = match self.kind {
			Kind::Aac { .. } => adts::MIN_HEADER_LEN + payload.len(),
			_ => payload.len(),
		};
		let place = place(timestamp, samples, rate);
		let behind = place % self.fit(len).1;
		let frame = 1_000_000_000 * u128::from(samples) / u128::from(rate);
		let spans = (delay / 2).as_nanos() / frame.max(1) + 1;
		Some((place - behind % spans, self.psi_slot(timestamp)))
	}

	/// How long a receiver takes to pass one of the track's packets on, so its last packet has
	/// to arrive that long before it decodes. A packet's bytes reach a decoder buffer only once
	/// they have drained through the T-STD buffers ahead of it (ISO 13818-1 2.4.2); without it
	/// a unit whose DTS sits on a PCR slot boundary, as a source's own 25 fps timestamps put
	/// every fifth frame, arrives complete only after it decodes.
	fn drain(&self) -> Duration {
		self.buffer().unwrap_or(Buffer::SYSTEM).drain()
	}

	/// How far ahead of its decode time the track's units may go out under `delay`: as far
	/// as [`schedule::ahead`] lets them, and nothing without a delay or for a placed frame,
	/// whose timestamp is when it goes out.
	fn ahead(&self, delay: Duration) -> Duration {
		// A placed frame goes out at its placement, never ahead of it.
		if delay.is_zero() || self.kind.placed() {
			return Duration::ZERO;
		}
		let slots = schedule::ahead(self.buffer().as_ref(), schedule::slot(delay.as_nanos()));
		PCR_INTERVAL * u32::try_from(slots).unwrap_or(u32::MAX)
	}

	/// Where the jitter buffer orders a frame decoding at `decode` under `delay`: at its release
	/// time, [`Self::ahead`] before its decode time, so it reaches the schedule as the first slot
	/// it may ride is laid, and a slot goes out the delay after the source's clock rather than a
	/// delay later still. Keys run a delay past release times, which no send-ahead exceeds, so a
	/// frame near the start of the timeline is not cut short of its send-ahead. The clock anchors
	/// on the track sent latest against these keys, so every track keeps the whole delay to
	/// arrive in. A send-ahead that grew (a new SPS declaring a larger CPB) releases no frame
	/// before the one ahead of it.
	///
	/// An audio frame that shares a PES with the frame released before it takes that frame's
	/// key, so the whole PES is in hand when its first slot is laid; the first frame released
	/// of a PES (a joiner's may not be the PES's first) keeps its own. That costs no latency.
	/// A TS source sends a PES's frames together, so they lose nothing. One that sends audio
	/// at its decode time sends the later frames behind their key, so the clock still anchors
	/// on the first, and they have that much less of the delay to arrive in.
	fn release_key(
		&mut self,
		decode: Timestamp,
		delay: Duration,
		frame: &Frame,
	) -> anyhow::Result<(Timestamp, Option<PesStart>)> {
		let lead = delay.saturating_sub(self.ahead(delay));
		let key = Duration::from(decode)
			.checked_add(lead)
			.context("MPEG-TS decode time past the end of the timeline")?;
		let mut key = Timestamp::try_from(key)?;
		let start = self.pack_start(&frame.payload, frame.timestamp, delay);
		if let Some((last, previous, shared)) = self.released
			&& decode >= last
		{
			key = match start.is_some() && shared == start {
				true => previous,
				false => key.max(previous),
			};
		}
		self.released = Some((decode, key, start));
		Ok((key, start))
	}

	/// Queue every held video frame whose DTS is settled or that is overdue
	/// ([`DecodeClock::settle`]), or all of them when `flush`.
	///
	/// A held frame is ready no earlier than the frame or observed loss that settled it,
	/// so it is judged as arriving then. Frames held through a silence,
	/// such as a dead publisher's last frames settled only by its replacement's, are late by the
	/// silence: the jitter buffer drops them, where their own arrival would queue them behind a
	/// schedule that has already gone past them. Nothing settles the frames still held when the
	/// track ends, so they keep their own arrival and go out late if they must.
	fn release(
		&mut self,
		name: &str,
		jitter: &mut jitter::Buffer<TrackKey, Queued>,
		flush: bool,
	) -> anyhow::Result<()> {
		if let Some(mut clock) = self.declared.take() {
			let released = (|| {
				while let Some((item, dts)) = clock.pop(flush)? {
					let Some((mut pending, description)) = item else {
						continue;
					};
					if !self.finished {
						pending.arrived = pending.arrived.max(self.latest);
					}
					self.push(name, pending, Some(dts), description, jitter)?;
				}
				anyhow::Ok(())
			})();
			self.declared = Some(clock);
			released?;
		}
		let (delay, lookahead) = self.timing.inferred();
		let lookahead = lookahead.max(self.timing.jitter);
		loop {
			let settle = match flush {
				true => Settle::All,
				false => self.clock.settle(lookahead, self.latest, jitter.delay()),
			};
			let Some(((mut pending, description), dts)) = self.clock.pop(lookahead, delay, settle) else {
				break;
			};
			if !self.finished {
				pending.arrived = pending.arrived.max(self.latest);
			}
			self.push(name, pending, Some(dts.into()), description, jitter)?;
		}
		Ok(())
	}

	/// Queue a frame decoding at `dts` (signed 90 kHz ticks), else at its PTS.
	///
	/// A DTS before the timeline's zero, a declared group's leading frames at its start, wraps
	/// into the 33-bit wire field like any other, and is scheduled from zero: the few ticks it
	/// precedes zero by are below anything the T-STD resolves.
	fn push(
		&mut self,
		name: &str,
		pending: Pending,
		dts: Option<i128>,
		description: Option<Bytes>,
		jitter: &mut jitter::Buffer<TrackKey, Queued>,
	) -> anyhow::Result<()> {
		let Pending {
			frame,
			restart,
			skip,
			arrived,
			read,
			..
		} = pending;
		let dts = dts.filter(|&dts| dts != i128::from(to_ticks(frame.timestamp)));
		let decode = dts
			.and_then(|ticks| Timestamp::from_scale(u64::try_from(ticks.max(0)).ok()?, 90_000).ok())
			.unwrap_or(frame.timestamp);
		// Two's complement, so the 33-bit mask wraps a DTS before zero as it would a later one.
		let dts = dts.map(|ticks| ticks as u64);
		let (key, pes) = self.release_key(decode, jitter.delay(), &frame)?;
		let video = matches!(self.kind, Kind::Video(_));
		let leading = video
			&& !frame.keyframe
			&& self.keyframe.is_some_and(|keyframe| frame.timestamp < keyframe)
			&& !matches!(self.kind, Kind::Video(stream_type) if radl(stream_type, description.as_ref(), &frame));
		if video && frame.keyframe {
			self.keyframe = Some(frame.timestamp);
		}
		// Video, MPEG-1/2 carried whole among it, resumes only at a keyframe, since its other
		// pictures reference earlier ones.
		let sync = frame.keyframe || self.kind.class() != super::stats::Class::Video;
		// Replaying anchors on the first frame, and the tracks arrive together rather than each
		// sent ahead by its send-ahead: judged as arriving that much earlier, as a live source
		// sends it, every track's first frames are on the clock whichever anchors it.
		let ahead = match jitter.replaying() {
			true => (Duration::from(decode) + jitter.delay()).saturating_sub(Duration::from(key)),
			false => Duration::ZERO,
		};
		// Logical clocks can begin at zero. Shift both readings by the same representable amount so
		// their ordering survives even when the full replay lead would precede the clock's origin.
		let ahead = ahead.min(arrived.duration_since(moq_clock::Instant::ZERO));
		let ahead = ahead.min(read.duration_since(moq_clock::Instant::ZERO));
		let arrival = Arrival {
			arrived: arrived - ahead,
			read: read - ahead,
			decode: key.into(),
			restart,
			skip,
			sync,
			leading,
			item: Queued {
				frame,
				decode,
				key,
				dts,
				description,
				pes,
			},
		};
		if jitter.push(self.key.clone(), arrival)? == Push::Late {
			tracing::warn!(
				track = %name,
				dropped = jitter.dropped(),
				trimmed = jitter.trimmed(),
				"frame missed its deadline; dropped"
			);
		}
		Ok(())
	}
}

/// What a video rendition declares about its reordering, which bounds its [`DecodeClock`].
#[derive(Default)]
struct Timing {
	/// The catalog `jitter` in 90 kHz ticks: the most any frame decodes before it is presented.
	jitter: u64,
	/// The catalog `framerate`: the picture period for an SPS that declares no fixed rate.
	framerate: Option<f64>,
	/// The avcC/hvcC [`Self::declared`] and [`Self::hrd`] were read from.
	description: Option<Bytes>,
	declared: Option<Reorder>,
	/// The stream's level limits, from the catalog codec.
	level: Option<Level>,
	/// The reorder depth the catalog declares, which clocks each group on its own.
	reorder: Option<usize>,
	/// The HRD its SPS declares.
	hrd: Option<Hrd>,
}

impl Timing {
	/// Take the rendition's timing from a catalog snapshot, before or after the program
	/// tables are written.
	fn configure(&mut self, config: &VideoConfig, name: &str) -> crate::Result<()> {
		self.jitter = config.jitter.map_or(0, |t| (t.as_micros() * 90_000 / 1_000_000) as u64);
		self.framerate = config.framerate.filter(|fps| fps.is_finite() && *fps > 0.0);
		self.level = Some(video_level(config, name));
		self.reorder = crate::container::declared::depth(config)?;
		Ok(())
	}

	/// Read the reorder depth from the codec config a keyframe is carried with.
	fn describe(&mut self, stream_type: StreamType, description: Option<&Bytes>, name: &str) {
		if self.description.as_ref() == description {
			return;
		}
		self.description = description.cloned();
		self.declared = description.and_then(|d| declared_reorder(stream_type, d));
		self.hrd = description.and_then(|d| declared_hrd(stream_type, d));
		tracing::debug!(track = %name, reorder = ?self.inferred(), "video reordering declared");
	}

	/// The receiver's buffers (H.222.0 2.14.3.1, 2.17.2): EB is the HRD's CPB, else the
	/// level's; the transport buffer drains at Rx, scaled from the HRD's BitRate or else the
	/// level's; and MB passes payload on into EB at the level's Rbx whatever the HRD declares.
	/// EB reaches the HRD's buffer delay ahead, else what the level's CPB holds at its top rate.
	fn buffer(&self) -> Option<Buffer> {
		let level = self.level?;
		let (bit_rate, cpb) = match self.hrd {
			Some(hrd) => (hrd.bit_rate, hrd.cpb_size),
			None => (level.bit_rate, level.cpb),
		};
		Some(Buffer {
			rate: bit_rate.saturating_mul(level.factor) / 1_000,
			leak: Some(level.leak),
			size: Some(usize::try_from(cpb / 8).unwrap_or(usize::MAX)),
			transport: 512,
			reach: self.buffer_delay().unwrap_or_else(|| fill(level.cpb, level.leak)),
		})
	}

	/// How long the HRD's CPB takes to fill at its rate: how far ahead of its decode time the
	/// source may send a picture, so the export has to as well.
	fn buffer_delay(&self) -> Option<Duration> {
		self.hrd.map(|hrd| fill(hrd.cpb_size, hrd.bit_rate))
	}

	/// The least reorder delay and lookahead the SPS declares, in ticks, so two exporters agree
	/// on them from their first keyframe whatever reordering each has seen.
	///
	/// The delay is the SPS's depth in pictures, at its picture period or the catalog
	/// `framerate`'s. A frame can be presented up to `2^depth - 1` pictures below one decoded
	/// before it (a B-pyramid that deep), which bounds the lookahead the catalog `jitter`
	/// gives when it is not published.
	fn inferred(&self) -> (u64, u64) {
		let Some(declared) = self.declared else {
			return (0, 0);
		};
		let (delay, lookahead) = declared.timing(self.framerate);
		(delay.min(MAX_REORDER), lookahead.min(MAX_REORDER))
	}
}

#[derive(Clone, PartialEq)]
enum Kind {
	/// Video carries its TS stream type (H.264 = 0x1B, H.265 = 0x24).
	Video(StreamType),
	/// AAC, framed as ADTS. A `channel_config` of 0 defers the layout to a program config
	/// element, which leads the first raw data block after each PAT/PMT, so a receiver that tunes
	/// in at the tables can decode from there. `repeat` marks that the next frame carries it.
	Aac { config: aac::InBand, repeat: bool },
	/// Opus (private stream_type 0x06). Each frame is one Opus packet, prefixed with
	/// the Opus-in-TS access-unit control header and announced with the 'Opus'
	/// registration plus DVB extension descriptor. `channel_config_code` is a plain
	/// code that descriptor can name, never a count clamped into range.
	Opus { channel_config_code: u8 },
	/// MP2, carried verbatim. The sample rate picks the stream type on the way
	/// out (0x03 vs 0x04).
	Mp2 { sample_rate: u32 },
	/// AC-3 (ATSC stream_type 0x81), carried verbatim.
	Ac3,
	/// E-AC-3 (ATSC stream_type 0x87), carried verbatim.
	Eac3,
	/// An undecoded elementary stream carried as private sections (SCTE-35, ...),
	/// re-announced in the PMT with its recorded `stream_type`.
	Verbatim { stream_type: u8 },
	/// An undecoded elementary stream whose frames are whole PES packets, written unchanged
	/// and sent at their timestamp, the source's placement ([`catalog::PesPacket`]).
	PesPacket { stream_type: u8 },
}

impl Kind {
	/// The `stream_type` the PMT announces it as.
	fn stream_type(&self) -> u8 {
		match self {
			Kind::Video(stream_type) => *stream_type as u8,
			Kind::Aac { .. } => StreamType::AdtsAac as u8,
			// Opus rides private-data PES; the registration + extension descriptors tell the
			// demuxer it's Opus.
			Kind::Opus { .. } => 0x06,
			// Half-rate MPEG-2 BC audio (< 32 kHz) re-announces as 0x04; the full rates are
			// MPEG-1 (0x03). The catalog sample rate came from the frame header, so the mapping
			// is faithful.
			Kind::Mp2 { sample_rate } if *sample_rate < 32000 => StreamType::Mpeg2HalvedSampleRateAudio as u8,
			Kind::Mp2 { .. } => StreamType::Mpeg1Audio as u8,
			Kind::Ac3 => StreamType::DolbyDigitalUpToSixChannelAudio as u8,
			Kind::Eac3 => StreamType::DolbyDigitalPlusUpTo16ChannelAudioForAtsc as u8,
			Kind::Verbatim { stream_type, .. } | Kind::PesPacket { stream_type } => *stream_type,
		}
	}

	/// The PMT ES descriptors announcing it when import captured none: the ATSC Dolby
	/// registration, so a fresh (non-TS) AC-3/E-AC-3 track is announced the way the import
	/// path expects, and Opus's registration and channel configuration.
	fn descriptors(&self) -> Vec<catalog::Descriptor> {
		match self {
			Kind::Ac3 => vec![registration(b"AC-3")],
			Kind::Eac3 => vec![registration(b"EAC3")],
			Kind::Opus { channel_config_code } => opus_descriptors(*channel_config_code),
			_ => Vec::new(),
		}
	}

	/// Its timestamp is arrival on the program clock, rather than a decode deadline.
	fn placed(&self) -> bool {
		matches!(self, Self::PesPacket { .. } | Self::Verbatim { .. })
	}

	/// The track suffix [`Import`](super::Import) gives a PID carrying this kind, so a stream's
	/// row is named alike at both edges.
	fn suffix(&self) -> &'static str {
		match self {
			Kind::Video(StreamType::H265) => ".hev1",
			Kind::Video(_) => ".avc3",
			Kind::Aac { .. } => ".aac",
			Kind::Opus { .. } => ".opus",
			Kind::Mp2 { .. } => ".mp2",
			Kind::Ac3 => ".ac3",
			Kind::Eac3 => ".eac3",
			Kind::Verbatim { .. } | Kind::PesPacket { .. } => ".ts",
		}
	}

	/// The classification [`Import`](super::Import) would give this PID, so a row is graded
	/// alike at both edges.
	fn class(&self) -> super::stats::Class {
		match self {
			// MPEG-1/2 video rides whole, but paces the program as decoded video does.
			Kind::Video(_)
			| Kind::PesPacket {
				stream_type: 0x01 | 0x02,
			} => super::stats::Class::Video,
			Kind::Aac { .. } | Kind::Opus { .. } | Kind::Mp2 { .. } | Kind::Ac3 | Kind::Eac3 => {
				super::stats::Class::Audio
			}
			Kind::Verbatim { .. } | Kind::PesPacket { .. } => super::stats::Class::Data,
		}
	}
}

/// The program tables: the PAT, fixed for the program's life but for its network entry, and the
/// PMT, which lists the streams on the air. The schedule lays them as they stand when each copy
/// begins to go out ([`Schedule::set_psi`]), so a change is one update of this state, and a
/// revision overtaken before any copy of it began takes no version.
struct Psi {
	/// The source's `transport_stream_id`, so the rebuilt PAT stays consistent with the carried SI.
	transport_stream_id: u16,
	/// What the PMT says now.
	pmt: psi::Pmt,
	/// PID the PMT rides on: the source's original (preserved from the service
	/// record) or the synthesized [`PMT_PID`] for a media-only source.
	pmt_pid: u16,
	/// The PMT the last tables handed to the schedule listed: the streams the next copy adds.
	led: Option<psi::Pmt>,
	/// The tables last set on the schedule, and the PIDs they flag a break on.
	stated: Option<(Laid, Vec<u16>)>,
	/// The tables of the last copy to begin going out, whose version the next change follows.
	laid: Option<Laid>,
	/// The tables of each copy begun and not yet finished, oldest first.
	sending: VecDeque<Laid>,
	/// The last tables returned.
	out: Option<Laid>,
}

/// Program tables as a copy of them goes out: the PMT at its version, and the PAT.
#[derive(Clone)]
struct Laid {
	version: VersionNumber,
	pmt: psi::Pmt,
	/// Whether the PAT lists the network entry, pointing at the NIT the export carries.
	network: bool,
	pat: Bytes,
}

impl Psi {
	/// The PID the PMT names for the clock.
	fn pcr_pid(&self) -> Option<u16> {
		self.pmt.pcr_pid.map(|pid| pid.as_u16())
	}

	/// The PID the clock rides alone, which no stream may take.
	fn own_clock(&self) -> Option<u16> {
		self.pcr_pid()
			.filter(|&pcr| self.pmt.streams.iter().all(|es| es.pid.as_u16() != pcr))
	}

	/// The tables as they stand, listing the network entry when `network`. A PMT that differs
	/// from the last laid goes out at the next version, `first` before any has; the PAT keeps its
	/// version until its network entry changes, so it never runs ahead of the PMT's.
	fn current(&self, network: bool, first: VersionNumber) -> anyhow::Result<Laid> {
		let (version, pat) = match &self.laid {
			Some(laid) if laid.pmt == self.pmt && laid.network == network => return Ok(laid.clone()),
			Some(laid) => {
				let mut version = laid.version;
				version.increment();
				(version, (laid.network == network).then(|| laid.pat.clone()))
			}
			None => (first, None),
		};
		let pat = match pat {
			Some(pat) => pat,
			None => encode_pat(
				self.transport_stream_id,
				self.pmt.program_number,
				self.pmt_pid,
				network,
				version,
			)?,
		};
		Ok(Laid {
			version,
			pmt: self.pmt.clone(),
			network,
			pat,
		})
	}
}

/// The DVB NIT's PID (ETSI EN 300 468 5.1.3), the network PID the PAT's program 0 names.
const NIT_PID: u16 = 0x0010;
/// The `table_id` of the NIT describing the delivering network.
const NIT_ACTUAL: u8 = 0x40;

/// The PAT for the exported program at `version`, listing the network PID as program 0 first when
/// the export carries the NIT (ISO/IEC 13818-1 2.4.4.3), which a receiver otherwise cannot find.
fn encode_pat(
	transport_stream_id: u16,
	program_number: u16,
	pmt_pid: u16,
	network: bool,
	version: VersionNumber,
) -> anyhow::Result<Bytes> {
	let network = network.then(|| ProgramAssociation {
		program_num: 0,
		program_map_pid: Pid::new(NIT_PID).expect("a valid PID"),
	});
	let program = ProgramAssociation {
		program_num: program_number,
		program_map_pid: Pid::new(pmt_pid)?,
	};
	psi::encode_pat(&Pat {
		transport_stream_id,
		version_number: version,
		table: network.into_iter().chain([program]).collect(),
	})
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
	/// Explicit PES stream_id (Opus); `None` derives it from `is_video`.
	stream_id: Option<u8>,
}

/// Consecutive audio frames muxed into one PES still to go out ([`Export::pack`]).
struct Pack {
	/// The first frame's PES fields: its PTS times the frames after it too.
	unit: PesUnit,
	/// When the first frame decodes, the PES's deadline.
	decode: Timestamp,
	/// The PES its frames share ([`Track::pack_start`]).
	start: Option<PesStart>,
	/// The frames' elementary stream bytes.
	payload: Vec<u8>,
	/// Each frame's decode nanoseconds and bytes, which leave the decoder buffer as it decodes.
	frames: Vec<(u128, usize)>,
	/// The frames' samples so far, and their sample rate.
	samples: u64,
	rate: u32,
}

impl Pack {
	/// When the frame after the last presents, in microseconds, if the frames run on without
	/// a gap.
	fn end(&self) -> u128 {
		self.unit.timestamp.as_micros() + u128::from(self.samples) * 1_000_000 / u128::from(self.rate)
	}
}

/// One SI entry's subscription: resolves the snapshot track named by the catalog,
/// reduces its newest complete group into the current section set, and remembers
/// when it last hit the wire so it re-emits on its own cadence.
struct SiTrack {
	/// The catalog's track name for this entry, so a catalog update that repoints
	/// the entry at a different track (a restarted publisher) is detected and the
	/// subscription rebuilt rather than left repeating the old track's sections.
	track: String,
	interval: Option<Duration>,
	state: SiState,
	/// The reduced newest *complete* group: what emission re-transmits.
	active: super::si::Snapshot,
	/// A group still being read; swapped into `active` when it ends, so a torn
	/// half-received snapshot is never emitted.
	pending: Option<(moq_net::group::Consumer, super::si::Snapshot)>,
	/// A promoted snapshot changed `active` and has not hit the wire yet: emit on
	/// the revision floor ([`SI_REVISION_INTERVAL`]) instead of waiting out the
	/// repetition interval. For a clock table (TDT/TOT) the cadence is the content,
	/// so holding a revision to the interval delivers the time seconds late and can
	/// re-assert an already-sent value (#2934).
	dirty: bool,
	/// Media timestamp of the last emission ([`Export::mux`]).
	last_emit: Option<Timestamp>,
	/// The same budget the media sources use. SI carries table snapshots at a low
	/// rate, so the real-time default would take only the newest group and drop
	/// the revisions between: an SI track needs the export's replay window as much
	/// as the media it describes.
	max_age: Duration,
}

/// The SI subscription's lifecycle, mirroring `ExportSource`'s but reading raw
/// snapshot groups instead of timestamp-paced container frames.
enum SiState {
	/// Waiting for the catalog broadcast to resolve; the track (by name) is
	/// subscribed once it does.
	Requesting(crate::Binding, String),
	/// Waiting for the subscription to resolve.
	Subscribing(kio::Pending<moq_net::track::Subscribing>),
	/// The resolved subscription, reading snapshot groups. Boxed: a reader is far larger
	/// than the other states.
	Active(Box<moq_net::track::Ordered>),
	/// The track ended or failed; the last complete snapshot keeps re-emitting,
	/// mirroring how a real mux repeats its tables between (absent) revisions.
	Done,
}

impl SiTrack {
	fn new(source: &crate::Source, entry: &catalog::SiEntry, max_age: Duration) -> Self {
		// An entry read from the inline catalog form has its sections in hand and
		// no track to subscribe to: it starts (and stays) where an ended track ends
		// up, re-emitting a fixed snapshot on its interval.
		let (state, active) = if entry.sections.is_empty() {
			(
				SiState::Requesting(source.request_catalog(), entry.track.clone()),
				Default::default(),
			)
		} else {
			(SiState::Done, Self::inline(entry))
		};
		Self {
			track: entry.track.clone(),
			interval: entry.interval,
			state,
			// Inline sections are a snapshot that has not hit the wire yet, which a
			// clock table only sends as a revision.
			dirty: !active.is_empty(),
			active,
			pending: None,
			last_emit: None,
			max_age,
		}
	}

	/// The snapshot an inline-form entry's sections reduce to.
	fn inline(entry: &catalog::SiEntry) -> super::si::Snapshot {
		let mut snapshot = super::si::Snapshot::default();
		for section in &entry.sections {
			snapshot.apply(section);
		}
		snapshot
	}

	/// Drive the subscription and fold arrived groups into `active`. Never returns
	/// an error: SI is auxiliary, so a failed or ended track logs and keeps the last
	/// snapshot rather than killing the mux.
	fn poll(&mut self, waiter: &kio::Waiter) {
		if matches!(self.state, SiState::Requesting(..)) {
			// Scope the borrow of `self.state` so the transitions below can assign it.
			let resolved = {
				let SiState::Requesting(pending, name) = &self.state else {
					unreachable!("just matched Requesting");
				};
				match pending.poll_broadcast(waiter) {
					Poll::Ready(Ok(broadcast)) => Ok((broadcast, name.clone())),
					Poll::Ready(Err(err)) => Err(err),
					Poll::Pending => return,
				}
			};
			self.state = match resolved {
				Ok((broadcast, name)) => match broadcast.track(&name) {
					Ok(track) => SiState::Subscribing(
						track.subscribe(moq_net::track::Subscription::default().with_max_delay(self.max_age)),
					),
					Err(err) => {
						tracing::warn!(%err, track = %name, "SI track unavailable; carrying the last snapshot");
						SiState::Done
					}
				},
				Err(err) => {
					tracing::warn!(%err, "SI broadcast unavailable; carrying the last snapshot");
					SiState::Done
				}
			};
		}

		if matches!(self.state, SiState::Subscribing(_)) {
			let resolved = {
				let SiState::Subscribing(pending) = &self.state else {
					unreachable!("just matched Subscribing");
				};
				match pending.poll_ok(waiter) {
					Poll::Ready(result) => result,
					Poll::Pending => return,
				}
			};
			self.state = match resolved {
				Ok(track) => SiState::Active(Box::new(track.ordered())),
				Err(err) => {
					tracing::warn!(%err, "SI subscription failed; carrying the last snapshot");
					SiState::Done
				}
			};
		}

		let mut ended = false;
		if let SiState::Active(track) = &mut self.state {
			loop {
				// Drain to the newest group first: a snapshot obsoletes every older
				// one, including a partially-read pending group.
				match track.poll_next_group(waiter) {
					Poll::Ready(Ok(Some(group))) => {
						self.pending = Some((group, Default::default()));
					}
					Poll::Ready(Ok(None)) => {
						ended = true;
						break;
					}
					Poll::Ready(Err(err)) => {
						tracing::warn!(%err, "SI track failed; carrying the last snapshot");
						ended = true;
						break;
					}
					Poll::Pending => break,
				}
			}
		}
		if ended {
			self.state = SiState::Done;
		}

		// Read the pending group to its end, then promote it wholesale: emitting a
		// half-received snapshot would re-introduce the torn state the group
		// boundary exists to prevent.
		while let Some((group, snapshot)) = &mut self.pending {
			let (promote, drop_pending) = match group.poll_read_frame(waiter) {
				Poll::Ready(Ok(Some(frame))) => {
					snapshot.apply(&frame.payload);
					(false, false)
				}
				Poll::Ready(Ok(None)) => (true, false),
				Poll::Ready(Err(err)) => {
					tracing::warn!(%err, "SI group failed; carrying the last snapshot");
					(false, true)
				}
				Poll::Pending => break,
			};
			if promote {
				let (_, snapshot) = self.pending.take().unwrap();
				// A repeated group (the same section set re-published) is not a change:
				// treating it as one would turn the source's snapshot cadence into extra
				// wire repetitions.
				self.dirty |= snapshot != self.active;
				self.active = snapshot;
			} else if drop_pending {
				self.pending = None;
			}
		}
	}
}

impl<S: Stream> Program<S>
where
	S::Ext: catalog::Catalog,
{
	fn new(source: crate::Source, catalog: S) -> Self {
		let instance = source.broadcast().info().epoch.clone();
		Self {
			id: 0,
			retired: false,
			left: None,
			expected: Vec::new(),
			expected_changed: false,
			source,
			catalog: Some(catalog),
			catalog_ended: false,
			cataloged: false,
			instance,
			stale: HashSet::new(),
			version: VersionNumber::default(),
			flagged: None,
			tables: Vec::new(),
			psi_due: false,
			revised: false,
			delay: Duration::ZERO,
			generation: 0,
			tracks: HashMap::new(),
			counters: FxHashMap::default(),
			program_descriptors: Vec::new(),
			program: None,
			si: BTreeMap::new(),
			last_timestamp: None,
			si_flushed: false,
			psi: None,
			last_psi: None,
			epoch: 0,
			emitted_epoch: 0,
			pcr_discontinuity: false,
			schedule: Schedule::new(Duration::ZERO),
			slot_timer: None,
			replay: false,
			queue: VecDeque::new(),
			liveness: Default::default(),
			video_start: None,
			video_out: None,
			trimmed: 0,
			mux_rate: None,
			mux_rate_override: None,
		}
	}
}
impl<S: Stream> Export<S>
where
	S::Ext: catalog::Catalog,
{
	/// Export `source` as MPEG-TS, driving track subscriptions from `catalog`.
	///
	/// The caller chooses the catalog format and rendition selection on any [`Stream`].
	/// The stream remains owned until the export is dropped or follows another broadcast;
	/// concrete catalog consumers release their subscription and decoder when they end.
	pub fn new(source: crate::Source, catalog: S) -> Self {
		Self {
			program: Program::new(source, catalog),
			jitter: jitter::Buffer::new(Duration::ZERO),
			held: None,
			pending: None,
			next_program: 1,
			active_program: 0,
			follow_until: None,
			follow_timer: None,
		}
	}

	/// Pad the output with null packets to `mux_rate` bits per second, whatever the
	/// catalog records. Without this the catalog's `mpegts.muxRate` decides, and a
	/// catalog without one leaves the output unpadded.
	///
	/// Zero and absurd rates are refused with a warning, leaving the output
	/// unpadded rather than allocating unbounded nulls.
	pub fn with_mux_rate(mut self, mux_rate: u64) -> Self {
		if sanitize_mux_rate(mux_rate).is_none() {
			tracing::warn!(mux_rate, "ignoring invalid MPEG-TS multiplex rate override");
		}
		self.program.mux_rate_override = Some(mux_rate);
		self.program.mux_rate = sanitize_mux_rate(mux_rate);
		self.program.schedule.set_rate(self.program.mux_rate);
		self
	}

	/// Read each track from the oldest group the delay still reaches instead of the live
	/// edge, and anchor the clock on the first frame instead of acquiring it, for a test that
	/// writes a broadcast whole before exporting it.
	#[cfg(test)]
	pub(super) fn with_replay(mut self) -> Self {
		self.program.replay = true;
		self.jitter = jitter::Buffer::new(self.program.delay).replay();
		self
	}

	/// Mux each frame this long after its release time, like an SRT receiver's TSBPD: its
	/// decode time less its send-ahead.
	///
	/// The send-ahead comes out of the same budget. A frame may go out up to the delay
	/// ahead of its decode time, as far as its PID's decoder buffer holds and as early as the
	/// receiver's buffers admit, so a heavy passage rides the slots before it. Its release
	/// time is that far before its decode time, so it is muxed just as the first slot it may
	/// ride is laid, and each slot goes out the delay after its own time on the source's
	/// clock rather than a delay later still.
	///
	/// The clock is acquired before anything goes out: frames are held until every expected
	/// track has delivered (two delays at most) and a track starts a new group, or for a delay
	/// at most once all have. The track sent latest against its release time sets the clock,
	/// so a joiner runs at the delay from its first output and every frame has the whole delay
	/// to arrive in. A source that sends its video its send-ahead
	/// early, as a TS mux does, is trailed by the delay; one that sends every track at its
	/// decode time, by the delay and the video's send-ahead. The output then keeps the
	/// source's pace and muxes every track in `(release time, PID)` order whatever the
	/// arrival skew between them. A frame that arrives after its deadline is dropped, and a
	/// video track that dropped one resumes at its next keyframe. A source whose clock runs
	/// further off ours than 13818-1 lets the PCR follow fails the export once it has used half
	/// the delay, where a brief network stall only drops what it made late. A stalled group is
	/// skipped once it falls half the delay behind the newest content (see
	/// [`Consumer`](crate::container::Consumer)), so the group after it still arrives with
	/// the other half to spare.
	///
	/// With a multiplex rate, a frame that cannot arrive by its decode time fails the export,
	/// as does video whose HRD declares a buffer delay past the delay. Defaults to
	/// [`Duration::ZERO`], which holds nothing and sends each frame by its decode time.
	pub fn with_delay(mut self, delay: Duration) -> Self {
		self.program.delay = delay;
		self.jitter = jitter::Buffer::new(delay);
		#[cfg(test)]
		if self.program.replay {
			self.jitter = jitter::Buffer::new(delay).replay();
		}
		self.program.schedule = Schedule::new(delay);
		self.program.schedule.set_rate(self.program.mux_rate);
		self
	}

	/// Get the next muxed frame.
	///
	/// Each [`Frame`] carries one slice of the PCR grid in `payload`: the clock
	/// packets that slice opens with, followed by the muxed bytes belonging to it.
	/// With a [delay](Self::with_delay), each slice is handed over at its time on the
	/// jitter buffer's clock, which follows the source's, so a sink writes it as it
	/// comes. It is stamped with the media time that slice starts at, for a transport
	/// that stamps its own delivery, and `keyframe` marks the slice a video keyframe
	/// begins in. The leading PAT/PMT rides on the first slice, and is
	/// re-emitted at video keyframes and periodically for mid-stream tune-in.
	/// Returns `None` when the broadcast ends. `duration` is always `None`: the
	/// muxer has no use for it.
	pub async fn next(&mut self) -> crate::Result<Option<Frame>> {
		kio::wait(|waiter| self.poll_next(waiter)).await
	}

	/// Poll for the next MPEG-TS packet frame, or `None` once the export ends.
	pub fn poll_next(&mut self, waiter: &kio::Waiter) -> Poll<crate::Result<Option<Frame>>> {
		if self.pending.is_some() {
			self.stop_old(waiter)?;
		}
		if let Err(err) = self.prepare(waiter) {
			self.leave(err)?;
		}
		if self.pending.is_some() {
			loop {
				if let Err(err) = self.prepare_pending(waiter) {
					// A replacement that leaves before it commits is no switch: the old program
					// plays on, and a later replacement gets its own follow window.
					if self.jitter.is_committed() || self.program.retired || !super::follower::left(&err) {
						return Poll::Ready(Err(err));
					}
					tracing::warn!(%err, "replacement left before it went out, keeping the old program");
					self.cancel_pending();
					self.follow_until = None;
					self.follow_timer = None;
					break;
				}
				self.stop_old(waiter)?;
				let Poll::Ready(ready) = self.jitter.poll_pending(waiter) else {
					break;
				};
				let mut pending = self.pending.take().expect("an acquiring program");
				std::mem::swap(&mut self.program, pending.as_mut());
				let kind = self
					.program
					.tracks
					.get(&ready.track.name)
					.context("missing incoming track")?
					.kind
					.clone();
				let leading = self.leads_video(&kind, &ready.item);
				let waiting = !leading
					&& self.program.video_out.is_none()
					&& self
						.program
						.tracks
						.values()
						.any(|track| matches!(track.kind, Kind::Video(_)));
				if !leading && !waiting {
					// A future queued video start can still be cut. Own its Ready
					// before selecting the program, and recheck held audio against it.
					for held in self.jitter.take_pending() {
						let kind = self
							.program
							.tracks
							.get(&held.track.name)
							.context("missing held track")?
							.kind
							.clone();
						if held.generation != ready.generation || self.leads_video(&kind, &held.item) {
							self.jitter.discard(&held);
						} else {
							self.jitter.hold_pending(held);
						}
					}
				}
				std::mem::swap(&mut self.program, pending.as_mut());
				self.pending = Some(pending);
				if leading {
					self.jitter.discard(&ready);
					continue;
				}
				if waiting {
					self.jitter.hold_pending(ready);
					continue;
				}
				for (name, track) in &mut self.program.tracks {
					track.release(name, &mut self.jitter, true)?;
				}
				tracing::debug!(target: "moq_mux::container::ts::handoff", phase = "commit-program", now = ?moq_clock::now(), old = self.program.id, new = ready.track.program, generation = ready.generation, pts = ?ready.item.frame.timestamp, key = ?ready.item.key, decode = ?ready.item.decode);
				self.jitter.commit(ready)?;
				break;
			}
		}

		// Once every source has ended, the tail goes out late if it must: the end of the
		// stream is not a missed deadline.
		self.program
			.schedule
			.set_ended(!self.program.tracks.is_empty() && self.program.tracks.values().all(|track| track.finished));

		// 4. Mux each frame the jitter buffer lets go (the first carries the buffered
		// PAT/PMT), then lay out every grid slot whose time has come ([`Self::lay_due`]).
		loop {
			if let Some(out) = self.pop() {
				return Poll::Ready(Ok(Some(out)));
			}
			self.settle()?;
			let ready = match self.held.take() {
				Some(ready) => ready,
				None => match self.jitter.poll_next(waiter) {
					Poll::Ready(ready) => ready,
					Poll::Pending => break,
				},
			};
			let switching = self.pending.as_ref().is_some_and(|next| ready.track.program == next.id);
			if switching {
				let mut pending = self.pending.take().expect("the ready program");
				std::mem::swap(&mut self.program, pending.as_mut());
				let kind = self
					.program
					.tracks
					.get(&ready.track.name)
					.context("missing incoming track")?
					.kind
					.clone();
				let leading = self.leads_video(&kind, &ready.item);
				std::mem::swap(&mut self.program, pending.as_mut());
				self.pending = Some(pending);
				if leading {
					self.jitter.discard(&ready);
					continue;
				}
			}
			if switching && (!self.program.schedule.is_empty() || self.packing()) {
				self.lay(None)?;
				self.held = Some(ready);
				continue;
			}
			if switching {
				self.adopt(&ready)?;
			}
			if ready.generation > self.program.generation {
				if !self.program.schedule.is_empty() || self.packing() {
					// A boundary ends valid media rather than reneging it.
					// Return that tail under the old generation before adopting the new one.
					self.lay(None)?;
					self.held = Some(ready);
					continue;
				}
				self.rewind();
				self.program.generation = ready.generation;
			}
			debug_assert!(
				ready.generation >= self.program.generation,
				"the jitter buffer released an older generation after a newer one"
			);
			if ready.track.program != self.program.id {
				return Poll::Ready(Err(
					anyhow::anyhow!("a queued frame outlived its publisher program").into()
				));
			}
			let name = ready.track.name.clone();
			let kind = self.program.tracks.get(&name).context("missing track")?.kind.clone();
			if self.leads_video(&kind, &ready.item) {
				self.jitter.discard(&ready);
			} else {
				self.program.last_timestamp = Some(ready.item.frame.timestamp);
				let decode = ready.item.decode.as_nanos();
				self.lay_before(ready.item.key.as_nanos())?;
				match self.list(&name)? {
					true => self.mux(&name, ready.item)?,
					false => self
						.program
						.tracks
						.get_mut(&name)
						.expect("a listed track")
						.parked
						.push_back(ready.item),
				}
				if self.program.delay.is_zero() {
					// Without a clock to lay slots on, a frame settles every slot before its own: a
					// later frame decodes no earlier, so it can be due no earlier than this, less the
					// longest drain of any track.
					let drain = self.max_drain();
					self.lay(Some(schedule::slot(decode.saturating_sub(drain.as_nanos()))))?;
				}
			}
			if self.program.queue.is_empty() && !self.jitter.is_committed() {
				// Releasing a due frame opens room in its source. Read it before parking:
				// a zero-delay slot may need another frame to prove the slot is complete.
				let track = self.program.tracks.get_mut(&name).expect("the released frame's track");
				let finished = track.finished;
				let filled = track.fill(
					&name,
					waiter,
					&mut Input {
						waiting_for_header: false,
						video_start: self.program.video_start,
						jitter: &mut self.jitter,
						trimmed: &mut self.program.trimmed,
					},
				);
				let ended = !finished && track.finished;
				if let Err(err) = filled {
					self.leave(err)?;
				}
				if ended || self.program.retired {
					self.program
						.schedule
						.set_ended(self.program.tracks.values().all(|track| track.finished));
				}
			}
		}
		// A PES's frames leave the jitter buffer together, under its first frame's key
		// ([`Track::release_key`]), so with none due its pack is whole, and without slots
		// running on an empty schedule nothing else would send it on before it is due.
		if self.program.schedule.is_empty() {
			self.flush_packs(|_, _| true)?;
		}
		self.lay_due(waiter)?;
		self.settle()?;
		if let Some(out) = self.pop() {
			return Poll::Ready(Ok(Some(out)));
		}

		// 5. Once every track has drained, nothing more can ride the slots still open, so
		// they go out: at their times on the clock, or at once without one. That's
		// independent of the catalog: a retained track finishes while the broadcast stays
		// live, and holding its tail until the catalog closed would strand it indefinitely.
		let drained = !self.program.tracks.is_empty()
			&& self.program.tracks.values().all(|t| t.finished && t.parked.is_empty())
			&& self.jitter.is_empty()
			&& self.held.is_none();
		if drained {
			if self.slot_due().is_none() {
				self.lay(None)?;
			}
			if let Some(out) = self.pop() {
				return Poll::Ready(Ok(Some(out)));
			}
			if !self.program.schedule.is_empty() {
				return Poll::Pending;
			}
			// SI emission rides media frames, so a snapshot that arrived behind the
			// last one gets one trailing flush before the stream ends.
			if self.program.catalog_ended && !self.program.si_flushed {
				self.program.si_flushed = true;
				if let Some(out) = self.write_si_tail()? {
					return Poll::Ready(Ok(Some(out)));
				}
			}
		}

		let pending_ended = !self.jitter.is_committed()
			&& self.pending.as_ref().is_some_and(|pending| {
				let finished = pending.catalog_ended
					&& (pending.tracks.is_empty() || pending.tracks.values().all(|track| track.finished));
				let no_video_start = pending.video_out.is_none()
					&& pending
						.tracks
						.values()
						.any(|track| matches!(track.kind, Kind::Video(_)))
					&& pending
						.tracks
						.values()
						.filter(|track| matches!(track.kind, Kind::Video(_)))
						.all(|track| !self.jitter.holds(&track.key));
				finished && (self.jitter.pending_empty() || no_video_start)
			});
		if pending_ended
			&& (drained || self.program.tracks.is_empty())
			&& self.program.schedule.is_empty()
			&& self.program.queue.is_empty()
		{
			return Poll::Ready(Ok(None));
		}

		// End of stream once the catalog is closed too: nothing more can appear.
		if self.pending.is_none() && self.program.catalog_ended && (drained || self.program.tracks.is_empty()) {
			return Poll::Ready(self.program.left.take().map_or(Ok(None), Err));
		}

		Poll::Pending
	}

	/// Return the next queued frame, committing what it carries to the output boundary.
	fn pop(&mut self) -> Option<Frame> {
		let (out, tally) = self.program.queue.pop_front()?;
		if let Some(tables) = tally.tables
			&& let Some(psi) = &mut self.program.psi
		{
			psi.out = Some(tables);
		}
		self.program.emitted_epoch = self.program.epoch;
		let (pcr, discontinuity) = tally.pcr;
		if discontinuity {
			self.program.liveness.discontinuity();
		}
		self.program.liveness.written_pcr(pcr);
		for pid in tally.units {
			self.program.liveness.delivered(pid, 1);
		}
		Some(out)
	}

	/// The trailing SI frame for end of stream: every entry's current sections,
	/// re-emitted once. Duplicates of already-emitted sections are harmless
	/// repetitions; `None` when there are no tables or no program was ever built.
	fn write_si_tail(&mut self) -> anyhow::Result<Option<Frame>> {
		if self.program.psi.is_none() {
			return Ok(None);
		}
		let pending: Vec<(u16, Vec<Bytes>)> = self
			.program
			.si
			.iter()
			.filter(|(_, si)| !si.active.is_empty())
			.map(|((pid, _), si)| (*pid, si.active.sections().cloned().collect()))
			.collect();
		if pending.is_empty() {
			return Ok(None);
		}
		let timestamp = self.program.last_timestamp.unwrap_or(Timestamp::ZERO);
		let mut out = Vec::new();
		for (pid, sections) in pending {
			for section in &sections {
				self.write_section(&mut out, pid, section)?;
			}
		}
		self.number(&mut out);
		Ok(Some(Frame {
			timestamp,
			duration: None,
			payload: Bytes::from(out),
			keyframe: false,
		}))
	}

	fn prepare_pending(&mut self, waiter: &kio::Waiter) -> crate::Result<()> {
		let mut pending = self.pending.take().expect("a selected program");
		std::mem::swap(&mut self.program, pending.as_mut());
		let result = self.prepare(waiter);
		std::mem::swap(&mut self.program, pending.as_mut());
		self.pending = Some(pending);
		result.map(|_| ())
	}

	fn selected(&self) -> &Program<S> {
		self.pending.as_deref().unwrap_or(&self.program)
	}

	fn prepare(&mut self, waiter: &kio::Waiter) -> crate::Result<bool> {
		if self.program.retired || (self.program.id == self.active_program && self.jitter.is_committed()) {
			return Ok(true);
		}
		// 1. Drain catalog updates, discovering the track layout.
		while !self.program.catalog_ended {
			match self
				.program
				.catalog
				.as_mut()
				.expect("an open catalog")
				.poll_next(waiter)?
			{
				Poll::Ready(Some(snapshot)) => {
					self.program.cataloged = true;
					self.update_catalog(snapshot)?
				}
				Poll::Ready(None) => {
					self.program.catalog_ended = true;
					break;
				}
				Poll::Pending => break,
			}
		}

		// 1b. Drive the SI subscriptions, folding arrived snapshots into each
		// entry's active set (emission happens on cadence in `mux`).
		//
		// Deliberately not part of the first-frame gate below: nothing in SI is
		// something a TS stream cannot begin without (the PAT/PMT are built locally,
		// and receivers acquire the service layer mid-stream by design), whereas
		// gating on it would let a catalog entry that never delivers (a stale
		// announce naming a dead track) hold the programme dark. An entry that
		// resolves late simply starts emitting on its cadence from then on, at most
		// one subscription round-trip behind the media.
		for si in self.program.si.values_mut() {
			si.poll(waiter);
		}

		if self.program.expected_changed {
			if self.program.id == self.active_program {
				self.jitter.expect(self.program.expected.clone());
				self.program.expected_changed = false;
			} else if self.jitter.has_pending() || self.jitter.is_committed() {
				self.jitter.expect_pending(
					self.program.tracks.values().map(|track| track.key.clone()),
					self.program.expected.clone(),
				);
				self.program.expected_changed = false;
			}
		}

		// 2. Read each source until it has a frame due, or before the program tables
		// are built, hold one per track.
		self.fill(waiter)?;

		// 3. Build the program tables once the layout is resolved and every
		// track's codec config is ready. The tables aren't emitted here: PSI has
		// no media time of its own, so `mux` prepends them to the first frame's
		// packets instead, letting the leading PAT/PMT inherit a real timestamp.
		if self.program.psi.is_none() {
			if self.program.tracks.is_empty() {
				// No tracks yet. If the catalog is also done, the broadcast is empty.
				if self.program.catalog_ended {
					return Ok(false);
				}
				return Ok(false);
			}
			if !self.header_ready() || !self.video_ready() {
				// Hold all output (tables and audio alike) until codec configs resolve
				// and, when the program has a video rendition, its first keyframe is
				// buffered: the stream must begin on that keyframe so the in-band
				// parameter sets lead it. An audio-only program has nothing to wait for.
				// If every track finished without producing a config, it can't be muxed.
				if self.program.catalog_ended && self.program.tracks.values().all(|t| t.finished) {
					return Ok(false);
				}
				return Ok(false);
			}
			self.build_psi()?;
			// Anchor tune-in to the first video keyframe and drop any non-video frame
			// already buffered ahead of it (see `video_start`).
			self.program.video_start = self.first_video_pts();
			if let Some(start) = self.program.video_start {
				for track in self.program.tracks.values_mut() {
					if !matches!(track.kind, Kind::Video(_))
						&& !track.kind.placed()
						&& track.pending.as_ref().is_some_and(|p| p.frame.timestamp < start)
					{
						track.pending = None;
						self.program.trimmed += 1;
					}
				}
			}
			if self.program.id != self.active_program && !self.jitter.has_pending() && !self.jitter.is_committed() {
				tracing::debug!(target: "moq_mux::container::ts::handoff", phase = "prepared", now = ?moq_clock::now(), program = self.program.id, old = self.active_program, tracks = self.program.tracks.len(), video_start = ?self.program.video_start);
				self.jitter.start(
					self.program.tracks.values().map(|track| track.key.clone()),
					self.program.expected.clone(),
				);
				self.program.expected_changed = false;
			}

			// Hand the held frames to the jitter buffer in arrival order, so the
			// earliest anchors its clock, then read on.
			let mut held: Vec<(moq_clock::Instant, String)> = self
				.program
				.tracks
				.iter()
				.filter_map(|(name, t)| t.pending.as_ref().map(|p| (p.arrived, name.clone())))
				.collect();
			held.sort();
			for (_, name) in held {
				let track = self.program.tracks.get_mut(&name).unwrap();
				let pending = track.pending.take().unwrap();
				track.queue(&name, pending, &mut self.jitter)?;
			}
			self.fill(waiter)?;
		}

		Ok(true)
	}

	/// The program playing failed with `err`. Only a publisher that left is survivable: its
	/// input stops at once, as a follow that runs out stops it, but what it already buffered
	/// goes out on schedule. Without a replacement, the export then reports `err`, so a
	/// [`Follower`](super::Follower) waits for a return only after the old output has ended,
	/// rather than leaving it buffered to go out late, behind the gap, once something returns.
	///
	/// The program keeps its catalog, whose format the follower subscribes a return with.
	fn leave(&mut self, err: crate::Error) -> crate::Result<()> {
		if !super::follower::left(&err) {
			return Err(err);
		}
		tracing::debug!(target: "moq_mux::container::ts::handoff", phase = "left", now = ?moq_clock::now(), program = self.program.id, committed = self.jitter.is_committed(), %err);
		if self.pending.is_none() {
			self.program.left = Some(err);
		}
		self.follow_timer = None;
		self.follow_until = None;
		self.retire()
	}

	fn stop_old(&mut self, waiter: &kio::Waiter) -> crate::Result<()> {
		if self.program.retired {
			return Ok(());
		}

		let expired = self.follow_until.is_some_and(|until| moq_clock::now() >= until);
		if !expired {
			if let Some(until) = self.follow_until {
				let timer = self
					.follow_timer
					.get_or_insert_with(|| Box::pin(moq_clock::sleep_until(until)));
				if timer.deadline() != until {
					timer.as_mut().reset(until);
				}
				let _ = waiter.poll_future(timer.as_mut());
			}
			return Ok(());
		}
		tracing::debug!(target: "moq_mux::container::ts::handoff", phase = "follow-expired", now = ?moq_clock::now(), program = self.program.id, committed = self.jitter.is_committed());
		self.follow_timer = None;
		self.follow_until = None;
		self.program.catalog = None;
		self.retire()
	}

	/// Stop the program's input. What it already buffered still goes out, unless a
	/// committed replacement's switch drops it.
	fn retire(&mut self) -> crate::Result<()> {
		self.program.retired = true;
		self.program.catalog_ended = true;
		for (name, track) in &mut self.program.tracks {
			if !track.finished {
				if !self.jitter.is_committed() {
					track.release(name, &mut self.jitter, true)?;
				}
				track.source.stop();
				track.finished = true;
			}
		}
		for si in self.program.si.values_mut() {
			si.state = SiState::Done;
		}
		Ok(())
	}

	fn adopt(&mut self, ready: &jitter::Ready<TrackKey, Queued>) -> crate::Result<()> {
		let Some(pending) = self.pending.as_ref() else {
			return Ok(());
		};
		if ready.track.program != pending.id {
			return Ok(());
		}
		let mut next = self.pending.take().expect("checked above");
		tracing::debug!(target: "moq_mux::container::ts::handoff", phase = "adopt", now = ?moq_clock::now(), old = self.program.id, new = next.id, generation = ready.generation, old_pts = ?self.program.last_timestamp, new_pts = ?ready.item.frame.timestamp, new_key = ?ready.item.key);
		let out = self
			.program
			.psi
			.as_ref()
			.and_then(|psi| psi.out.as_ref())
			.map(|laid| laid.version);
		next.version = out.unwrap_or(self.program.version);
		next.epoch = self.program.epoch;
		next.emitted_epoch = self.program.emitted_epoch;
		if out.is_some() {
			next.version.increment();
			next.epoch += 1;
			next.flagged = Some(HashSet::new());
			next.pcr_discontinuity = true;
		}
		// Tables were prepared under the incoming layout, but no packet has left it yet.
		next.psi = None;
		self.active_program = next.id;
		self.jitter.adopted();
		self.program = *next;
		self.program.generation = ready.generation;
		self.follow_until = None;
		self.follow_timer = None;
		self.build_psi()?;
		Ok(())
	}

	/// Read each source until it has a frame due.
	///
	/// Before the program tables are built, each track holds its first frame instead,
	/// and slices that arrive before their codec config resolves are dropped: a receiver
	/// joining mid-GOP can't use them, and parking them would stop us polling for the
	/// keyframe that carries the parameter sets. [`ExportSource`] has already transformed
	/// Annex-B avc3/hev1 into length-prefixed form and resolved the avcC/hvcC.
	fn fill(&mut self, waiter: &kio::Waiter) -> crate::Result<()> {
		let mut input = Input {
			waiting_for_header: self.program.psi.is_none(),
			video_start: self.program.video_start,
			jitter: &mut self.jitter,
			trimmed: &mut self.program.trimmed,
		};
		for (name, track) in &mut self.program.tracks {
			track.fill(name, waiter, &mut input)?;
		}
		Ok(())
	}

	fn update_catalog(&mut self, mut catalog: Catalog<S::Ext>) -> anyhow::Result<()> {
		// The MPEG-TS section lives in the extension. The trait only exposes
		// `mpegts_mut`, and this snapshot is owned, so clone it out (`()` yields the
		// empty default: no verbatim streams, no preserved PIDs/descriptors).
		let mpegts = catalog.ext.mpegts_mut().cloned().unwrap_or_default();
		reject_ambiguous_tracks(&catalog, &mpegts)?;
		self.program.source.retain_selectable(&mut catalog);

		self.program.program_descriptors = mpegts.program_descriptors.clone();
		self.program.program = mpegts.program.clone();
		// An explicit override wins even when it is refused (leaving the output
		// unpadded), so a bad flag cannot silently fall back to the catalog rate.
		let mux_rate = match self.program.mux_rate_override {
			Some(override_rate) => sanitize_mux_rate(override_rate),
			None => mpegts.mux_rate.and_then(|rate| {
				sanitize_mux_rate(rate).or_else(|| {
					tracing::warn!(mux_rate = rate, "ignoring invalid MPEG-TS multiplex rate in catalog");
					None
				})
			}),
		};
		// A revision is one boundary: the rate rides the next tables out.
		if mux_rate != self.program.mux_rate {
			self.program.mux_rate = mux_rate;
			self.program.revised = true;
		}
		if self.program.psi.is_none() {
			self.program.schedule.set_rate(self.program.mux_rate);
		}

		// Reconcile the SI subscriptions with the catalog's map. Entries may appear
		// after the PAT/PMT is built (a table acquired late): they ride standalone
		// PIDs outside the program, so unlike the elementary tracks below they are
		// not part of the latched layout.
		self.program
			.si
			.retain(|key, _| mpegts.si.get(&key.0).is_some_and(|t| t.contains_key(&key.1)));
		reject_colliding_si_pids(&mpegts, self.pmt_pid(), &[])?;
		for (pid, tables) in mpegts.si.iter() {
			for (table_id, entry) in tables.iter() {
				match self.program.si.get_mut(&(*pid, *table_id)) {
					// A repointed entry (same key, new track) rebuilds the subscription:
					// the old track is a restarted or replaced publisher's leftover, and
					// staying attached would repeat its stale sections forever. The last
					// snapshot carries across so emission never goes dark mid-swap.
					Some(existing) if existing.track != entry.track => {
						let mut replacement = SiTrack::new(&self.program.source, entry, self.program.delay);
						// An inline entry already holds its snapshot; only a track entry
						// has nothing to emit until its first group lands.
						if replacement.active.is_empty() {
							replacement.active = std::mem::take(&mut existing.active);
							replacement.dirty = existing.dirty;
						} else {
							replacement.dirty = replacement.active != existing.active;
						}
						replacement.last_emit = existing.last_emit;
						*existing = replacement;
					}
					Some(existing) => {
						existing.interval = entry.interval;
						// Inline sections arrive with the catalog itself, so a revised
						// table shows up here rather than on a track.
						if !entry.sections.is_empty() {
							let snapshot = SiTrack::inline(entry);
							if existing.active != snapshot {
								existing.active = snapshot;
								existing.dirty = true;
							}
						}
					}
					None => {
						self.program.si.insert(
							(*pid, *table_id),
							SiTrack::new(&self.program.source, entry, self.program.delay),
						);
					}
				}
			}
		}

		// The desired track set: media renditions plus the verbatim streams.
		let mut active: BTreeMap<String, ()> = BTreeMap::new();
		for name in catalog.video.renditions.keys() {
			active.insert(name.clone(), ());
		}
		for name in catalog.audio.renditions.keys() {
			active.insert(name.clone(), ());
		}
		for (name, track) in mpegts.tracks.iter() {
			if track.verbatim.is_some() || track.pes_packet.is_some() {
				active.insert(name.clone(), ());
			}
		}

		if self.program.psi.is_some() {
			if !self.program.stale.is_empty() {
				self.resubscribe(&catalog, &mpegts)?;
			}
			self.rebind(&catalog)?;
			// A listed track keeps its PID, kind and descriptors, which its packets went out
			// under, even when the catalog moves it to another track: a stream the source
			// revises comes under a new name. The timing is not fixed: a `jitter` or
			// `framerate` published after the tables still sizes the decode clock.
			for (name, config) in catalog.video.renditions.iter() {
				if let Some(track) = self.program.tracks.get_mut(name).filter(|track| track.listed) {
					track.timing.configure(config, name)?;
				}
			}
			// A track that leaves the catalog is read to its own end. A publisher retires a
			// rendition as its track finishes or drops, and the catalog update races that end
			// on another stream, so the leaving cannot say which it was. The track's end does:
			// a finish ends it cleanly, and a drop is the error the export reports. One the PMT
			// does not list yet is read no further, so a publisher that never ends what it
			// withdraws leaves nothing behind but what it already sent, which still goes out. A
			// name that returns before that is gone is the same stream, so it is read on.
			for (name, track) in self.program.tracks.iter_mut() {
				track.cataloged = active.contains_key(name);
				if track.cataloged && std::mem::take(&mut track.withdrawn) {
					track.finished = false;
				}
				if !track.cataloged && !track.listed && !track.finished {
					track.release(name, &mut self.jitter, true)?;
					track.finished = true;
					track.withdrawn = true;
				}
			}
			self.revise()?;
		}

		// Assign a PID to every desired track the PMT does not list yet: prefer the original
		// recorded in the `mpegts` section, then fill the rest from FIRST_ES_PID. The importer
		// fills PIDs, descriptors, and stream_ids across several catalog publishes, so this runs
		// every snapshot until the PMT lists the track, which is *refreshed*, not latched from
		// the first (partial) snapshot. A recorded PID may take over one a stream the catalog
		// dropped still drains on ([`Self::list`]); any other PID the PMT lists is taken.
		let pmt_pid = self.pmt_pid();
		let mut taken: Vec<u16> = vec![0x0000, pmt_pid, 0x1FFF];
		taken.extend(self.program.psi.as_ref().and_then(|psi| psi.own_clock()));
		taken.extend(
			self.program
				.tracks
				.values()
				.filter(|t| t.listed && t.cataloged)
				.map(|t| t.pid),
		);
		let draining: Vec<u16> = self
			.program
			.tracks
			.values()
			.filter(|t| t.listed && !t.cataloged)
			.map(|t| t.pid)
			.collect();
		let joining: Vec<&String> = active
			.keys()
			.filter(|name| self.program.tracks.get(*name).is_none_or(|track| !track.listed))
			.collect();
		let mut pids: BTreeMap<String, u16> = BTreeMap::new();
		for &name in &joining {
			if let Some(pid) = mpegts.tracks.get(name).map(|t| t.pid)
				&& !taken.contains(&pid)
			{
				taken.push(pid);
				pids.insert(name.clone(), pid);
			}
		}
		for &name in &joining {
			if !pids.contains_key(name) {
				let mut pid = FIRST_ES_PID;
				while taken.contains(&pid) || draining.contains(&pid) {
					pid += 1;
				}
				taken.push(pid);
				pids.insert(name.clone(), pid);
			}
		}
		let es_pids: Vec<u16> = taken
			.iter()
			.chain(&draining)
			.copied()
			.filter(|&pid| pid >= 0x10 && pid != 0x1FFF)
			.collect();
		reject_colliding_si_pids(&mpegts, pmt_pid, &es_pids)?;

		// Reuse each joining track's source (and any pending frame) by name; refresh its PID,
		// kind, and descriptors from this snapshot, and start over on one the catalog moved to
		// another track. Before the tables, drop tracks no longer present; after them, a track
		// the catalog dropped stays until what it holds is gone ([`Self::settle`]).
		let mut old: HashMap<String, Track> = self
			.program
			.tracks
			.extract_if(|name, track| !track.listed && active.contains_key(name))
			.collect();
		if self.program.psi.is_none() {
			self.program.tracks.clear();
		}
		for (name, config) in catalog.video.renditions.iter() {
			let Some(&pid) = pids.get(name) else {
				continue;
			};
			let kind = video_kind(config, name)?;
			let reused = old
				.remove(name)
				.filter(|track| track.source.reads(&self.program.source, name, config));
			let mut track = match reused {
				Some(track) => track,
				None => match ExportSource::for_video(&self.program.source, name, config, self.budget())?
					.map(|s| at_edge(s, self.program.replay))
				{
					Some(source) => Track::new(source, kind.clone()),
					None => continue,
				},
			};
			track.kind = kind;
			track.timing.configure(config, name)?;
			self.refresh(name, track, pid, track_descriptors(&mpegts, name));
		}
		for (name, config) in catalog.audio.renditions.iter() {
			let Some(&pid) = pids.get(name) else {
				continue;
			};
			let kind = audio_kind(config, name)?;
			let reused = old
				.remove(name)
				.filter(|track| track.source.reads(&self.program.source, name, config));
			let mut track = match reused {
				Some(track) => track,
				None => match ExportSource::for_audio(&self.program.source, name, config, self.budget())?
					.map(|s| at_edge(s, self.program.replay))
				{
					Some(source) => Track::new(source, kind.clone()),
					None => continue,
				},
			};
			track.buffer = Some(audio_buffer(config, &kind));
			track.kind = kind;
			self.refresh(name, track, pid, track_descriptors(&mpegts, name));
		}
		for (name, entry) in mpegts.tracks.iter() {
			let Some(&pid) = pids.get(name) else {
				continue;
			};
			let kind = match (&entry.verbatim, &entry.pes_packet) {
				(Some(verbatim), _) => Kind::Verbatim {
					stream_type: verbatim.stream_type,
				},
				(None, Some(pes)) => Kind::PesPacket {
					stream_type: pes.stream_type,
				},
				(None, None) => continue,
			};
			let mut track = match old.remove(name) {
				Some(track) => track,
				None => Track::new(
					ExportSource::for_stream(&self.program.source, name, self.budget())?,
					kind.clone(),
				),
			};
			track.buffer = verbatim_buffer(&kind, &entry.descriptors);
			track.kind = kind;
			self.refresh(name, track, pid, entry.descriptors.clone());
		}
		// The clock anchors on whichever track a source sends latest against its decode time
		// (audio just in time while video runs most of a second ahead), so it waits to hear
		// from each continuous one: audio, video, and the passthrough streams the schedule
		// models (DVB AC-3, teletext). Not from sparse ones such as SCTE-35, subtitles or ID3,
		// which would hold every join for the full bound.
		let continuous = self
			.program
			.tracks
			.iter()
			.filter(|(_, track)| !track.finished)
			.filter(|(_, track)| track.kind.class() != super::stats::Class::Data || track.buffer.is_some());
		self.program.expected = continuous.map(|(_, track)| track.key.clone()).collect();
		self.program.expected_changed = true;
		Ok(())
	}

	/// Keep `track` under `name` with this snapshot's PID and descriptors.
	fn refresh(&mut self, name: &str, mut track: Track, pid: u16, descriptors: Vec<catalog::Descriptor>) {
		track.key = TrackKey::new(self.program.id, name);
		track.pid = pid;
		track.descriptors = descriptors;
		self.program.tracks.insert(name.to_string(), track);
	}

	/// Point each stale track this snapshot lists at the returned broadcast.
	///
	/// The PIDs and PMT stay as announced. A track the returned catalog does not list stays
	/// finished, silent on its PID. The new subscription's playhead jumped from wherever the
	/// old one stopped, so its first frame counts as a skip: late frames drop, and a leap
	/// ahead opens a new generation.
	fn resubscribe(&mut self, catalog: &Catalog<S::Ext>, mpegts: &catalog::Mpegts) -> anyhow::Result<()> {
		let budget = self.budget();
		for (name, track) in self.program.tracks.iter_mut() {
			if !self.program.stale.contains(name) {
				continue;
			}
			let source = if let Some(config) = catalog.video.renditions.get(name) {
				ExportSource::for_video(&self.program.source, name, config, budget)?
					.map(|s| at_edge(s, self.program.replay))
			} else if let Some(config) = catalog.audio.renditions.get(name) {
				ExportSource::for_audio(&self.program.source, name, config, budget)?
					.map(|s| at_edge(s, self.program.replay))
			} else if mpegts
				.tracks
				.get(name)
				.is_some_and(|t| t.verbatim.is_some() || t.pes_packet.is_some())
			{
				Some(ExportSource::for_stream(&self.program.source, name, budget)?)
			} else {
				None
			};
			let Some(source) = source else {
				continue;
			};
			self.program.stale.remove(name);
			// The new subscription counts as cutting whatever group the old one was reading.
			track.base.0 += track.source.markers();
			track.base.1 += track.source.skips() + 1;
			track.base.2 += track.source.cuts() + 1;
			track.source = source;
			track.subscribed = moq_clock::now();
			track.finished = false;
		}
		Ok(())
	}

	/// Move each listed track whose rendition the catalog moved to another track or broadcast
	/// onto it. One the PMT does not list yet starts over on the new track instead.
	///
	/// The PID, PMT and lane stay as announced, and so does the kind the track's frames are
	/// framed under as they go out, so the new track must have the same kind: an AAC track keeps
	/// its configuration. Video parameter sets ride with each frame and may change. The new
	/// source continues the old one ([`ExportSource::continuing`]), and the frames the old one
	/// left held for their DTS go out first.
	fn rebind(&mut self, catalog: &Catalog<S::Ext>) -> anyhow::Result<()> {
		let budget = self.budget();
		for (name, track) in self.program.tracks.iter_mut().filter(|(_, track)| track.listed) {
			let (source, kind) = if let Some(config) = catalog.video.renditions.get(name) {
				if track.source.reads(&self.program.source, name, config) {
					continue;
				}
				let source = ExportSource::for_video(&self.program.source, name, config, budget)?;
				(source, video_kind(config, name)?)
			} else if let Some(config) = catalog.audio.renditions.get(name) {
				if track.source.reads(&self.program.source, name, config) {
					continue;
				}
				let source = ExportSource::for_audio(&self.program.source, name, config, budget)?;
				(source, audio_kind(config, name)?)
			} else {
				continue;
			};
			let Some(source) = source else {
				continue;
			};
			let same = match (&kind, &track.kind) {
				(Kind::Aac { config, .. }, Kind::Aac { config: old, .. }) => config == old,
				(kind, old) => kind == old,
			};
			anyhow::ensure!(
				same,
				"TS track layout changed after PAT/PMT was emitted: '{name}' moved to a track announced differently"
			);
			track.release(name, &mut self.jitter, true)?;
			track.clock = DecodeClock::default();
			track.declared = None;
			track.source = source.continuing(&track.source);
			track.finished = false;
			track.subscribed = moq_clock::now();
		}
		Ok(())
	}

	/// How many frames were dropped for missing their deadline.
	#[cfg(test)]
	pub(super) fn dropped(&self) -> u64 {
		self.jitter.dropped()
	}

	/// Snapshot the access units each elementary stream has written, how long each has been
	/// quiet on the PCR the output carries, and how the release clock is keeping up with the
	/// source.
	///
	/// A track stalled upstream stops advancing its row while the PSI and the other PIDs
	/// keep flowing, which nothing graded on the output bytes alone can see. The rows are
	/// empty until the program tables are built. Cheap enough to poll per frame.
	pub fn stats(&self) -> super::stats::Export {
		let mut stats = super::stats::Export {
			release: super::stats::Release::of(&self.jitter),
			..Default::default()
		};
		stats.release.trimmed += self.program.trimmed;
		stats.release.gaps = self
			.program
			.tracks
			.values()
			.map(|track| {
				let skips = track.base.1 + track.source.skips();
				skips - (track.base.0 + track.source.markers())
			})
			.sum();
		if self.program.psi.is_none() {
			return stats;
		}
		for track in self.program.tracks.values().filter(|track| track.listed) {
			let (units, quiet) = self.program.liveness.stream(track.pid);
			let row = super::stats::Stream {
				track: track.kind.suffix().to_string(),
				class: track.kind.class(),
				units,
				quiet,
				..Default::default()
			};
			stats.streams.insert(track.pid, row);
		}
		stats
	}

	/// How many access units are waiting in the jitter buffer or a decode clock.
	#[cfg(test)]
	pub(super) fn buffered(&self) -> usize {
		self.jitter.len()
			+ self
				.program
				.tracks
				.values()
				.map(|track| track.clock.len())
				.sum::<usize>()
	}

	/// How many tracks the export carries, listed or not.
	#[cfg(test)]
	pub(super) fn tracks(&self) -> usize {
		self.program.tracks.len()
	}

	/// `pid`'s access units and how long it has been quiet, as [`Self::stats`] reads them.
	#[cfg(test)]
	pub(super) fn liveness(&self, pid: u16) -> (u64, Option<Duration>) {
		self.program.liveness.stream(pid)
	}

	/// The track the PMT lists on `pid`.
	#[cfg(test)]
	pub(super) fn listed_on(&self, pid: u16) -> Option<&str> {
		self.program
			.tracks
			.iter()
			.find(|(_, track)| track.listed && track.pid == pid)
			.map(|(name, _)| name.as_str())
	}

	/// Take the next frame due out of the jitter buffer and hold it, as a generation boundary
	/// holds the new generation's first frame behind the old one's tail. Returns its PID.
	#[cfg(test)]
	pub(super) fn hold_next(&mut self) -> Option<u16> {
		let Poll::Ready(ready) = self.jitter.poll_next(&kio::Waiter::noop()) else {
			return None;
		};
		let pid = self.program.tracks.get(&ready.track.name)?.pid;
		self.held = Some(ready);
		Some(pid)
	}

	#[cfg(test)]
	pub(super) fn holding_replacement(&self) -> bool {
		self.held
			.as_ref()
			.is_some_and(|ready| self.pending.as_ref().is_some_and(|next| ready.track.program == next.id))
	}

	/// When the next queued frame is due, or the next grid slot while media is queued or
	/// every source has finished and the tail runs out.
	#[cfg(test)]
	pub(super) fn next_due(&self) -> Option<moq_clock::Instant> {
		let finished = !self.program.tracks.is_empty() && self.program.tracks.values().all(|track| track.finished);
		let slot = self
			.slot_due()
			.filter(|_| self.program.schedule.queued() || self.packing() || finished)
			.map(|(_, at)| at);
		self.jitter.next_deadline().into_iter().chain(slot).min()
	}

	/// The discontinuity counter of the most recently returned output frame.
	/// Compare across reads and re-anchor pacing when it changes. Renditions have
	/// independent source counters; this counter describes the emitted program.
	pub fn discontinuity(&self) -> u64 {
		self.program.emitted_epoch
	}

	pub(super) fn source(&self) -> &crate::Source {
		&self.selected().source
	}

	/// The publisher epoch of the broadcast being exported, `None` for an epochless route.
	pub(super) fn instance(&self) -> Option<&moq_net::Epoch> {
		self.selected().instance.as_ref()
	}

	/// Carry on into `broadcast`, using the caller's replacement `catalog` stream.
	///
	/// The same publisher instance (an equal epoch) continues the stream: the program and
	/// its PSI carry on, and its catalog resubscribes each track as it lists it, so a gap is a
	/// skip like any other. A track it adds or drops revises the PMT, as any catalog update
	/// does.
	///
	/// Another instance (another epoch, or an epochless route) opens a fresh jitter generation.
	/// [`Follower`](super::Follower) keeps feeding the old program until the new clock and
	/// first media are usable, bounded by its follow window. Then what the old program
	/// already released goes out before the new PMT, and what it still buffers is dropped,
	/// so the new program keeps its own delay. The new PMT has advanced table versions,
	/// every PID's first packet flags the break, and each stream starts on a keyframe. No
	/// timestamps are aligned.
	pub fn follow(mut self, broadcast: moq_net::broadcast::Consumer, catalog: S) -> Result<Self, crate::Error> {
		self.followed(&broadcast, catalog, Duration::ZERO)?;
		Ok(self)
	}

	/// Whether the catalog this export last subscribed has delivered a snapshot.
	pub(super) fn cataloged(&self) -> bool {
		self.selected().cataloged
	}

	/// [`Self::follow`], once `broadcast`'s catalog is subscribed.
	pub(super) fn followed(
		&mut self,
		broadcast: &moq_net::broadcast::Consumer,
		catalog: S,
		follow: Duration,
	) -> Result<(), crate::Error> {
		tracing::debug!(target: "moq_mux::container::ts::handoff", phase = "follow", now = ?moq_clock::now(), program = self.program.id, pending = ?self.pending.as_ref().map(|pending| pending.id), old_epoch = ?self.program.instance, new_epoch = ?broadcast.info().epoch, ?follow);
		if self.pending.is_some() && self.program.instance.is_some() && broadcast.info().epoch == self.program.instance
		{
			self.cancel_pending();
			self.follow_until = None;
			self.follow_timer = None;
		}
		let same = self.selected().instance.is_some() && broadcast.info().epoch == self.selected().instance;
		if !same {
			self.cancel_pending();
			let next = self.replacement(self.program.source.switched(broadcast), catalog);
			self.pending = Some(Box::new(next));
			self.follow_until.get_or_insert_with(|| moq_clock::now() + follow);
			return Ok(());
		}
		if let Some(mut pending) = self.pending.take() {
			pending.source.pin(broadcast);
			if pending.psi.is_none() {
				if self
					.held
					.as_ref()
					.is_some_and(|ready| ready.track.program == pending.id)
				{
					self.held = None;
				}
				self.jitter.cancel();
				let next = self.replacement(pending.source, catalog);
				self.pending = Some(Box::new(next));
			} else {
				std::mem::swap(&mut self.program, pending.as_mut());
				let resumed = self.resume(catalog);
				std::mem::swap(&mut self.program, pending.as_mut());
				self.pending = Some(pending);
				resumed?;
			}
			return Ok(());
		}
		self.program.source.pin(broadcast);
		if self.program.psi.is_none() {
			self.program = self.replacement(self.program.source.clone(), catalog);
			self.active_program = self.program.id;
			self.held = None;
			self.jitter = jitter::Buffer::new(self.program.delay);
			#[cfg(test)]
			if self.program.replay {
				self.jitter = jitter::Buffer::new(self.program.delay).replay();
			}
			return Ok(());
		}
		self.resume(catalog)
	}

	fn cancel_pending(&mut self) {
		if let Some(previous) = self.pending.take() {
			if self
				.held
				.as_ref()
				.is_some_and(|ready| ready.track.program == previous.id)
			{
				self.held = None;
			}
			self.jitter.cancel();
		}
	}

	fn replacement(&mut self, source: crate::Source, catalog: S) -> Program<S> {
		let mut next = Program::new(source, catalog);
		next.id = self.next_program;
		self.next_program += 1;
		next.delay = self.program.delay;
		next.replay = self.program.replay;
		next.mux_rate_override = self.program.mux_rate_override;
		next.schedule = Schedule::new(next.delay);
		next
	}

	fn resume(&mut self, catalog: S) -> Result<(), crate::Error> {
		self.program.catalog = Some(catalog);
		self.program.catalog_ended = false;
		self.program.cataloged = false;
		self.program.retired = false;
		self.program.left = None;
		self.program.si_flushed = false;
		for si in self.program.si.values_mut() {
			si.track.clear();
		}
		for (name, track) in &mut self.program.tracks {
			track.release(name, &mut self.jitter, true)?;
			track.finished = true;
			track.withdrawn = false;
			self.program.stale.insert(name.clone());
		}
		Ok(())
	}

	/// Whether a frame of `kind` released now presents ahead of the video keyframe the stream
	/// begins on (`video_start`), and so is dropped. A video track whose start group
	/// went stale before it went out starts at its next keyframe instead
	/// ([`jitter::Buffer`]), so until video goes out the start is the earliest frame a video
	/// track holds, and with none held, everything leads the keyframe yet to arrive. Video goes
	/// out further ahead of its decode time than the other tracks, so what they release before
	/// it presents ahead of it too.
	///
	/// A placed frame has no presentation to compare: it goes out at its placement, so it is
	/// dropped only when that comes before the keyframe departs. Both are release times a
	/// delay before their jitter buffer keys, so it compares keys, and keeps whatever goes
	/// out beside or after the keyframe even when its PTS is earlier.
	fn leads_video(&mut self, kind: &Kind, queued: &Queued) -> bool {
		let Some(start) = self.program.video_start else {
			return false;
		};
		if matches!(kind, Kind::Video(_)) {
			if self.program.video_out.is_none() {
				self.program.video_out = Some(queued.key);
				self.program.video_start = Some(start.max(queued.frame.timestamp));
			}
			return false;
		}
		let placed = kind.placed();
		if let Some(departed) = self.program.video_out {
			return match placed {
				true => queued.key < departed,
				false => queued.frame.timestamp < start,
			};
		}
		let next = self
			.program
			.tracks
			.iter()
			.filter(|(_, track)| matches!(track.kind, Kind::Video(_)))
			.filter_map(|(_, track)| self.jitter.peek(&track.key))
			.map(|next| match placed {
				true => next.key,
				false => next.frame.timestamp.max(start),
			})
			.min();
		let at = match placed {
			true => queued.key,
			false => queued.frame.timestamp,
		};
		next.is_none_or(|next| at < next)
	}

	/// Discard what has not gone out and restart the program clock. Every rendition
	/// joins the new generation: no track is fenced across a declared marker, since a
	/// latency skip on one track says nothing about another's timeline. The continuity
	/// counters run on, since they number only what went out.
	fn rewind(&mut self) {
		self.program.epoch += 1;
		self.program.schedule.clear();
		// A break flag queued on a PID that has not gone out yet was just dropped with it.
		if let Some(flagged) = &mut self.program.flagged {
			flagged.clear();
		}
		self.program.queue.clear();
		self.program.last_psi = None;
		for si in self.program.si.values_mut() {
			si.last_emit = None;
		}
		self.program.video_start = None;
		self.program.pcr_discontinuity = true;
		self.program.tables.clear();
		if let Some(psi) = &mut self.program.psi {
			psi.sending.clear();
			psi.laid = psi.out.clone();
			psi.led = psi.out.as_ref().map(|laid| laid.pmt.clone());
		}
		for track in self.program.tracks.values_mut() {
			track.pack = None;
			track.parked.clear();
		}
	}

	/// Header is ready when every track's [`ExportSource`] has resolved its
	/// codec config (from the catalog `description`, or built by the transform).
	fn header_ready(&self) -> bool {
		self.program.tracks.values().all(|t| t.source.header_ready())
	}

	/// Every video track has buffered its first frame (the keyframe) or finished.
	/// The tables wait for this so the tune-in point (`video_start`) can be
	/// read from the keyframe before any audio is emitted ahead of it. A program
	/// with no video track is trivially ready.
	fn video_ready(&self) -> bool {
		self.program
			.tracks
			.values()
			.filter(|t| matches!(t.kind, Kind::Video(_)))
			.all(|t| t.pending.is_some() || t.finished)
	}

	/// The smallest timestamp among the video tracks' buffered frames: the first
	/// video keyframe, since pre-keyframe video frames are dropped before the tables
	/// are built. `None` when no video track has a buffered frame (audio-only program).
	fn first_video_pts(&self) -> Option<Timestamp> {
		self.program
			.tracks
			.values()
			.filter(|t| matches!(t.kind, Kind::Video(_)))
			.filter_map(|t| t.pending.as_ref().map(|p| p.frame.timestamp))
			.min()
	}

	/// Whether `pid`'s next packet is its first since a switch to another instance, so it has
	/// to flag the break. The PCR PID's first packet always opens a slot with its PCR, flagged
	/// on its own ([`Self::lay`]).
	fn breaks(&mut self, pid: u16) -> bool {
		let owes = self.owes_break(pid);
		if owes && let Some(flagged) = &mut self.program.flagged {
			flagged.insert(pid);
		}
		owes
	}

	/// Whether `pid`'s next packet has to flag the break ([`Self::breaks`]), without spending it.
	fn owes_break(&self, pid: u16) -> bool {
		let pcr = self.program.psi.as_ref().and_then(Psi::pcr_pid);
		self.program.flagged.as_ref().is_some_and(|flagged| {
			Some(pid) != pcr && !self.program.counters.contains_key(&pid) && !flagged.contains(&pid)
		})
	}

	/// PID the PMT rides on: the source's original (preserved in the service record),
	/// or the synthesized [`PMT_PID`] for a media-only source or an invalid (zero) value.
	/// Once the tables are built they keep the PID they went out on.
	fn pmt_pid(&self) -> u16 {
		if let Some(psi) = &self.program.psi {
			return psi.pmt_pid;
		}
		self.program
			.program
			.as_ref()
			.map(|s| s.pmt_pid)
			.filter(|&pid| pid != 0)
			.unwrap_or(PMT_PID)
	}

	/// Build the PAT/PMT once every track's PID and codec is known.
	fn build_psi(&mut self) -> anyhow::Result<()> {
		for track in self.program.tracks.values_mut() {
			track.listed = true;
		}
		let pcr_pid = self.pick_clock()?;
		let pmt = self.program_map(pcr_pid)?;

		// Preserve the source's program identity so the rebuilt PAT stays consistent with the
		// carried SI; synthesize a minimal identity otherwise.
		let pmt_pid = self.pmt_pid();
		let transport_stream_id = self
			.program
			.program
			.as_ref()
			.map(|s| s.transport_stream_id)
			.unwrap_or(1);

		self.program.schedule.set_tables(pmt_pid);
		self.program.schedule.set_clock(pcr_pid);
		// A reader of the first tables has no program clock yet. Tables that take over from
		// an earlier program's flag a break in the clock it already reads instead.
		if !self.program.pcr_discontinuity {
			self.program.schedule.open();
		}
		for track in self.program.tracks.values() {
			self.program.liveness.register(track.pid);
		}
		self.program.psi = Some(Psi {
			transport_stream_id,
			pmt,
			pmt_pid,
			led: None,
			stated: None,
			laid: None,
			sending: VecDeque::new(),
			out: None,
		});
		self.program.psi_due = true;
		Ok(())
	}

	/// Whether the catalog carries a NIT on its PID, which the PAT then points a receiver at.
	fn carries_nit(&self) -> bool {
		self.program.si.contains_key(&(NIT_PID, NIT_ACTUAL))
	}

	/// The PID to carry the clock: the first video track's, else the first audio track's, else
	/// one used by nothing else, since sparse data cannot carry the clock's cadence.
	fn pick_clock(&self) -> anyhow::Result<u16> {
		let mut tracks: Vec<&Track> = self.program.tracks.values().filter(|t| t.listed).collect();
		tracks.sort_by_key(|t| t.pid);
		tracks
			.iter()
			.find(|t| matches!(t.kind, Kind::Video(_)))
			.or_else(|| {
				tracks.iter().find(|t| {
					matches!(
						t.kind,
						Kind::Aac { .. } | Kind::Opus { .. } | Kind::Mp2 { .. } | Kind::Ac3 | Kind::Eac3
					)
				})
			})
			.map(|t| t.pid)
			.or_else(|| {
				(FIRST_ES_PID..0x1fff).find(|pid| {
					*pid != self.pmt_pid()
						&& self.program.tracks.values().all(|track| track.pid != *pid)
						&& self.program.si.keys().all(|(si, _)| si != pid)
				})
			})
			.context("TS export has no unused PID for the PCR")
	}

	/// The PMT listing every listed track in PID order, with the clock on `pcr_pid`.
	fn program_map(&self, pcr_pid: u16) -> anyhow::Result<psi::Pmt> {
		let mut tracks: Vec<&Track> = self.program.tracks.values().filter(|t| t.listed).collect();
		tracks.sort_by_key(|t| t.pid);

		let streams = tracks
			.iter()
			.map(|t| {
				// Prefer the descriptors captured verbatim on import.
				let descriptors = match t.descriptors.is_empty() {
					true => t.kind.descriptors(),
					false => t.descriptors.clone(),
				};
				Ok(super::psi::Es {
					stream_type: t.kind.stream_type(),
					pid: Pid::new(t.pid)?,
					descriptors,
				})
			})
			.collect::<anyhow::Result<Vec<_>>>()?;

		// Re-emit the captured program-level descriptors. With none (a non-TS source),
		// derive the SCTE-35 'CUEI' registration when a 0x86 verbatim stream is present.
		let program_info = if !self.program.program_descriptors.is_empty() {
			self.program.program_descriptors.clone()
		} else if tracks.iter().any(|t| {
			// Only derive CUEI for section-framed 0x86 (SCTE-35); a whole-PES 0x86
			// (e.g. DTS audio) must not advertise SCTE-35 section signaling.
			matches!(&t.kind, Kind::Verbatim { stream_type: 0x86 })
		}) {
			vec![registration(b"CUEI")]
		} else {
			Vec::new()
		};

		// A revision stays on the program the PAT went out naming.
		let program_number = match &self.program.psi {
			Some(psi) => psi.pmt.program_number,
			None => self
				.program
				.program
				.as_ref()
				.map(|s| s.program_number)
				.filter(|&id| id != 0)
				.unwrap_or(1),
		};
		Ok(super::psi::Pmt {
			program_number,
			pcr_pid: Some(Pid::new(pcr_pid)?),
			program_info,
			streams,
		})
	}

	/// Rebuild the PMT from the listed tracks, leading the next frame muxed when it changed. The
	/// clock stays on its PID unless the stream carrying it retired.
	fn revise(&mut self) -> anyhow::Result<()> {
		let Some(psi) = self.program.psi.as_ref() else {
			return Ok(());
		};
		let current = psi.pcr_pid().context("PMT without a clock")?;
		let kept = psi.own_clock() == Some(current)
			|| self
				.program
				.tracks
				.values()
				.any(|track| track.listed && track.pid == current);
		let pcr_pid = match kept {
			true => current,
			false => self.pick_clock()?,
		};
		let pmt = self.program_map(pcr_pid)?;
		let psi = self.program.psi.as_mut().expect("checked above");
		if pmt != psi.pmt {
			psi.pmt = pmt;
			self.program.revised = true;
			self.restate()?;
		}
		Ok(())
	}

	/// List `name` in the PMT ahead of its first frame: `false` while it waits for its PID,
	/// which a stream the catalog dropped still holds. That stream is no longer read, so only
	/// what it already holds drains ahead of this one ([`Self::settle`]).
	fn list(&mut self, name: &str) -> anyhow::Result<bool> {
		let track = self.program.tracks.get(name).context("missing track")?;
		if track.listed {
			return Ok(true);
		}
		if !track.parked.is_empty() {
			return Ok(false);
		}
		let pid = track.pid;
		if let Some((holder, track)) = self
			.program
			.tracks
			.iter_mut()
			.find(|(other, track)| track.listed && track.pid == pid && other.as_str() != name)
		{
			if !track.finished {
				track.release(holder, &mut self.jitter, true)?;
				track.finished = true;
			}
			// Replaced, so a return of its name no longer reads it on.
			track.withdrawn = false;
			// A same-instance return that put another track on its PID replaced it.
			self.program.stale.remove(holder.as_str());
			return Ok(false);
		}
		self.program.tracks.get_mut(name).expect("checked above").listed = true;
		self.program.liveness.register(pid);
		self.revise()?;
		Ok(true)
	}

	/// Retire each track the catalog dropped once nothing of it is left to go out, and list
	/// the tracks that waited for its PID ([`Self::list`]).
	fn settle(&mut self) -> anyhow::Result<()> {
		if self.program.psi.is_none() {
			return Ok(());
		}
		let gone: Vec<String> =
			self.program
				.tracks
				.iter()
				.filter(|(name, track)| {
					// A track the same instance's return has yet to resubscribe stays, silent on its PID.
					!track.cataloged
						&& !self.program.stale.contains(name.as_str())
						&& track.finished && track.pack.is_none()
						&& track.parked.is_empty()
						&& !self.jitter.holds(&track.key)
						&& self.held.as_ref().is_none_or(|held| held.track != track.key)
						&& !(track.listed && self.program.schedule.holds(track.pid))
				})
				.map(|(name, _)| name.clone())
				.collect();
		let mut changed = false;
		for name in gone {
			let track = self.program.tracks.remove(&name).expect("collected above");
			self.jitter.forget(&TrackKey::new(self.program.id, &name));
			self.program.stale.remove(&name);
			if track.listed {
				self.program.liveness.retire(track.pid);
				self.program.schedule.forget(track.pid);
				changed = true;
			}
		}
		// A freed PID goes to the track that has waited for it longest; the rest wait their turn.
		let mut waiting: Vec<(Timestamp, &String, u16)> = self
			.program
			.tracks
			.iter()
			.filter_map(|(name, track)| Some((track.parked.front()?.key, name, track.pid)))
			.collect();
		waiting.sort();
		let mut claimed: Vec<u16> = self
			.program
			.tracks
			.values()
			.filter(|t| t.listed)
			.map(|t| t.pid)
			.collect();
		let mut free = Vec::new();
		for (_, name, pid) in waiting {
			if !claimed.contains(&pid) {
				claimed.push(pid);
				free.push(name.clone());
			}
		}
		for name in &free {
			let track = self.program.tracks.get_mut(name).expect("collected above");
			track.listed = true;
			self.program.liveness.register(track.pid);
			changed = true;
		}
		if changed {
			self.revise()?;
		}
		for name in free {
			let parked = std::mem::take(&mut self.program.tracks.get_mut(&name).expect("collected above").parked);
			for queued in parked {
				self.mux(&name, queued)?;
			}
		}
		Ok(())
	}

	/// Hand the program tables to the schedule ahead of the next unit, if they are due.
	fn lead_tables(&mut self) -> anyhow::Result<()> {
		if !std::mem::take(&mut self.program.psi_due) {
			return Ok(());
		}
		self.restate()?;
		let psi = self.program.psi.as_mut().context("PSI not built")?;
		let added: Vec<u16> = psi
			.pmt
			.streams
			.iter()
			.filter(|es| psi.led.as_ref().is_none_or(|pmt| !pmt.streams.contains(es)))
			.map(|es| es.pid.as_u16())
			.collect();
		psi.led = Some(psi.pmt.clone());
		self.program.schedule.lead(super::schedule::Tables {
			rate: self.program.mux_rate,
			added,
		});
		Ok(())
	}

	/// Set the program tables as they stand on the schedule, which every copy of them still to
	/// begin lays. A NIT acquired or lost after the tables went out revises the PAT's network
	/// entry, at the next version beside the PMT. The first copy after a switch to another
	/// instance flags the break on each table PID that owes one ([`Self::breaks`]).
	fn restate(&mut self) -> anyhow::Result<()> {
		let network = self.carries_nit();
		let psi = self.program.psi.as_ref().context("PSI not built")?;
		let tables = psi.current(network, self.program.version)?;
		let pmt_pid = psi.pmt_pid;
		let clock = psi.pcr_pid().context("PMT without a clock")?;
		let section = super::psi::encode_pmt(&tables.pmt, tables.version)?;
		let breaks: Vec<u16> = [Pid::PAT, pmt_pid]
			.into_iter()
			.filter(|&pid| self.owes_break(pid))
			.collect();
		let encode = |flag: &dyn Fn(u16) -> bool| -> anyhow::Result<Vec<u8>> {
			let mut packets = Vec::new();
			section_packets(&mut packets, Pid::PAT, &tables.pat, flag(Pid::PAT))?;
			section_packets(&mut packets, pmt_pid, &section, flag(pmt_pid))?;
			Ok(packets)
		};
		let packets = encode(&|_| false)?;
		let flagged = match breaks.is_empty() {
			true => None,
			false => Some(encode(&|pid| breaks.contains(&pid))?),
		};
		self.program.schedule.set_psi(super::schedule::Psi {
			packets,
			flagged,
			clock,
		});
		self.program.psi.as_mut().expect("checked above").stated = Some((tables, breaks));
		Ok(())
	}

	/// Packetize one media frame and queue it on the [`Schedule`], re-emitting PAT/PMT
	/// before video keyframes (and periodically) so receivers can tune in mid-stream.
	fn mux(&mut self, name: &str, queued: Queued) -> anyhow::Result<()> {
		let Queued {
			frame,
			decode,
			dts,
			description,
			pes,
			..
		} = queued;
		let is_video = matches!(
			self.program.tracks.get(name).context("missing track")?.kind,
			Kind::Video(_)
		);
		// Refresh PSI at keyframes or after the interval lapses, and lead with a revision.
		let cadence = (is_video && frame.keyframe) || due(frame.timestamp, self.program.last_psi, PSI_INTERVAL);
		let psi = std::mem::take(&mut self.program.revised) || cadence;
		if psi {
			// A receiver tunes in at the tables, so each AAC PID's next frame repeats its program
			// config element. Its older frames go on ahead of the tables, so that frame starts a
			// PES rather than following raw blocks that lack the element.
			self.flush_packs(
				|track, _| matches!(&track.kind, Kind::Aac { config, .. } if config.program_config.is_some()),
			)?;
			for track in self.program.tracks.values_mut() {
				if let Kind::Aac { repeat, .. } = &mut track.kind {
					*repeat = true;
				}
			}
		}
		let track = self.program.tracks.get_mut(name).context("missing track")?;
		let pid = track.pid;
		let kind = track.kind.clone();
		if let Kind::Aac { repeat, .. } = &mut track.kind {
			*repeat = false;
		}
		let keyframe = frame.keyframe;
		let pce = matches!(kind, Kind::Aac { repeat: true, .. });
		let es_payload = es_payload(&kind, description.as_ref(), &frame, pce)?;

		let mut out = Vec::with_capacity(TsPacket::SIZE);

		if cadence {
			self.program.last_psi = Some(frame.timestamp);
		}

		// Emit each SI entry's sections verbatim. A changed snapshot (`dirty`) goes
		// out once the revision floor has elapsed since the entry last hit the wire,
		// rather than waiting for a slot: a TDT/TOT revision held to a 30s grid would
		// deliver the clock up to a whole slot late (#2934). Unchanged repeats ride
		// the absolute media-time grid [`due`] gives the PSI, so two exporters of
		// one broadcast repeat them at the same instants whatever their start
		// (#3948): an SDT every 2s where the PSI wants 500ms. Clock tables never
		// repeat unchanged, since a repeat re-asserts an already-sent time and steps
		// a receiver backwards; every value they carry is a revision. Unknown tables
		// have no declared interval and fall back to the PSI cadence. `Bytes` clones
		// are refcount bumps, and only a due entry is collected at all.
		let pending: Vec<(u16, Vec<Bytes>)> = self
			.program
			.si
			.iter_mut()
			.filter(|(_, si)| !si.active.is_empty())
			.filter(|((_, table_id), si)| {
				let interval = si.interval.unwrap_or(PSI_INTERVAL);
				// A deferred revision stays dirty and carries whatever `active`
				// holds when it finally rides.
				let revision = si.dirty && si_due(frame.timestamp, si.last_emit, SI_REVISION_INTERVAL.min(interval));
				let repeat = !is_clock_table(*table_id) && due(frame.timestamp, si.last_emit, interval);
				revision || repeat
			})
			.map(|((pid, _), si)| {
				si.dirty = false;
				// The anchor never moves backwards. A non-zero interval cannot regress
				// it on its own (`si_due` and `due` admit only timestamps past it), but
				// a zero-interval entry emits on every frame including reordered
				// (B-frame) timestamps below the anchor, and a catalog update can raise
				// the interval later; a regressed anchor would then fall in an earlier
				// slot or credit the reorder span against the floor, emitting early.
				if si.last_emit.is_none_or(|last| frame.timestamp > last) {
					si.last_emit = Some(frame.timestamp);
				}
				(*pid, si.active.sections().cloned().collect())
			})
			.collect();
		for (pid, sections) in pending {
			self.program.schedule.set_buffer(pid, Buffer::SYSTEM);
			for section in &sections {
				self.write_section(&mut out, pid, section)?;
			}
		}
		// A reader drops SI it cannot place on the program clock, so while the opening's PCR
		// interval may still be ahead of the unit this frame goes in, the schedule holds the SI
		// for it rather than the media it would lead.
		if self.program.schedule.clocking() {
			self.program.schedule.push_clocked(std::mem::take(&mut out));
		}

		// The tables lead the unit that carries this frame, behind whatever went before it.
		if kind.placed() {
			self.program.psi_due |= psi;
			return self.place(name, &frame, out);
		}
		match es_payload {
			// Section-framed verbatim (SCTE-35, ...) rides in private sections, not PES;
			// carry the bytes verbatim.
			None => {
				self.program.psi_due |= psi;
				self.write_section(&mut out, pid, &frame.payload)?;
			}
			Some(es_payload) => {
				let unit = PesUnit {
					pid,
					is_video,
					keyframe,
					timestamp: frame.timestamp,
					dts,
					stream_id: stream_id(&kind),
				};
				// Only a delay leaves the frames time to wait for the ones after them.
				let track = self.program.tracks.get(name).context("missing track")?;
				if let Some((samples, rate)) = track.samples(&es_payload).filter(|_| !self.program.delay.is_zero()) {
					return self.pack(name, unit, decode, (psi, out), es_payload, (samples, rate, pes));
				}
				self.flush(name)?;
				self.program.psi_due |= psi;
				self.write_pes(&mut out, &unit, &es_payload)?;
			}
		}
		self.push_unit(name, pid, decode, out, keyframe, Vec::new())
	}

	/// Queue a whole PES or section unchanged on `name`'s PID behind `tables`, its
	/// first byte going out at the frame's timestamp, the source's placement, and no earlier.
	///
	/// A PID the T-STD models holds the PES's bytes in its decoder buffer until they decode,
	/// timed by the PTS in its header. For DVB Dolby audio that times only the first access
	/// unit that begins in it, so each one after counts on from there and leaves at its own time.
	/// The bytes ahead of the first end a frame an earlier PES began; they leave with it,
	/// which only overstates what the buffer holds. A PES without a PTS holds nothing there.
	fn place(&mut self, name: &str, frame: &Frame, tables: Vec<u8>) -> anyhow::Result<()> {
		let track = self.program.tracks.get(name).context("missing track")?;
		let pes = match track.kind {
			Kind::PesPacket { stream_type } => Some(
				PesPacket::parse(&frame.payload, stream_type)
					.with_context(|| format!("MPEG-TS track '{name}' carries a malformed PES packet"))?,
			),
			Kind::Verbatim { .. } => None,
			_ => unreachable!("a placed track"),
		};
		let pid = track.pid;
		let place = frame.timestamp.as_nanos();
		// Its first access unit decodes at the DTS, which a reordered picture carries ahead of its PTS.
		let decode = pes
			.as_ref()
			.and_then(|pes| pes.dts.or(pes.pts))
			.map(|ticks| unwrap_pts(ticks, frame.timestamp));
		let dolby = Dolby::described(&track.descriptors)
			.filter(|_| matches!(track.kind, Kind::PesPacket { stream_type: 0x06 }));
		let frames = match (&pes, decode, dolby) {
			(Some(pes), Some(decode), Some(codec)) => dolby_removals(codec, &frame.payload[pes.payload..], decode),
			_ => Vec::new(),
		};
		let buffer = track.buffer();
		let mut out = std::mem::take(&mut self.program.tables);
		out.extend_from_slice(&tables);
		let prefix = match pes {
			Some(_) => {
				self.write_pes_packet(&mut out, pid, &frame.payload)?;
				0
			}
			None => {
				self.write_section(&mut out, pid, &frame.payload)?;
				1
			}
		};
		if let Some(buffer) = buffer {
			self.program.schedule.set_buffer(pid, buffer);
		}
		self.lead_tables()?;
		self.program.schedule.push_placed(
			pid,
			(place, decode.unwrap_or(place)),
			prefix,
			out,
			frame.keyframe,
			frames,
		)
	}

	/// Mux an audio frame into `name`'s PES, sending the PES on first if the frame cannot join
	/// it, and leave the program tables due (`psi`) and the SI muxed ahead of it to ride the
	/// next unit queued. A PES carries
	/// frames while they run on from its PTS without a gap and fit the PID's decoder buffer
	/// whole with its header, so it arrives by its first frame's decode time as one frame
	/// would, and they span at most half the delay: its frames leave the jitter buffer with
	/// its first ([`Track::release_key`]), so each keeps at least that much of the delay to
	/// arrive in.
	///
	/// A PES carries only frames that left the jitter buffer together ([`Track::pack_start`]),
	/// so it is whole however often the exporter is polled, and exporters of the broadcast that
	/// joined at different frames still cut the same PESs from the first that starts one, like
	/// [`due`].
	fn pack(
		&mut self,
		name: &str,
		unit: PesUnit,
		decode: Timestamp,
		(psi, tables): (bool, Vec<u8>),
		payload: Vec<u8>,
		(samples, rate, start): (u64, u32, Option<PesStart>),
	) -> anyhow::Result<()> {
		let budget = self.budget().as_nanos();
		let track = self.program.tracks.get(name).context("missing track")?;
		let (size, fit) = track.fit(payload.len());
		let place = place(unit.timestamp, samples, rate);
		// A PTS is a whole 90 kHz tick, so the frames run on within one of where they end.
		let joins = start.is_some()
			&& track.pack.as_ref().is_some_and(|pack| {
				pack.start == start
					&& pack.rate == rate
					&& pack.payload.len() + payload.len() <= size
					&& decode.as_nanos().saturating_sub(pack.decode.as_nanos()) <= budget
					&& pack.end().abs_diff(unit.timestamp.as_micros()) <= 12
			});
		if !joins {
			self.flush(name)?;
		}
		self.program.psi_due |= psi;
		self.program.tables.extend_from_slice(&tables);
		let track = self.program.tracks.get_mut(name).context("missing track")?;
		let pack = track.pack.get_or_insert(Pack {
			unit,
			decode,
			start,
			payload: Vec::new(),
			frames: Vec::new(),
			samples: 0,
			rate,
		});
		pack.frames.push((decode.as_nanos(), payload.len()));
		pack.payload.extend_from_slice(&payload);
		pack.samples += samples;
		// The next frame starts a PES of its own anyway.
		if (place + 1).is_multiple_of(fit) {
			self.flush(name)?;
		}
		Ok(())
	}

	/// Send `name`'s packed frames on as one PES.
	fn flush(&mut self, name: &str) -> anyhow::Result<()> {
		let Some(pack) = self.program.tracks.get_mut(name).and_then(|track| track.pack.take()) else {
			return Ok(());
		};
		let Pack {
			unit,
			decode,
			payload,
			frames,
			..
		} = pack;
		let mut out = Vec::new();
		self.write_pes(&mut out, &unit, &payload)?;
		self.push_unit(name, unit.pid, decode, out, unit.keyframe, frames)
	}

	/// Send on, in decode order, the packed frames of each track for which `done` holds, given
	/// the track and the slot its first frame is due in.
	fn flush_packs(&mut self, done: impl Fn(&Track, u128) -> bool) -> anyhow::Result<()> {
		let mut due: Vec<(Timestamp, String)> = self
			.program
			.tracks
			.iter()
			.filter_map(|(name, track)| {
				let pack = track.pack.as_ref()?;
				let by = pack.decode.as_nanos().saturating_sub(track.drain().as_nanos());
				done(track, schedule::slot(by)).then(|| (pack.decode, name.clone()))
			})
			.collect();
		due.sort();
		for (_, name) in due {
			self.flush(&name)?;
		}
		Ok(())
	}

	/// Queue one unit of `name`'s packets on `pid`, decoding at `decode`, on the [`Schedule`],
	/// behind the tables still waiting: with `frames`, each packed frame's decode nanoseconds
	/// and bytes.
	fn push_unit(
		&mut self,
		name: &str,
		pid: u16,
		decode: Timestamp,
		out: Vec<u8>,
		keyframe: bool,
		frames: Vec<(u128, usize)>,
	) -> anyhow::Result<()> {
		let mut tables = std::mem::take(&mut self.program.tables);
		let out = match tables.is_empty() {
			true => out,
			false => {
				tables.extend_from_slice(&out);
				tables
			}
		};
		let track = self.program.tracks.get(name).context("missing track")?;
		if let Some(buffer) = track.buffer() {
			self.program.schedule.set_buffer(pid, buffer);
		}
		// A padded output cannot burst a picture late, so a CPB that the source may fill to
		// its declared buffer delay needs that much send-ahead, all of it out of the delay.
		if self.program.mux_rate.is_some()
			&& let Some(needs) = track.timing.buffer_delay()
		{
			let slots = schedule::slot(needs.as_nanos());
			anyhow::ensure!(
				slots <= schedule::slot(self.program.delay.as_nanos()),
				"MPEG-TS video on PID {pid} declares a {needs:.3?} decoder buffer delay (its HRD's CPB over its bit rate), past what a {:?} delay sends it ahead; raise the delay to {:?}",
				self.program.delay,
				PCR_INTERVAL * u32::try_from(slots).unwrap_or(u32::MAX),
			);
		}
		let by = decode.as_nanos().saturating_sub(track.drain().as_nanos());
		self.lead_tables()?;
		self.program.schedule.push_frames(
			pid,
			super::schedule::Timing {
				by,
				decode: decode.as_nanos(),
			},
			out,
			keyframe,
			frames,
		)?;
		Ok(())
	}

	/// The longest any track's receiver takes to pass one of its packets on.
	fn max_drain(&self) -> Duration {
		self.program.tracks.values().map(Track::drain).max().unwrap_or_default()
	}

	/// When the next grid slot is due on the jitter buffer's clock: once every frame that could
	/// ride it has gone out of the jitter buffer, so its contents depend on the frames alone,
	/// never on when they arrived, and the output keeps the clock's pace while media is queued
	/// (in the schedule or still in the jitter buffer) or still to decode. `None` without a
	/// delay, where frames settle the slots instead.
	fn slot_due(&self) -> Option<(u128, moq_clock::Instant)> {
		let (known, settled) = self.slot_settled()?;
		let settled = Timestamp::from_micros(u64::try_from(settled / 1_000).ok()?).ok()?;
		// The tail of the generation still going out keeps its own clock after the next is acquired.
		Some((known, self.jitter.at(self.program.generation, settled)?))
	}

	/// The `known` that settles the next grid slot, and the jitter buffer key, in nanoseconds,
	/// by which every frame that could ride it has gone out of the jitter buffer. Keys run a
	/// delay past release times ([`Track::release_key`]).
	fn slot_settled(&self) -> Option<(u128, u128)> {
		if self.program.delay.is_zero()
			|| self.program.psi.is_none()
			|| (self.program.schedule.is_empty()
				&& !self.packing()
				&& (self.jitter.is_empty()
					|| (self.pending.is_some()
						&& !self.program.tracks.values().any(|track| self.jitter.holds(&track.key)))))
		{
			return None;
		}
		let (_, known) = self.program.schedule.upcoming()?;
		Some((
			known,
			known * PCR_INTERVAL.as_nanos() + self.max_drain().as_nanos() + self.program.delay.as_nanos(),
		))
	}

	/// Whether a track holds frames packed for a PES still to go out.
	fn packing(&self) -> bool {
		self.program.tracks.values().any(|track| track.pack.is_some())
	}

	/// Lay out every grid slot settled before a frame keyed `key` in the jitter buffer goes
	/// out, so frames and slots follow each other in the same order however often the
	/// exporter is polled.
	fn lay_before(&mut self, key: u128) -> anyhow::Result<()> {
		while let Some((known, _)) = self.slot_settled().filter(|&(_, settled)| settled < key) {
			self.lay(Some(known))?;
		}
		Ok(())
	}

	/// How far a stalled group may fall behind the newest content before each source skips it:
	/// half the delay. A skip comes once the next group's start is that far behind, and its
	/// frames are only read then, so with the whole delay they would arrive at their deadline.
	fn budget(&self) -> Duration {
		self.program.delay / 2
	}

	/// Lay out every grid slot whose time has come, and wake when the next one is due.
	fn lay_due(&mut self, waiter: &kio::Waiter) -> anyhow::Result<()> {
		while let Some((known, at)) = self.slot_due() {
			if moq_clock::now() < at {
				let timer = self
					.program
					.slot_timer
					.get_or_insert_with(|| Box::pin(moq_clock::sleep_until(at)));
				if timer.deadline() != at {
					timer.as_mut().reset(at);
				}
				if waiter.poll_future(timer.as_mut()).is_pending() {
					return Ok(());
				}
			}
			self.lay(Some(known))?;
		}
		self.program.slot_timer = None;
		Ok(())
	}

	/// Lay out every grid slot the schedule has settled, one [`Frame`] each: `known` is
	/// the first slot a frame still to be muxed could be due in, or `None` when nothing
	/// more is coming.
	///
	/// Each frame opens with its slot's PCR: at a multiplex rate, in the adaptation field of
	/// the clock PID's packet when one can go out there, else in a clock packet of its own.
	/// At a multiplex rate its value is the time of its own byte position at the rate, so a
	/// consumer holding only the byte stream (which is every MPEG-TS tool) recovers the same
	/// clock the values assert. Each frame is stamped at its slot boundary, so a pacing
	/// caller releases the clock at the instant it asserts.
	///
	/// The PCR's value never comes from the PES units: a PCR sampled from the DTS would
	/// step with the frame cadence, and no downstream CBR stage can repair that, because a
	/// groomer can only place the clock samples it receives. So the PCR follows its own
	/// grid instead: slots on the media timeline, shared by every exporter of the
	/// broadcast, like [`due`].
	fn lay(&mut self, known: Option<u128>) -> anyhow::Result<()> {
		let Some(pmt_pid) = self.program.psi.as_ref().map(|psi| psi.pmt_pid) else {
			return Ok(());
		};
		if known.is_none() {
			self.flush_packs(|_, _| true)?;
		}
		let lead = schedule::slot(self.budget().as_nanos());
		loop {
			// A PES of packed frames goes out half the delay before its first is due, so it
			// has that long to reach the decoder.
			if known.is_some()
				&& let Some((index, _)) = self.program.schedule.upcoming()
			{
				self.flush_packs(|_, due| due <= index + lead)?;
			}
			let Some(slot) = self.program.schedule.next(known)? else {
				break;
			};
			let clock = pcr_packet(slot.clock.context("a slot without a clock PID")?, slot.pcr)?;
			let mut payload = slot.layout(&clock, pmt_pid, &NULL_PACKET);
			// The slot opens with its PCR, in the clock packet or the clock PID's packet that
			// carries it, whose adaptation field flags the break.
			let discontinuity = std::mem::take(&mut self.program.pcr_discontinuity);
			if discontinuity {
				payload[5] |= 0x80;
			}
			self.number(&mut payload);
			let frame = Frame {
				timestamp: slot_stamp(slot.index)?,
				duration: None,
				payload: Bytes::from(payload),
				keyframe: slot.keyframe,
			};
			let psi = self.program.psi.as_mut().expect("checked above");
			// Each copy begun laid the tables as they stood, and the first spent the breaks they flag.
			let mut spent = Vec::new();
			if slot.begun > 0 {
				let (tables, breaks) = psi.stated.clone().context("program tables laid but never set")?;
				psi.sending.extend(std::iter::repeat_n(tables.clone(), slot.begun));
				psi.laid = Some(tables);
				spent = breaks;
			}
			let tables = (0..slot.tables).filter_map(|_| psi.sending.pop_front()).last();
			let tally = Tally {
				pcr: (slot.pcr, discontinuity),
				units: slot.units,
				tables,
			};
			self.program.queue.push_back((frame, tally));
			if !spent.is_empty() {
				if let Some(flagged) = &mut self.program.flagged {
					flagged.extend(spent);
				}
				self.restate()?;
			}
		}
		Ok(())
	}

	/// Number the continuity counters of packets about to go out. A packet without a
	/// payload (a clock packet) repeats the counter before it (ISO 13818-1 2.4.3.3); before
	/// anything has gone out on its PID, any value starts a valid run.
	fn number(&mut self, packets: &mut [u8]) {
		for packet in packets.as_chunks_mut::<{ TsPacket::SIZE }>().0 {
			let pid = u16::from(packet[1] & 0x1f) << 8 | u16::from(packet[2]);
			if pid == 0x1FFF {
				continue;
			}
			let next = self.program.counters.entry(pid).or_default();
			let cc = match packet[3] & 0x10 != 0 {
				true => std::mem::replace(next, (*next + 1) & ContinuityCounter::MAX),
				false => next.wrapping_sub(1) & ContinuityCounter::MAX,
			};
			packet[3] = (packet[3] & 0xf0) | cc;
		}
	}

	/// Packetize a PES payload into 188-byte TS packets.
	fn write_pes(&mut self, out: &mut Vec<u8>, unit: &PesUnit, payload: &[u8]) -> anyhow::Result<()> {
		let flag = self.breaks(unit.pid);
		pes_packets(out, unit, payload, flag)
	}

	/// Packetize `pes`, a whole PES packet, on `pid` unchanged ([`whole_pes_packets`]).
	fn write_pes_packet(&mut self, out: &mut Vec<u8>, pid: u16, pes: &[u8]) -> anyhow::Result<()> {
		let flag = self.breaks(pid);
		whole_pes_packets(out, pid, pes, flag)
	}

	/// Packetize a complete PSI or private section verbatim ([`section_packets`]).
	fn write_section(&mut self, out: &mut Vec<u8>, pid: u16, section: &[u8]) -> anyhow::Result<()> {
		// The verbatim track is public; a non-importer producer could publish a frame
		// that isn't a complete section. Drop it (with a warning) rather than emit a
		// malformed section a downstream demuxer would choke on. One bad section must
		// not abort a live export, so this skips instead of erroring.
		if !is_complete_section(section) {
			tracing::warn!(pid, len = section.len(), "dropping malformed private section on export");
			return Ok(());
		}
		let flag = self.breaks(pid);
		section_packets(out, pid, section, flag)
	}
}

/// Packetize a PES payload into 188-byte TS packets, flagging a break in the first when `flag`.
fn pes_packets(out: &mut Vec<u8>, unit: &PesUnit, payload: &[u8], flag: bool) -> anyhow::Result<()> {
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
		let flag = first && flag;
		let adaptation = ((first && unit.keyframe) || flag).then(|| AdaptationField {
			random_access_indicator: first && unit.keyframe,
			..flags(flag)
		});

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

		write_packet(out, unit.pid, adaptation, ts_payload)?;

		offset += take;
		first = false;
		if offset >= payload.len() {
			break;
		}
	}
	Ok(())
}

/// Packetize `pes`, a whole PES packet, into TS packets on `pid` without touching a byte of
/// it, flagging a break in the first packet when `flag` and stuffing the last packet's
/// adaptation field. The `mpeg2ts` writer would rebuild the PES header instead, so the packets
/// are laid out by hand.
fn whole_pes_packets(out: &mut Vec<u8>, pid: u16, pes: &[u8], flag: bool) -> anyhow::Result<()> {
	anyhow::ensure!(pid < 0x1FFF, "PID out of range: {pid}");
	let mut offset = 0;
	while offset < pes.len() {
		let start = offset == 0;
		let flag = start && flag;
		// The adaptation field: its length byte, then a flags byte for the break.
		let field = if flag { 2 } else { 0 };
		let take = (TsBytes::MAX_SIZE - field).min(pes.len() - offset);
		let field = TsBytes::MAX_SIZE - take;
		out.extend_from_slice(&[0x47, u8::from(start) << 6 | (pid >> 8) as u8, pid as u8]);
		match field {
			0 => out.push(0x10),
			_ => {
				out.push(0x30);
				out.push((field - 1) as u8);
				if field > 1 {
					out.push(if flag { 0x80 } else { 0 });
					out.resize(out.len() + field - 2, 0xff);
				}
			}
		}
		out.extend_from_slice(&pes[offset..offset + take]);
		offset += take;
	}
	Ok(())
}

/// Packetize a complete PSI or private section verbatim, flagging a break in the first packet
/// when `flag`. The first packet carries the pointer_field plus the section start as a
/// `Section` payload (sets the unit-start bit so the receiver finds the pointer_field);
/// continuations are `Raw`. The section bytes are opaque, so this round-trips byte-for-byte.
fn section_packets(out: &mut Vec<u8>, pid: u16, section: &[u8], flag: bool) -> anyhow::Result<()> {
	let mut offset = 0;
	let mut first = true;
	loop {
		let flag = (first && flag).then(|| flags(true));
		let payload = if first {
			// pointer_field (1 byte, written by `Section`) eats one payload byte, and a flag
			// for the break its adaptation field.
			let room = TsBytes::MAX_SIZE - 1 - flag.as_ref().map_or(0, adaptation_size);
			let take = room.min(section.len());
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

		write_packet(out, pid, flag, payload)?;
		first = false;
		if offset >= section.len() {
			break;
		}
	}
	Ok(())
}

/// Serialize one TS packet into `out`. Its continuity counter is numbered when it goes
/// out ([`Export::number`]), since the schedule decides that order.
fn write_packet(
	out: &mut Vec<u8>,
	pid: u16,
	adaptation_field: Option<AdaptationField>,
	payload: TsPayload,
) -> anyhow::Result<()> {
	let continuity_counter = ContinuityCounter::default();

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

impl<E: catalog::Catalog> Export<crate::catalog::Consumer<E>> {
	pub(super) fn catalog_format(&self) -> crate::catalog::CatalogFormat {
		self.selected().catalog.as_ref().expect("selected catalog").format()
	}
}

/// One adaptation-field-only TS packet carrying `ticks` (continuous 90 kHz) as its
/// PCR, laid out by hand: the `mpeg2ts` serializer writes the six reserved bits
/// between PCR base and extension as zeros where ISO 13818-1 requires ones, and
/// strict analyzers flag that.
///
/// There is no payload, so the field's stuffing fills the packet; its continuity counter
/// is numbered when it goes out ([`Export::number`]).
fn pcr_packet(pid: u16, pcr: u64) -> anyhow::Result<Vec<u8>> {
	anyhow::ensure!(pid <= 0x1FFF, "PID out of range: {pid}");
	let mut p = Vec::with_capacity(TsPacket::SIZE);
	p.push(0x47);
	p.push((pid >> 8) as u8);
	p.push(pid as u8);
	// adaptation_field_control = adaptation field only, no scrambling.
	p.push(0x20);
	// adaptation_field_length covers the rest of the packet.
	p.push(183);
	// PCR_flag alone.
	p.push(0x10);
	p.extend_from_slice(&super::schedule::pcr_field(pcr));
	p.resize(TsPacket::SIZE, 0xff);
	Ok(p)
}

/// PES header bytes ahead of the optional region: start code, stream id and packet length.
const PES_PREFIX_LEN: usize = 6;
/// Optional PES header region carrying PTS only: 2 flag bytes + 1 length byte + 5 PTS bytes.
const PES_OPTIONAL_LEN: usize = 3 + 5;
/// Extra bytes when the optional region also carries a DTS (5 DTS bytes).
const PES_DTS_LEN: usize = 5;

/// What the mux reads of a frame holding one whole PES packet ([`Kind::PesPacket`]).
struct PesPacket {
	/// Its PTS in 90 kHz ticks, as the 33-bit field carries it.
	pts: Option<u64>,
	/// Its DTS, likewise, where it differs from the PTS.
	dts: Option<u64>,
	/// Where its payload begins, past the header.
	payload: usize,
}

impl PesPacket {
	/// Read `bytes` as exactly one well-formed PES packet of a stream of `stream_type`:
	/// the start code, a `PES_packet_length` that matches, and a complete optional header
	/// where the `stream_id` has one (ISO 13818-1 2.4.3.6). Only video may leave the
	/// length zero, unbounded, as 2.4.3.7 permits.
	fn parse(bytes: &[u8], stream_type: u8) -> anyhow::Result<Self> {
		// The importer refuses with this same check, so a PES it published never fails here.
		let pes = psi::PesStart::parse(bytes)?;
		// The clock fields it refuses ride through unchanged.
		pes.whole(stream_type)?;
		if let Some(len) = pes.data_len {
			anyhow::ensure!(
				len == pes.data.len(),
				"PES_packet_length declares {len} payload bytes where the frame holds {}",
				pes.data.len()
			);
		}
		Ok(Self {
			pts: pes.pts,
			dts: pes.dts,
			payload: pes.header.len(),
		})
	}
}

/// A 33-bit PTS read off a placed PES, on the media timeline in nanoseconds: the value
/// nearest its placement `at` that the wrapped field allows, since a PES decodes close to
/// where it is placed.
fn unwrap_pts(pts: u64, at: Timestamp) -> u128 {
	const WRAP: i128 = 1 << 33;
	let base = i128::from(to_ticks(at));
	let ahead = (i128::from(pts) - base).rem_euclid(WRAP);
	let ticks = base + if ahead >= WRAP / 2 { ahead - WRAP } else { ahead };
	(ticks.max(0) as u128) * 1_000_000_000 / 90_000
}

/// When each Dolby access unit that begins in a PES's `payload` leaves the decoder buffer and
/// the bytes it takes, counting on from `decode`, the PES's PTS: [`Schedule::push_placed`]'s
/// frames. The bytes ahead of the first unit go with it, and a frame the PES leaves
/// unfinished keeps its unit's time for the bytes it holds. Empty with fewer than two units,
/// where the whole PES leaves at once.
fn dolby_removals(codec: Dolby, payload: &[u8], decode: u128) -> Vec<(u128, usize)> {
	let Some(mut offset) = dolby_sync(codec, payload) else {
		return Vec::new();
	};
	let mut frames: Vec<(u128, usize)> = Vec::new();
	let (mut at, mut lead) = (decode, offset);
	while offset < payload.len() {
		let rest = &payload[offset..];
		let Ok(header) = codec.parse(rest) else {
			break;
		};
		let len = header.len.min(rest.len());
		match frames.last_mut() {
			// A substream leaves with its access unit, or the first when that began earlier.
			Some((_, bytes)) if !codec.begins_unit(rest) => *bytes += len,
			None if !codec.begins_unit(rest) => lead += len,
			_ => {
				frames.push((at, len));
				at += 1_000_000_000 * u128::from(header.samples) / u128::from(header.sample_rate.max(1));
			}
		}
		offset += len.max(1);
	}
	if let Some(head) = frames.first_mut() {
		head.1 += lead;
	}
	if let Some(tail) = frames.last_mut() {
		tail.1 += payload.len().saturating_sub(offset);
	}
	frames
}

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

/// Whether a changed SI snapshot may go out: `interval` has elapsed on the media
/// timeline since the entry last hit the wire.
///
/// A floor measured from the entry's own last emission, unlike [`due`]'s absolute
/// grid, so a revision goes out promptly (`SiTrack::dirty`, [`SI_REVISION_INTERVAL`])
/// while a publisher revising every frame still cannot drive the mux at that rate.
/// `None` (never emitted) is always due, so a fresh exporter leads with the tables.
fn si_due(timestamp: Timestamp, last: Option<Timestamp>, interval: Duration) -> bool {
	let Some(last) = last else {
		return true;
	};
	// Reordered (B-frame) timestamps step backwards; saturate rather than wrap so a
	// dip never counts as elapsed time (a zero interval still means "every frame").
	Duration::from(timestamp).saturating_sub(Duration::from(last)) >= interval
}

/// DVB's TDT and TOT: tables whose content is the current time, so a repeat of an
/// unchanged snapshot asserts a time already sent (#2934).
fn is_clock_table(table_id: u8) -> bool {
	matches!(table_id, 0x70 | 0x73)
}

/// Index of `timestamp`'s repetition slot: how many whole `interval`s fit under it.
///
/// Nanoseconds, so the divisor is zero only for a genuinely zero `interval`, which [`due`]
/// takes before it gets here. Coarser units would floor a sub-unit interval to zero and
/// divide by it.
fn slot(timestamp: Timestamp, interval: Duration) -> u128 {
	Duration::from(timestamp).as_nanos() / interval.as_nanos()
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

/// An adaptation field carrying nothing but `discontinuity_indicator`.
fn flags(discontinuity: bool) -> AdaptationField {
	AdaptationField {
		discontinuity_indicator: discontinuity,
		random_access_indicator: false,
		es_priority_indicator: false,
		pcr: None,
		opcr: None,
		splice_countdown: None,
		transport_private_data: Vec::new(),
		extension: None,
	}
}

/// External byte size of an adaptation field (manual mirror of the crate's
/// private `external_size`); only PCR is ever set.
fn adaptation_size(af: &AdaptationField) -> usize {
	2 + if af.pcr.is_some() { 6 } else { 0 }
}

/// Where an audio frame of `samples` at `rate` presented at `timestamp` sits on the media
/// timeline, counted in frames. Rounded, since a frame need not last whole microseconds:
/// AAC's 1,024 samples at 48 kHz.
fn place(timestamp: Timestamp, samples: u64, rate: u32) -> u128 {
	let frame = 1_000_000 * u128::from(samples);
	(timestamp.as_micros() * u128::from(rate) + frame / 2) / frame
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

/// Start a media track's source at the live edge, the newest group, so the export does not
/// begin on a group up to a delay old and carry that lag ever after; unless `replay`.
/// Verbatim streams are sparse, and anything of theirs ahead of the first keyframe is
/// dropped anyway, so they reach back as far as the skip budget allows.
fn at_edge(source: ExportSource, replay: bool) -> ExportSource {
	match replay {
		true => source,
		false => source.live(),
	}
}

/// A video level's T-STD limits (H.222.0 2.14.3.1, 2.17.2), in bits per second and bits.
#[derive(Clone, Copy, Debug)]
struct Level {
	/// The leak from MB into EB, Rbx: CpbBrNalFactor * MaxBR.
	leak: u64,
	/// BitRate without an HRD, which `factor` scales to Rx: 1200 * MaxBR for H.264, the
	/// least cpbBrNalFactor of any profile, and CpbBrVclFactor * MaxBR for H.265.
	bit_rate: u64,
	/// EB without an HRD: CpbBrNalFactor * MaxCPB.
	cpb: u64,
	/// Rx over an HRD's BitRate, in thousandths: 1.2 for H.264, CpbBrNalFactor /
	/// CpbBrVclFactor for H.265.
	factor: u64,
}

/// A video rendition's level limits from its catalog codec (H.264 Table A-1, H.265 Table
/// A.8). A level the tables don't list takes the lowest, which only sends its units earlier.
fn video_level(config: &VideoConfig, name: &str) -> Level {
	let level = match &config.codec {
		VideoCodec::H264(h264) => {
			// level_idc 11 with constraint_set3_flag is level 1b for Baseline, Main and Extended.
			let level_1b = h264.level == 11 && h264.constraints & 0x10 != 0 && matches!(h264.profile, 66 | 77 | 88);
			let limits = match h264.level {
				_ if level_1b => Some((128, 350)),
				9 => Some((128, 350)),
				10 => Some((64, 175)),
				11 => Some((192, 500)),
				12 => Some((384, 1_000)),
				13 => Some((768, 2_000)),
				20 => Some((2_000, 2_000)),
				21 | 22 => Some((4_000, 4_000)),
				30 => Some((10_000, 10_000)),
				31 => Some((14_000, 14_000)),
				32 => Some((20_000, 20_000)),
				40 => Some((20_000, 25_000)),
				41 | 42 => Some((50_000, 62_500)),
				50 => Some((135_000, 135_000)),
				51 | 52 | 60 => Some((240_000, 240_000)),
				61 => Some((480_000, 480_000)),
				62 => Some((800_000, 800_000)),
				_ => None,
			};
			limits.map(|(max_br, max_cpb)| Level {
				leak: 1_200 * max_br,
				bit_rate: 1_200 * max_br,
				cpb: 1_200 * max_cpb,
				factor: 1_200,
			})
		}
		VideoCodec::H265(h265) => {
			let limits = match (h265.level_idc, h265.tier_flag) {
				(30, false) => Some((128, 350)),
				(60, false) => Some((1_500, 1_500)),
				(63, false) => Some((3_000, 3_000)),
				(90, false) => Some((6_000, 6_000)),
				(93, false) => Some((10_000, 10_000)),
				(120, false) => Some((12_000, 12_000)),
				(120, true) => Some((30_000, 30_000)),
				(123, false) => Some((20_000, 20_000)),
				(123, true) => Some((50_000, 50_000)),
				(150, false) => Some((25_000, 25_000)),
				(150, true) => Some((100_000, 100_000)),
				(153, false) => Some((40_000, 40_000)),
				(153, true) => Some((160_000, 160_000)),
				(156 | 180, false) => Some((60_000, 60_000)),
				(156 | 180, true) => Some((240_000, 240_000)),
				(183, false) => Some((120_000, 120_000)),
				(183, true) => Some((480_000, 480_000)),
				(186, false) => Some((240_000, 240_000)),
				(186, true) => Some((800_000, 800_000)),
				_ => None,
			};
			limits.map(|(max_br, max_cpb)| Level {
				leak: 1_100 * max_br,
				bit_rate: 1_000 * max_br,
				cpb: 1_100 * max_cpb,
				factor: 1_100,
			})
		}
		_ => None,
	};
	level.unwrap_or_else(|| {
		tracing::warn!(track = %name, codec = %config.codec, "no T-STD limits for this level; sending its units early");
		Level {
			leak: 1_200 * 64,
			bit_rate: 1_200 * 64,
			cpb: 1_200 * 175,
			factor: 1_200,
		}
	})
}

/// How long `bits` take to arrive at `rate` bits per second: how far ahead of its decode
/// time a decoder buffer that size, filling at that rate, holds a unit.
fn fill(bits: u64, rate: u64) -> Duration {
	let nanos = u128::from(bits) * 1_000_000_000 / u128::from(rate.max(1));
	Duration::from_nanos(u64::try_from(nanos).unwrap_or(u64::MAX))
}

/// An audio stream's buffers in a receiver (H.222.0 2.4.2.3): by channel count for ADTS
/// AAC (and Opus, which borrows them), 2 Mb/s and the codec's main buffer for the rest. B
/// reaches as far ahead as it takes to fill at Rx.
fn audio_buffer(config: &AudioConfig, kind: &Kind) -> Buffer {
	let by_channels = || match config.channel_count {
		0..=2 => (2_000_000, 3_584),
		3..=8 => (5_529_600, 8_976),
		9..=12 => (8_294_400, 12_804),
		_ => (33_177_600, 51_216),
	};
	let (rate, size) = match kind {
		Kind::Aac { .. } | Kind::Opus { .. } => by_channels(),
		Kind::Mp2 { .. } => (2_000_000, 3_584),
		// ATSC A/53 Part 5 5.7, and A/52 Annex G 3.6.1 for E-AC-3.
		Kind::Ac3 => (2_000_000, 2_592),
		Kind::Eac3 => (2_000_000, 12_896),
		_ => (2_000_000, 3_584),
	};
	Buffer {
		rate,
		leak: None,
		size: Some(size),
		transport: 512,
		reach: fill(size as u64 * 8, rate),
	}
}

/// The DVB AC-3 descriptor's tag (ETSI EN 300 468 6.2.1), marking AC-3 as private data.
const AC3_DESCRIPTOR: u8 = 0x6a;

/// The DVB Enhanced AC-3 descriptor's tag (ETSI EN 300 468 Annex D).
const EAC3_DESCRIPTOR: u8 = 0x7a;

/// The DVB teletext descriptor's tag (ETSI EN 300 468 6.2.43).
const TELETEXT_DESCRIPTOR: u8 = 0x56;

/// A whole-PES stream's buffers, for the kinds its descriptors identify: Dolby audio as DVB
/// private data (ETSI TS 101 154 4.1.6.11), and teletext, whose 480-byte transport buffer drains at
/// 6.75 Mb/s (ETSI EN 300 472 5). Any other stream has none the schedule knows.
fn verbatim_buffer(kind: &Kind, descriptors: &[catalog::Descriptor]) -> Option<Buffer> {
	let Kind::PesPacket { stream_type: 0x06 } = kind else {
		return None;
	};
	descriptors.iter().find_map(|descriptor| match descriptor.tag {
		// ETSI TS 101 154 V2.9.1 4.1.6.11 recommends 5,696 bytes for both DVB codecs.
		AC3_DESCRIPTOR | EAC3_DESCRIPTOR => Some(Buffer {
			rate: 2_000_000,
			leak: None,
			size: Some(5_696),
			transport: 512,
			reach: fill(5_696 * 8, 2_000_000),
		}),
		TELETEXT_DESCRIPTOR => Some(Buffer {
			rate: 6_750_000,
			leak: None,
			size: None,
			transport: 480,
			reach: Duration::ZERO,
		}),
		_ => None,
	})
}

/// The Dolby audio a track passes through as DVB private data.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Dolby {
	Ac3,
	Eac3,
}

impl Dolby {
	/// The Dolby audio a private-data stream's descriptors identify.
	fn described(descriptors: &[catalog::Descriptor]) -> Option<Self> {
		descriptors.iter().find_map(|descriptor| match descriptor.tag {
			AC3_DESCRIPTOR => Some(Self::Ac3),
			EAC3_DESCRIPTOR => Some(Self::Eac3),
			_ => None,
		})
	}

	/// Bytes needed to attempt a header parse: an E-AC-3 stream's AC-3 core has the longer one.
	fn min_header_len(self) -> usize {
		let ac3 = crate::codec::ac3::DESCRIPTOR.min_header_len;
		match self {
			Self::Ac3 => ac3,
			Self::Eac3 => ac3.max(crate::codec::eac3::DESCRIPTOR.min_header_len),
		}
	}

	/// Parse the header of the sync frame at the start of `data`, of any substream.
	fn parse(self, data: &[u8]) -> crate::codec::legacy::Result<crate::codec::legacy::Header> {
		match self {
			Self::Eac3 if !ac3_core(data) => crate::codec::eac3::sync_header(data),
			_ => crate::codec::ac3::parse_header(data),
		}
	}

	/// Whether the sync frame at the start of `frame`, whose header parses, begins an access
	/// unit rather than adding a substream to the one before it.
	fn begins_unit(self, frame: &[u8]) -> bool {
		match self {
			Self::Eac3 if !ac3_core(frame) => crate::codec::eac3::begins_unit(frame),
			_ => true,
		}
	}
}

/// Whether the sync frame at the start of `data` is AC-3 (bsid 10 or less), which an E-AC-3
/// stream may carry as its independent substream 0 (ETSI TS 102 366 E.1.3.1.2).
fn ac3_core(data: &[u8]) -> bool {
	data.get(5).is_some_and(|byte| byte >> 3 <= 10)
}

/// Where the first Dolby sync frame in `payload` begins: a sync word whose header parses and
/// whose frame ends at the next sync word. Failing that, the first that may still begin one,
/// for the next PES to confirm: a frame that runs to the payload's end or past it, or a
/// header the payload cuts short. A sync word in the body of a frame begun before the join can
/// pass for a header, so it is taken only when nothing confirmed follows.
fn dolby_sync(codec: Dolby, payload: &[u8]) -> Option<usize> {
	let mut fallback = None;
	for (i, _) in payload.iter().enumerate().filter(|&(_, &byte)| byte == 0x0b) {
		let rest = &payload[i..];
		match codec.parse(rest) {
			Ok(header) => match rest.get(header.len..) {
				Some([0x0b, 0x77, ..]) => return Some(i),
				Some(next) if !ac3_sync_prefix(next) => continue,
				_ => {}
			},
			Err(_) if rest.len() < codec.min_header_len() && ac3_sync_prefix(rest) => {}
			Err(_) => continue,
		}
		fallback.get_or_insert(i);
	}
	fallback
}

/// Whether `bytes` could begin a Dolby sync word, for all they show of it.
fn ac3_sync_prefix(bytes: &[u8]) -> bool {
	bytes.iter().zip([0x0b, 0x77]).all(|(&byte, sync)| byte == sync)
}

/// Build the elementary-stream payload for a frame of `kind`, repeating an AAC program config
/// element when `pce`. Video needs the resolved avcC/hvcC to rewrite length-prefixed NALs as
/// Annex-B. Section-framed verbatim streams carry no PES payload, so they have none.
fn es_payload(kind: &Kind, description: Option<&Bytes>, frame: &Frame, pce: bool) -> anyhow::Result<Option<Vec<u8>>> {
	Ok(match kind {
		Kind::Video(stream_type) => Some(video_es_payload(*stream_type, description, frame)?),
		Kind::Aac { config, .. } => {
			let pce = match pce {
				true => config.program_config.as_deref().unwrap_or_default(),
				false => &[],
			};
			let raw_len = pce.len() + frame.payload.len();
			let header = adts::write_header(config.object_type, config.sample_rate, config.channel_config, raw_len)?;
			let mut framed = Vec::with_capacity(header.len() + raw_len);
			framed.extend_from_slice(&header);
			framed.extend_from_slice(pce);
			framed.extend_from_slice(&frame.payload);
			Some(framed)
		}
		// Each moq Opus frame is one packet; prefix the Opus-in-TS control header.
		Kind::Opus { .. } => Some(opus_es_payload(&frame.payload)),
		// Legacy audio frames were ingested whole (framing header included), so
		// they pass through untouched.
		Kind::Mp2 { .. } | Kind::Ac3 | Kind::Eac3 => Some(frame.payload.to_vec()),
		// Section-framed verbatim streams carry no PES payload.
		Kind::Verbatim { .. } => None,
		// The frame is the PES already, written by [`Export::place`].
		Kind::PesPacket { .. } => None,
	})
}

/// The PES stream_id a frame of `kind` goes out under, `None` to derive it from whether it is
/// video: Opus is private-data PES, carried under private_stream_1 like ffmpeg.
fn stream_id(kind: &Kind) -> Option<u8> {
	matches!(kind, Kind::Opus { .. }).then_some(StreamId::PRIVATE_STREAM_1)
}

/// Whether a video frame is an HEVC RADL picture, which leads its keyframe but references only
/// it, so it goes out wherever that keyframe does rather than being trimmed as an orphan.
fn radl(stream_type: StreamType, description: Option<&Bytes>, frame: &Frame) -> bool {
	stream_type == StreamType::H265
		&& description
			.and_then(|description| crate::codec::h265::hvcc_params(description).ok())
			.is_some_and(|(length_size, _)| crate::codec::h265::hvc1_is_radl(&frame.payload, length_size))
}

/// Build the Annex-B elementary-stream payload for one video frame: rewrite the
/// length-prefixed NALs to start-code-delimited NALs, injecting the parameter
/// sets (SPS/PPS, plus VPS for H.265) from the avcC/hvcC after a leading AUD on
/// keyframes so a receiver can tune in mid-stream.
fn video_es_payload(stream_type: StreamType, description: Option<&Bytes>, frame: &Frame) -> anyhow::Result<Vec<u8>> {
	let description = description.context("video codec config (avcC/hvcC) not resolved")?;
	let (codec, (length_size, params)) = match stream_type {
		StreamType::H264 => (InBandCodec::H264, crate::codec::h264::avcc_params(description)?),
		StreamType::H265 => (InBandCodec::H265, crate::codec::h265::hvcc_params(description)?),
		other => anyhow::bail!("unsupported TS video stream type {other:?}"),
	};

	let mut out = Vec::with_capacity(frame.payload.len() + 64);
	let mut nals = annexb::length_prefixed_nals(&frame.payload, length_size)?;
	if frame.keyframe {
		if let Some(aud) = nals.take_delimiter(codec)? {
			out.extend_from_slice(&annexb::START_CODE);
			out.extend_from_slice(aud);
		}
		for nal in &params {
			out.extend_from_slice(&annexb::START_CODE);
			out.extend_from_slice(nal);
		}
	}
	for nal in nals {
		out.extend_from_slice(&annexb::START_CODE);
		out.extend_from_slice(nal?);
	}
	Ok(out)
}

fn audio_kind(config: &AudioConfig, name: &str) -> anyhow::Result<Kind> {
	ensure_raw(&config.container, "audio", name)?;
	match &config.codec {
		AudioCodec::AAC(codec) => {
			// The description is exact, and names the LC core under explicit SBR or PS. Without
			// one, the catalog is all there is.
			let in_band = match &config.description {
				Some(asc) => aac::in_band(asc)?,
				None => aac::InBand {
					object_type: codec.profile,
					sample_rate: config.sample_rate,
					channel_config: adts::channel_config_from_count(config.channel_count)?,
					program_config: None,
				},
			};
			// A rebuilt kind repeats the element too, so a catalog update between a PAT/PMT and
			// this track's next frame cannot drop it.
			Ok(Kind::Aac {
				config: in_band,
				repeat: true,
			})
		}
		AudioCodec::Mp2 => Ok(Kind::Mp2 {
			sample_rate: config.sample_rate,
		}),
		AudioCodec::Opus => Ok(Kind::Opus {
			channel_config_code: opus_channel_code(config, name)?,
		}),
		AudioCodec::Ac3 => Ok(Kind::Ac3),
		AudioCodec::Ec3 => Ok(Kind::Eac3),
		other => anyhow::bail!("TS export does not support audio codec {other:?} (track '{name}')"),
	}
}

/// The two PMT descriptors for an Opus elementary stream: the `Opus` registration
/// descriptor (which sets the codec) and the DVB extension descriptor 0x80 carrying
/// the channel configuration. ffmpeg's demuxer requires both to recognize the stream.
///
/// `channel_config_code` is already a plain code ([`opus_channel_code`]): 1 is mono,
/// 2 is stereo, and 3..=8 is the Vorbis family 1 mapping for that many channels.
fn opus_descriptors(channel_config_code: u8) -> Vec<catalog::Descriptor> {
	vec![
		registration(b"Opus"),
		catalog::Descriptor {
			tag: 0x7f,
			// extension_descriptor_tag 0x80, then the plain channel_config_code.
			data: Bytes::copy_from_slice(&[0x80, channel_config_code]),
		},
	]
}

/// A registration descriptor (tag 0x05) naming the format identifier.
fn registration(format: &'static [u8; 4]) -> catalog::Descriptor {
	catalog::Descriptor {
		tag: 0x05,
		data: Bytes::from_static(format),
	}
}

/// Plain `channel_config_code` for an Opus track the extension descriptor can name.
///
/// Family 0, and family 1 with the Vorbis mapping, use that channel count. A missing
/// head is mono or stereo only. Every other layout is refused: the descriptor has no
/// code for it, and a clamped count would name a layout the packets do not have.
fn opus_channel_code(config: &AudioConfig, name: &str) -> anyhow::Result<u8> {
	let Some(description) = config.description.as_deref() else {
		anyhow::ensure!(
			matches!(config.channel_count, 1 | 2),
			"TS export cannot label Opus track '{name}' with {} channels and no OpusHead",
			config.channel_count
		);
		return Ok(config.channel_count as u8);
	};

	let mut buf = description;
	let head = opus::Config::parse(&mut buf)
		.map_err(|err| anyhow::anyhow!("TS export cannot read the OpusHead on track '{name}': {err}"))?;
	anyhow::ensure!(
		head.channel_count == config.channel_count,
		"Opus head has {} channels but the catalog declares {} (track '{name}')",
		head.channel_count,
		config.channel_count
	);

	if let Some(mapping) = &head.mapping {
		let channels = mapping.table().len() as u8;
		let vorbis = opus::Mapping::vorbis(channels).ok();
		anyhow::ensure!(
			vorbis.as_ref() == Some(mapping),
			"TS export cannot label Opus track '{name}': channel mapping family {} is not the Vorbis layout",
			mapping.family()
		);
	}

	let code = u8::try_from(head.channel_count).with_context(|| {
		format!(
			"TS export cannot label Opus track '{name}' with {} channels",
			head.channel_count
		)
	})?;
	anyhow::ensure!(
		(1..=8).contains(&code),
		"TS export cannot label Opus track '{name}' with {code} channels"
	);
	Ok(code)
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

/// Refuse an SI PID that would share a packet stream with PAT, PMT, null, or an ES.
fn reject_colliding_si_pids(mpegts: &catalog::Mpegts, pmt_pid: u16, es_pids: &[u16]) -> anyhow::Result<()> {
	for pid in mpegts.si.keys() {
		anyhow::ensure!(
			(1..0x1FFF).contains(pid) && *pid != pmt_pid && !es_pids.contains(pid),
			"mpegts.si PID {pid} collides with PAT, PMT, null, or an elementary stream"
		);
	}
	Ok(())
}

/// Refuse an `mpegts.tracks` entry that names both carriages, or carries a track the
/// catalog also describes as a media rendition: either leaves two ways to write one PID.
fn reject_ambiguous_tracks<E: catalog::Catalog>(catalog: &Catalog<E>, mpegts: &catalog::Mpegts) -> anyhow::Result<()> {
	for (name, track) in &mpegts.tracks {
		anyhow::ensure!(
			track.verbatim.is_none() || track.pes_packet.is_none(),
			"mpegts track '{name}' names both verbatim and pesPacket carriage"
		);
		anyhow::ensure!(
			(track.verbatim.is_none() && track.pes_packet.is_none())
				|| !(catalog.video.renditions.contains_key(name) || catalog.audio.renditions.contains_key(name)),
			"mpegts track '{name}' is carried undecoded but also described as a media rendition"
		);
	}
	Ok(())
}

/// The PMT descriptors recorded for `name` in the `mpegts` section, if any.
fn track_descriptors(mpegts: &catalog::Mpegts, name: &str) -> Vec<catalog::Descriptor> {
	mpegts
		.tracks
		.get(name)
		.map(|t| t.descriptors.clone())
		.unwrap_or_default()
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

/// The HRD declared by the first SPS in a video rendition's avcC/hvcC.
fn declared_hrd(stream_type: StreamType, description: &[u8]) -> Option<Hrd> {
	match stream_type {
		StreamType::H264 => {
			crate::codec::h264::sps_hrd(crate::codec::h264::Avcc::parse(description).ok()?.sps.first()?)
		}
		StreamType::H265 => {
			crate::codec::h265::sps_hrd(crate::codec::h265::Hvcc::parse(description).ok()?.sps.first()?)
		}
		_ => None,
	}
}

/// The reordering declared by the first SPS in a video rendition's avcC/hvcC.
fn declared_reorder(stream_type: StreamType, description: &[u8]) -> Option<Reorder> {
	match stream_type {
		StreamType::H264 => {
			crate::codec::h264::sps_reorder(crate::codec::h264::Avcc::parse(description).ok()?.sps.first()?)
		}
		StreamType::H265 => {
			crate::codec::h265::sps_reorder(crate::codec::h265::Hvcc::parse(description).ok()?.sps.first()?)
		}
		_ => None,
	}
}

#[cfg(test)]
mod tests {
	use std::time::Duration;

	use moq_net::Timestamp;

	use super::{
		Buffer, DecodeClock, Dolby, Kind, PSI_INTERVAL, PesPacket, Psi, Settle, dolby_removals, due,
		is_complete_section, si_due, slot, unwrap_pts, verbatim_buffer,
	};

	/// The tables as they stand take the version after the last laid, whatever was set on the
	/// schedule meanwhile, and the last laid's own when they come back to it. The PAT keeps its
	/// version until its network entry changes.
	#[test]
	fn a_revision_takes_the_version_after_the_last_laid() {
		use mpeg2ts::ts::{Pid, VersionNumber};

		let pmt = |pids: &[u16]| super::psi::Pmt {
			program_number: 1,
			pcr_pid: Pid::new(0x100).ok(),
			program_info: Vec::new(),
			streams: pids
				.iter()
				.map(|&pid| super::psi::Es {
					stream_type: 0x0f,
					pid: Pid::new(pid).unwrap(),
					descriptors: Vec::new(),
				})
				.collect(),
		};
		let mut psi = Psi {
			transport_stream_id: 1,
			pmt: pmt(&[0x100]),
			pmt_pid: 0x20,
			led: None,
			stated: None,
			laid: None,
			sending: Default::default(),
			out: None,
		};
		let first = VersionNumber::from_u8(3).unwrap();
		let laid = psi.current(false, first).unwrap();
		assert_eq!(laid.version.as_u8(), 3);
		psi.laid = Some(laid.clone());

		psi.pmt = pmt(&[0x100, 0x101]);
		let revised = psi.current(false, first).unwrap();
		assert_eq!(revised.version.as_u8(), 4);
		assert_eq!(revised.pat, laid.pat);

		psi.pmt = pmt(&[0x100]);
		assert_eq!(psi.current(false, first).unwrap().version.as_u8(), 3);

		let network = psi.current(true, first).unwrap();
		assert_eq!(network.version.as_u8(), 4);
		assert_ne!(network.pat, laid.pat);
	}

	/// The same available video/audio backlog must choose the same start in either fill order.
	#[tokio::test(start_paused = true)]
	async fn track_fill_observes_every_available_track_before_acquisition() {
		use crate::catalog::hang::Container;
		use crate::container::{Kind as MediaKind, Producer};

		for order in [[0, 1], [1, 0]] {
			let broadcast = moq_net::broadcast::Info::new().produce();
			let mut publishers = Vec::new();
			for name in ["video", "audio"] {
				let track = broadcast.create_track(name, hang::container::track_info(0)).unwrap();
				publishers.push(Producer::new(track, Container::Legacy(MediaKind::Data)));
			}
			let source = crate::source::announced(&broadcast.consume()).await;
			let mut tracks = Vec::new();
			for (pid, name, kind) in [
				(1, "video", Kind::Video(mpeg2ts::es::StreamType::H264)),
				(2, "audio", Kind::Mp2 { sample_rate: 48_000 }),
			] {
				let source = super::ExportSource::for_stream(&source, name, Duration::from_secs(2)).unwrap();
				let mut track = super::Track::new(source, kind);
				track.pid = pid;
				tracks.push(track);
			}
			let frame = |at| super::Frame {
				timestamp: ms(at),
				duration: None,
				payload: bytes::Bytes::from_static(b"frame"),
				keyframe: at % 600 == 0,
			};
			let start = moq_clock::now();
			let mut jitter = super::jitter::Buffer::new(Duration::from_millis(500));
			jitter.expect([super::TrackKey::new(0, "video"), super::TrackKey::new(0, "audio")]);
			let mut trimmed = 0;
			for publisher in &mut publishers {
				publisher.write(frame(0)).unwrap();
			}
			// As in Export::poll_next, the first held frame of each track arrives first.
			kio::wait(|waiter| {
				for (track, name) in tracks.iter_mut().zip(["video", "audio"]) {
					track
						.fill(
							name,
							waiter,
							&mut super::Input {
								waiting_for_header: false,
								video_start: None,
								jitter: &mut jitter,
								trimmed: &mut trimmed,
							},
						)
						.unwrap();
				}
				if jitter.len() == 2 {
					std::task::Poll::Ready(())
				} else {
					std::task::Poll::Pending
				}
			})
			.await;
			for (index, publisher) in publishers.iter_mut().enumerate() {
				let end = if index == 0 { 1_200 } else { 800 };
				for at in (40..=end).step_by(40) {
					publisher.write(frame(at)).unwrap();
				}
				publisher.finish().unwrap();
			}
			let waiter = kio::Waiter::noop();
			for index in order {
				tracks[index]
					.fill(
						["video", "audio"][index],
						&waiter,
						&mut super::Input {
							waiting_for_header: false,
							video_start: None,
							jitter: &mut jitter,
							trimmed: &mut trimmed,
						},
					)
					.unwrap();
			}
			assert!(jitter.poll_next(&waiter).is_pending());
			// The last available audio frame, including its scheduling lead, anchors both orders.
			let (decode, key, _) = tracks[1].released.unwrap();
			assert_eq!(decode.as_micros(), 800_000);
			assert_eq!(jitter.at(0, key), Some(start + Duration::from_millis(500)), "{order:?}");
			tokio::time::advance(Duration::from_millis(500)).await;
			let video = std::iter::from_fn(|| match jitter.poll_next(&waiter) {
				std::task::Poll::Ready(ready) => Some(ready),
				std::task::Poll::Pending => None,
			})
			.find(|ready| ready.track.name == "video")
			.expect("a video start");
			assert_eq!(video.item.frame.timestamp.as_micros(), 600_000, "{order:?}");
		}
	}

	fn ms(value: u64) -> Timestamp {
		Timestamp::from_millis(value).unwrap()
	}

	#[test]
	fn video_es_payload_keeps_aud_first() {
		for (stream_type, params, aud, slice) in [
			(
				super::StreamType::H264,
				vec![vec![0x67, 0x42, 0, 0x1f], vec![0x68, 0xce]],
				vec![0x09, 0x10],
				vec![0x65, 0x88],
			),
			(
				super::StreamType::H265,
				vec![vec![0x40, 1], vec![0x42, 1], vec![0x44, 1]],
				vec![0x46, 1, 0x10],
				vec![0x26, 1, 0x88],
			),
		] {
			let params: Vec<bytes::Bytes> = params.into_iter().map(bytes::Bytes::from).collect();
			let description = if stream_type == super::StreamType::H264 {
				crate::codec::h264::build_avcc(&params[..1], &params[1..]).unwrap()
			} else {
				let mut hvcc = vec![0; 23];
				hvcc[21] = 3;
				hvcc[22] = 3;
				for (kind, nal) in (32..=34).zip(&params) {
					hvcc.extend_from_slice(&[kind, 0, 1]);
					hvcc.extend_from_slice(&(nal.len() as u16).to_be_bytes());
					hvcc.extend_from_slice(nal);
				}
				hvcc.into()
			};
			for keyframe in [false, true] {
				for leading in [None, Some(aud.as_slice())] {
					let mut payload = Vec::new();
					for nal in leading.into_iter().chain([slice.as_slice()]) {
						payload.extend_from_slice(&(nal.len() as u32).to_be_bytes());
						payload.extend_from_slice(nal);
					}
					let frame = super::Frame {
						timestamp: ms(0),
						payload: payload.into(),
						keyframe,
						duration: None,
					};
					let mut expected = Vec::new();
					if let Some(aud) = leading {
						expected.extend_from_slice(&super::annexb::START_CODE);
						expected.extend_from_slice(aud);
					}
					if keyframe {
						expected.extend_from_slice(&super::annexb::build_prefix(&params));
					}
					expected.extend_from_slice(&super::annexb::START_CODE);
					expected.extend_from_slice(&slice);
					assert_eq!(
						super::video_es_payload(stream_type, Some(&description), &frame).unwrap(),
						expected
					);
				}
			}
		}
	}

	/// A six-block E-AC-3 sync frame of 512 bytes: `stream` is byte 2's top bits (strmtyp,
	/// substreamid) and `rate` byte 4 (fscod, numblkscod, acmod, lfeon).
	fn eac3(stream: u8, rate: u8) -> Vec<u8> {
		let mut frame = vec![0x0b, 0x77, stream, 0xff, rate, 0x80];
		frame.resize(512, 0xaa);
		frame
	}

	/// Teletext passed through as DVB private data takes its transport buffer's rate from
	/// EN 300 472, so the schedule spreads it no faster than a receiver passes it on; private
	/// data nothing identifies has no buffers.
	#[test]
	fn teletext_drains_at_its_own_rate() {
		let kind = Kind::PesPacket { stream_type: 0x06 };
		let descriptor = |tag| super::catalog::Descriptor {
			tag,
			data: bytes::Bytes::from_static(b"eng\x09\x00"),
		};
		assert_eq!(
			verbatim_buffer(&kind, &[descriptor(0x56)]),
			Some(Buffer {
				rate: 6_750_000,
				leak: None,
				size: None,
				transport: 480,
				reach: Duration::ZERO,
			})
		);
		assert_eq!(verbatim_buffer(&kind, &[descriptor(0x0a)]), None);
	}

	/// Push a decode-order PTS stream (90 kHz) through the decode clock under a catalog
	/// `jitter` and a declared reorder `floor`, flushing at the end, and return the DTS per
	/// frame.
	fn run_clock(pts: &[u64], jitter: u64, floor: u64) -> Vec<u64> {
		let mut clock = DecodeClock::default();
		let mut dts = vec![0; pts.len()];
		for (i, &p) in pts.iter().enumerate() {
			clock.push(i, p, moq_clock::Instant::ZERO);
			while let Some((i, d)) = clock.pop(jitter, floor, Settle::Due) {
				dts[i] = d;
			}
		}
		while let Some((i, d)) = clock.pop(jitter, floor, Settle::All) {
			dts[i] = d;
		}
		dts
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

	/// How an encoder times `pts`: the n-th frame decodes at the n-th presentation time, a
	/// fixed `delay` early. Returns the DTS and the catalog `jitter` an importer would publish.
	fn source_dts(pts: &[u64], delay: u64) -> (Vec<u64>, u64) {
		let mut slots = pts.to_vec();
		slots.sort();
		let dts: Vec<u64> = slots.iter().map(|slot| slot - delay).collect();
		let jitter = pts.iter().zip(&dts).map(|(p, d)| p - d).max().unwrap();
		(dts, jitter)
	}

	#[test]
	fn dts_is_monotonic_across_reorder() {
		// 25 fps, 10 s offset. Even with nothing declared, the decode timeline is strictly
		// increasing (the `+igndts` fix), and past the first reordered frames it stays at or
		// before the PTS.
		for b in [1, 3, 5] {
			let pts = decode_order(40, b, 3_600, 10_000_000);
			assert!(pts.windows(2).any(|w| w[1] < w[0]), "b={b}: stream must reorder PTS");
			let dts = run_clock(&pts, 0, 0);
			for (i, win) in dts.windows(2).enumerate() {
				assert!(win[1] > win[0], "b={b}: DTS not strictly increasing at {i}: {win:?}");
			}
			let settled = 2 * (b + 1);
			for (i, (&d, &p)) in dts.iter().zip(pts.iter()).enumerate().skip(settled) {
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
		let dts = run_clock(&pts, 0, 0);

		assert!(pts.iter().any(|&p| p >= wrap), "test must cross the wrap boundary");
		for (i, win) in dts.windows(2).enumerate() {
			assert!(
				win[1] > win[0],
				"DTS not strictly increasing across wrap at {i}: {win:?}"
			);
		}
	}

	#[test]
	fn dts_without_reorder_is_the_pts() {
		let pts: Vec<u64> = (0..40).map(|i| 10_000_000 + i * 3_600).collect();
		assert_eq!(run_clock(&pts, 0, 0), pts);
	}

	/// Given the catalog `jitter` and the SPS's reorder depth, a reordered stream decodes
	/// exactly as its encoder timed it, a frame period apart.
	#[test]
	fn reordered_dts_is_the_encoders() {
		let dur = 3_600;
		for b in [1, 2, 3] {
			let pts = decode_order(40, b, dur, 10_000_000);
			let (source, jitter) = source_dts(&pts, dur);
			assert_eq!(run_clock(&pts, jitter, dur), source, "b={b}");
		}
	}

	/// A field-coded passage decodes a field apart and a frame-coded one a frame apart: the
	/// reorder delay is a time, so a switch between them does not squeeze the timeline.
	#[test]
	fn a_switch_to_field_coding_keeps_the_encoders_spacing() {
		let frame = 3_600;
		let mut pts = decode_order(10, 2, frame, 10_000_000);
		// Then each picture is two fields, a reference pair ahead of the B pairs it predicts.
		let start = pts.iter().max().unwrap() + frame;
		for g in 0..10u64 {
			let reference = start + (g * 3 + 2) * frame;
			let pair = |at: u64| [at, at + frame / 2];
			pts.extend(pair(reference));
			pts.extend(pair(reference - 2 * frame));
			pts.extend(pair(reference - frame));
		}
		let (source, jitter) = source_dts(&pts, frame);
		assert_eq!(run_clock(&pts, jitter, frame), source);
	}

	/// An open GOP's leading pictures, read after the keyframe they precede in presentation,
	/// decode a slot apart rather than one tick behind it.
	#[test]
	fn leading_pictures_decode_a_slot_apart() {
		let frame = 3_600;
		let base = 10_000_000;
		// The keyframe is presented after three leading B-frames.
		let mut pts = vec![base + 3 * frame, base, base + frame, base + 2 * frame];
		pts.extend(decode_order(10, 2, frame, base + 3 * frame).into_iter().skip(1));
		let (source, jitter) = source_dts(&pts, frame);
		let dts = run_clock(&pts, jitter, frame);
		assert_eq!(dts, source);
		for (i, win) in dts.windows(2).enumerate() {
			assert!(win[1] - win[0] >= frame, "DTS less than a frame apart at {i}: {dts:?}");
		}
	}

	#[test]
	fn dts_is_join_independent_at_a_peak() {
		// An exporter that has been running and one that just joined author the same decode
		// timeline from any frame whose PTS leads everything decoded before it. A keyframe is
		// exactly that (export only ever tunes in on one).
		for b in [1, 3, 5] {
			let pts = decode_order(40, b, 3_600, 10_000_000);
			let (_, jitter) = source_dts(&pts, 3_600);
			let running = run_clock(&pts, jitter, 3_600);

			let mut peaks = 0;
			for k in 1..pts.len() {
				if pts[..k].iter().any(|&p| p >= pts[k]) {
					continue;
				}
				peaks += 1;
				let fresh = run_clock(&pts[k..], jitter, 3_600);
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

		// The slot grid honors whatever interval it is given, not just the PSI's own.
		let coarse = Duration::from_millis(2_000);
		assert!(!due(ms(1_500), Some(ms(1_000)), coarse));
		assert!(due(ms(3_000), Some(ms(1_000)), coarse));
	}

	#[test]
	fn si_due_floors_repeats_from_the_last_emission() {
		// Never emitted: always due, so a fresh exporter leads with the tables.
		assert!(si_due(ms(1_000), None, PSI_INTERVAL));

		// The interval is measured from the entry's own last emission, not an absolute
		// grid: an emission at 1.4s holds the next repeat to 3.4s, where the grid
		// would have re-sent at 2s.
		let interval = Duration::from_millis(2_000);
		assert!(!si_due(ms(2_000), Some(ms(1_400)), interval));
		assert!(!si_due(ms(3_399), Some(ms(1_400)), interval));
		assert!(si_due(ms(3_400), Some(ms(1_400)), interval));

		// A reordered (B-frame) timestamp behind the last emission is not elapsed time.
		assert!(!si_due(ms(1_000), Some(ms(1_400)), interval));
		// A zero interval means every frame, even one sharing the last timestamp.
		assert!(si_due(ms(1_400), Some(ms(1_400)), Duration::ZERO));
	}

	/// Drive a run of timestamps through the slot cadence, advancing the stored emission
	/// only when one fires, and return how many tables it emitted. PSI additionally emits
	/// (and re-anchors) at every video keyframe; SI does not use the grid at all
	/// ([`si_due`] floors unchanged repeats from the last emission instead).
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

	/// A PES of `stream_id` with an optional header of `flags` and `fields`, its length right.
	fn pes(stream_id: u8, flags: u8, fields: &[u8], payload: &[u8]) -> Vec<u8> {
		let mut pes = vec![0, 0, 1, stream_id];
		pes.extend_from_slice(&((3 + fields.len() + payload.len()) as u16).to_be_bytes());
		pes.extend_from_slice(&[0x84, flags, fields.len() as u8]);
		pes.extend_from_slice(fields);
		pes.extend_from_slice(payload);
		pes
	}

	#[test]
	fn pes_packet_validation() {
		// A PTS of 0x1_2345_6789 behind the '0010' prefix, read back as written.
		let pts = [0x29, 0x8d, 0x15, 0xcf, 0x13];
		let parsed = PesPacket::parse(&pes(0xbd, 0x80, &pts, b"data"), 0x06).unwrap();
		assert_eq!(parsed.pts, Some(0x1_2345_6789));
		assert_eq!(parsed.payload, 14);
		assert_eq!(
			PesPacket::parse(&pes(0xbd, 0x00, &[], b"data"), 0x06).unwrap().pts,
			None
		);
		// private_stream_2 and padding carry no optional header.
		let bare = [0, 0, 1, 0xbf, 0, 2, 0xaa, 0xbb];
		assert_eq!(PesPacket::parse(&bare, 0x06).unwrap().payload, 6);

		// No start code, a length that disagrees, or a header that runs past the packet.
		assert!(PesPacket::parse(&[0, 0, 2, 0xbd, 0, 0], 0x06).is_err());
		let mut long = pes(0xbd, 0x00, &[], b"data");
		long.push(0);
		assert!(PesPacket::parse(&long, 0x06).is_err());
		let mut truncated = pes(0xbd, 0x00, &[], &[]);
		truncated[8] = 4;
		assert!(PesPacket::parse(&truncated, 0x06).is_err());
		// The marker bits, the forbidden PTS_DTS_flags, and fields past the header length.
		let mut marker = pes(0xbd, 0x00, &[], b"data");
		marker[6] = 0x44;
		assert!(PesPacket::parse(&marker, 0x06).is_err());
		assert!(PesPacket::parse(&pes(0xbd, 0x40, &pts, b""), 0x06).is_err());
		assert!(PesPacket::parse(&pes(0xbd, 0xc0, &pts, b""), 0x06).is_err());
		assert!(PesPacket::parse(&pes(0xbd, 0x01, &[0x80, 0, 0], b""), 0x06).is_err());
		// A clock field with the wrong prefix, or a marker bit cleared, PTS and DTS alike.
		let mut prefix = pts;
		prefix[0] = 0x39;
		assert!(PesPacket::parse(&pes(0xbd, 0x80, &prefix, b""), 0x06).is_err());
		let mut marker = pts;
		marker[2] &= !1;
		assert!(PesPacket::parse(&pes(0xbd, 0x80, &marker, b""), 0x06).is_err());
		let mut both = [pts, pts].concat();
		both[0] = 0x39;
		both[5] = 0x19;
		assert!(PesPacket::parse(&pes(0xbd, 0xc0, &both, b""), 0x06).is_ok());
		both[9] &= !1;
		assert!(PesPacket::parse(&pes(0xbd, 0xc0, &both, b""), 0x06).is_err());

		// Only video may leave the length unbounded: its stream IDs, or 0xFD under a video type.
		let unbounded = |stream_id: u8| {
			let mut pes = pes(stream_id, 0x00, &[], b"picture");
			pes[4..6].fill(0);
			pes
		};
		assert!(PesPacket::parse(&unbounded(0xe0), 0x06).is_ok());
		assert!(PesPacket::parse(&unbounded(0xfd), 0xea).is_ok());
		assert!(PesPacket::parse(&unbounded(0xfd), 0x06).is_err());
		assert!(PesPacket::parse(&unbounded(0xbd), 0x06).is_err());
	}

	#[test]
	fn placed_pts_unwraps_near_its_placement() {
		// A PTS just past the 33-bit wrap belongs after a placement just before it.
		let at = Timestamp::from_micros(((1u64 << 33) - 900) * 1_000_000 / 90_000).unwrap();
		assert_eq!(unwrap_pts(900, at), ((1u128 << 33) + 900) * 1_000_000_000 / 90_000);
		// A PTS a little ahead, and one a little behind, stay where they are.
		let at = ms(10_000);
		assert_eq!(unwrap_pts(90 * 10_100, at), 10_100_000_000);
		assert_eq!(unwrap_pts(90 * 9_900, at), 9_900_000_000);
	}

	#[test]
	fn dolby_removals_count_on_from_the_pts() {
		// 32 kb/s 48 kHz sync frames: 128 bytes, 32 ms each.
		let mut sync = vec![0x0b, 0x77, 0x00, 0x00, 0x00, 0x40, 0x40];
		sync.resize(128, 0);
		let mut payload = vec![0xee; 10];
		payload.extend(sync.repeat(2));
		payload.extend_from_slice(&sync[..50]);
		let removals = dolby_removals(Dolby::Ac3, &payload, 1_000_000_000);
		// The tail of an earlier frame goes with the first, and the cut frame keeps its time.
		assert_eq!(
			removals,
			vec![(1_000_000_000, 138), (1_032_000_000, 128), (1_064_000_000, 50)]
		);
		assert!(
			dolby_removals(Dolby::Ac3, &[0xee; 20], 0).is_empty(),
			"no sync frame begins"
		);
	}

	/// E-AC-3 substreams leave with their access unit, so a PES's units leave a 32 ms interval
	/// apart whatever substreams each carries.
	#[test]
	fn dolby_removals_keep_eac3_substreams_with_their_unit() {
		let (independent, dependent) = (eac3(0x00, 0x3f), eac3(0x40, 0x3f));
		let payload = [
			&dependent[..],
			&independent,
			&dependent,
			&independent,
			&dependent[..100],
		]
		.concat();
		let removals = dolby_removals(Dolby::Eac3, &payload, 1_000_000_000);
		// The substream ending an earlier unit goes with the first, the cut one with the last.
		assert_eq!(removals, vec![(1_000_000_000, 1_536), (1_032_000_000, 612)]);
	}
}
