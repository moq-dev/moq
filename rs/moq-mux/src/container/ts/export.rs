//! MPEG-TS muxer.
//!
//! [`Export`] subscribes to a MoQ broadcast and produces MPEG-TS: PAT/PMT program
//! tables and PES packets, packetized into 188-byte TS packets, with the PCR
//! riding its own adaptation-field-only packets on a fixed media-time grid.
//! Output is sliced on that grid rather than per media frame (the schedule):
//! each [`Frame`] is one slot's clock packet plus the bytes belonging to it,
//! stamped at the slot boundary, so the clock a receiver recovers from byte
//! position agrees with the values, and a pacing caller releases each slot at
//! the instant it asserts. Video is carried as Annex-B, audio as ADTS AAC.
//!
//! Every frame is muxed a fixed delay after its decode time ([`Export::with_delay`]),
//! so tracks interleave in `(DTS, PID)` order whatever the arrival skew between them,
//! and the output keeps the source's pace.
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
use mpeg2ts::ts::payload::{Bytes as TsBytes, Pat, Pes, Pmt, Section};
use mpeg2ts::ts::{
	AdaptationField, ContinuityCounter, Descriptor, EsInfo, Pid, ProgramAssociation, TransportScramblingControl,
	TsHeader, TsPacket, TsPacketWriter, TsPayload, VersionNumber, WriteTsPacket,
};

use moq_net::Timestamp;

use crate::catalog::hang::Catalog;
use crate::catalog::{CatalogFormat, Stream};
use crate::codec::video::{Hrd, Reorder};
use crate::codec::{aac, annexb, opus};
use crate::container::{ExportSource, Frame};
use crate::jitter::{self, Arrival, Push};

use super::adts;
use super::catalog;
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
/// is re-emitted at video keyframes and periodically for mid-stream tune-in.
/// Returns `None` when the broadcast ends.
pub struct Export<E: catalog::Catalog = ()> {
	source: crate::Source,
	catalog: Option<crate::catalog::Consumer<E>>,
	catalog_format: CatalogFormat,
	/// Tracks [`Self::resume`] left on the ended broadcast, resubscribed as the
	/// returned catalog lists them.
	stale: HashSet<String>,
	/// How long after its decode time each frame goes out.
	delay: Duration,
	/// Holds every track's frames until `delay` past their decode time, keyed by PID.
	jitter: jitter::Buffer<u16, Queued>,
	/// A frame the jitter buffer let go, waiting for the tail of the generation before it to go out.
	held: Option<jitter::Ready<u16, Queued>>,
	/// Jitter-buffer generation of the program being muxed; a newer one rewinds it.
	generation: u64,

	tracks: HashMap<String, Track>,
	/// The next continuity counter per PID, numbered as packets go out.
	counters: HashMap<u16, u8>,
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

	/// Program tables, built once the track layout is known.
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
	slot_timer: Option<std::pin::Pin<Box<web_async::time::Sleep>>>,
	/// Output frames ready to hand out, one per grid slot, each with what it tells
	/// [`Self::stats`] once returned.
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
	/// Each elementary stream's access units and silence on the PCR returned ([`Self::stats`]).
	liveness: super::import::Liveness,
}

/// What a queued output frame tells [`Export::stats`], applied only once the frame is
/// returned: a rewind drops the queue unwritten.
struct Tally {
	/// The frame's PCR in 27 MHz ticks, and whether it flags a new time base.
	pcr: (u64, bool),
	/// The PID of each access unit whose packets begin in the frame.
	units: Vec<u16>,
}

/// A frame read from its source.
struct Pending {
	frame: Frame,
	/// How many times the source had restarted its timeline when the frame was read.
	restart: u64,
	/// How many times its playhead had jumped, restarts included ([`jitter::Arrival::skip`]).
	skip: u64,
	/// The earliest the frame could have arrived: when its source was last found empty.
	arrived: web_async::time::Instant,
	/// When it was read: the latest it could have arrived.
	read: web_async::time::Instant,
}

/// A frame waiting in the jitter buffer, with what it needs from the moment it was read.
struct Queued {
	frame: Frame,
	/// When the frame decodes: its DTS, else its PTS.
	decode: Timestamp,
	/// Authored decode timestamp, as [`PesUnit::dts`].
	dts: Option<u64>,
	/// The avcC/hvcC the frame was read under, for video.
	description: Option<Bytes>,
}

struct Track {
	source: ExportSource,
	/// The first frame, held until the program tables are built.
	pending: Option<Pending>,
	/// When the source was last found empty. A frame read since arrived no earlier, and
	/// no later than it is read; the jitter buffer judges it on the earlier bound, so a caller
	/// that polls late (a sink sleeping to pace its writes) does not make it late.
	empty: web_async::time::Instant,
	/// The source's restart counter the decode clock runs under.
	restart: u64,
	finished: bool,
	pid: u16,
	kind: Kind,
	/// PMT ES-level descriptors to re-announce, captured verbatim on import (language,
	/// registration, ...). Empty for non-TS sources; AC-3/E-AC-3 then synthesize one.
	descriptors: Vec<catalog::Descriptor>,
	/// Authors each video frame's DTS, holding the frames until it can.
	clock: DecodeClock<(Pending, Option<Bytes>)>,
	/// What the catalog and the SPS declare about the video's reordering.
	timing: Timing,
	/// The receiver's buffers for an audio or verbatim track; video's come from [`Timing`].
	buffer: Option<Buffer>,
}

impl Track {
	/// A track reading `source`, its PID filled in by the catalog.
	fn new(source: ExportSource, kind: Kind) -> Self {
		Self {
			source,
			pending: None,
			empty: web_async::time::Instant::now(),
			restart: 0,
			finished: false,
			pid: 0,
			kind,
			descriptors: Vec::new(),
			clock: DecodeClock::default(),
			timing: Timing::default(),
			buffer: None,
		}
	}

	/// Queue a frame read from the source in the jitter buffer, once its decode time is known.
	///
	/// The decode clock runs here, as frames are read, because the deadline is on decode time:
	/// reordered video arrives in decode order with PTS 0, 120, 40, 80, so a PTS deadline would
	/// strand a B-frame behind its reference or send it first.
	fn queue(&mut self, name: &str, pending: Pending, jitter: &mut jitter::Buffer<u16, Queued>) -> anyhow::Result<()> {
		if pending.restart != self.restart {
			// The source may have restarted its timeline, so the decode clock restarts too.
			self.release(name, jitter, true)?;
			self.restart = pending.restart;
			self.clock = DecodeClock::default();
		}
		let Kind::Video(stream_type) = self.kind else {
			return self.push(name, pending, None, None, jitter);
		};
		if pending.frame.keyframe {
			self.timing.describe(stream_type, self.source.description(), name);
		}
		let pts = to_ticks(pending.frame.timestamp);
		self.clock.push((pending, self.source.description().cloned()), pts);
		self.release(name, jitter, false)
	}

	/// Whether the track passes AC-3 through as DVB private data, whose PES may carry several
	/// sync frames.
	fn carries_ac3(&self) -> bool {
		matches!(
			self.kind,
			Kind::Verbatim {
				stream_type: 0x06,
				framing: catalog::Framing::Pes,
				..
			}
		) && self
			.descriptors
			.iter()
			.any(|descriptor| descriptor.tag == AC3_DESCRIPTOR)
	}

	/// The receiver's buffers for the track's PID, if the T-STD gives it any.
	fn buffer(&self) -> Option<Buffer> {
		match self.kind {
			Kind::Video(_) => self.timing.buffer(),
			_ => self.buffer,
		}
	}

	/// How long a receiver takes to pass one of the track's packets on, so its last packet has
	/// to arrive that long before it decodes. A packet's bytes reach a decoder buffer only once
	/// they have drained through the T-STD buffers ahead of it (ISO 13818-1 2.4.2); without it
	/// a unit whose DTS sits on a PCR slot boundary, as a source's own 25 fps timestamps put
	/// every fifth frame, arrives complete only after it decodes.
	fn drain(&self) -> Duration {
		let rate = self.buffer().map_or(Buffer::SYSTEM.rate, |buffer| buffer.rate);
		Duration::from_nanos(TsPacket::SIZE as u64 * 8 * 1_000_000_000 / rate.max(1))
	}

	/// Queue every held video frame whose DTS is settled, or all of them when `flush`.
	fn release(&mut self, name: &str, jitter: &mut jitter::Buffer<u16, Queued>, flush: bool) -> anyhow::Result<()> {
		let (delay, lookahead) = self.timing.reorder();
		let lookahead = lookahead.max(self.timing.jitter);
		while let Some(((pending, description), dts)) = self.clock.pop(lookahead, delay, flush) {
			self.push(name, pending, Some(dts), description, jitter)?;
		}
		Ok(())
	}

	/// Queue a frame decoding at `dts` (90 kHz ticks), else at its PTS.
	fn push(
		&mut self,
		name: &str,
		pending: Pending,
		dts: Option<u64>,
		description: Option<Bytes>,
		jitter: &mut jitter::Buffer<u16, Queued>,
	) -> anyhow::Result<()> {
		let Pending {
			frame,
			restart,
			skip,
			arrived,
			read,
		} = pending;
		let dts = dts.filter(|&dts| dts != to_ticks(frame.timestamp));
		let decode = dts
			.and_then(|ticks| Timestamp::from_scale(ticks, 90_000).ok())
			.unwrap_or(frame.timestamp);
		let arrival = Arrival {
			arrived,
			read,
			decode,
			restart,
			skip,
			sync: frame.keyframe || !matches!(self.kind, Kind::Video(_)),
			item: Queued {
				frame,
				decode,
				dts,
				description,
			},
		};
		if jitter.push(self.pid, arrival)? == Push::Late {
			tracing::warn!(track = %name, dropped = jitter.dropped(), "frame missed its deadline; dropped");
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
	/// The HRD its SPS declares.
	hrd: Option<Hrd>,
}

impl Timing {
	/// Take the rendition's timing from a catalog snapshot, before or after the program
	/// tables are written.
	fn configure(&mut self, config: &VideoConfig, name: &str) {
		self.jitter = config.jitter.map_or(0, |t| (t.as_micros() * 90_000 / 1_000_000) as u64);
		self.framerate = config.framerate.filter(|fps| fps.is_finite() && *fps > 0.0);
		self.level = Some(video_level(config, name));
	}

	/// Read the reorder depth from the codec config a keyframe is carried with.
	fn describe(&mut self, stream_type: StreamType, description: Option<&Bytes>, name: &str) {
		if self.description.as_ref() == description {
			return;
		}
		self.description = description.cloned();
		self.declared = description.and_then(|d| declared_reorder(stream_type, d));
		self.hrd = description.and_then(|d| declared_hrd(stream_type, d));
		tracing::debug!(track = %name, reorder = ?self.reorder(), "video reordering declared");
	}

	/// The receiver's buffers (H.222.0 2.14.3.1, 2.17.2): EB is the HRD's CPB, else the
	/// level's, and packets go no faster than the slower of the transport buffer's Rx and the
	/// leak from MB into EB, so neither MB nor TB fills.
	fn buffer(&self) -> Option<Buffer> {
		let level = self.level?;
		let (rate, cpb) = match self.hrd {
			Some(hrd) => (hrd.bit_rate * level.factor / 1_000, hrd.cpb_size),
			None => (level.leak, level.cpb),
		};
		Some(Buffer {
			rate: rate.min(level.leak),
			size: Some(usize::try_from(cpb / 8).unwrap_or(usize::MAX)),
		})
	}

	/// The least reorder delay and lookahead the SPS declares, in ticks, so two exporters agree
	/// on them from their first keyframe whatever reordering each has seen.
	///
	/// The delay is the SPS's depth in pictures, at its picture period or the catalog
	/// `framerate`'s. A frame can be presented up to `2^depth - 1` pictures below one decoded
	/// before it (a B-pyramid that deep), which bounds the lookahead the catalog `jitter`
	/// gives when it is not published.
	fn reorder(&self) -> (u64, u64) {
		let Some(declared) = self.declared else {
			return (0, 0);
		};
		let period = match declared.period {
			Some((units, scale)) => units.saturating_mul(90_000).div_ceil(scale),
			None => self.framerate.map_or(0, |fps| (90_000.0 / fps).ceil() as u64),
		};
		let delay = u64::from(declared.depth).saturating_mul(period);
		let lookahead = 1u64
			.checked_shl(declared.depth)
			.unwrap_or(u64::MAX)
			.saturating_mul(period);
		(delay.min(MAX_REORDER), lookahead.min(MAX_REORDER))
	}
}

#[derive(Clone)]
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

impl Kind {
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
			Kind::Verbatim { .. } => ".ts",
		}
	}

	/// The classification [`Import`](super::Import) would give this PID, so a row is graded
	/// alike at both edges.
	fn class(&self) -> super::stats::Class {
		match self {
			Kind::Video(_) => super::stats::Class::Video,
			Kind::Aac { .. } | Kind::Opus { .. } | Kind::Mp2 { .. } | Kind::Ac3 | Kind::Eac3 => {
				super::stats::Class::Audio
			}
			Kind::Verbatim { .. } => super::stats::Class::Data,
		}
	}
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
	Requesting(kio::Pending<moq_net::origin::Requesting>, String),
	/// Waiting for the subscription to resolve.
	Subscribing(kio::Pending<moq_net::track::Subscribing>),
	/// The resolved subscription, reading snapshot groups.
	Active(moq_net::track::Ordered),
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
				match pending.poll_ok(waiter) {
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
				Ok(track) => SiState::Active(track.ordered()),
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
		let catalog = source.catalog::<E>(catalog_format).await?;
		Ok(Self {
			source,
			catalog: Some(catalog),
			catalog_format,
			stale: HashSet::new(),
			delay: Duration::ZERO,
			jitter: jitter::Buffer::new(Duration::ZERO),
			held: None,
			generation: 0,
			tracks: HashMap::new(),
			counters: HashMap::new(),
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
			mux_rate: None,
			mux_rate_override: None,
		})
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
		self.mux_rate_override = Some(mux_rate);
		self.mux_rate = sanitize_mux_rate(mux_rate);
		self.schedule.set_rate(self.mux_rate);
		self
	}

	/// Read each track from the oldest group the delay still reaches instead of the live
	/// edge, and anchor the clock on the first frame instead of acquiring it, for a test that
	/// writes a broadcast whole before exporting it.
	#[cfg(test)]
	pub(super) fn with_replay(mut self) -> Self {
		self.replay = true;
		self.jitter = jitter::Buffer::new(self.delay).replay();
		self
	}

	/// Mux each frame this long after its decode time, like an SRT receiver's TSBPD.
	///
	/// The clock is acquired before anything goes out: frames are held until a track starts
	/// a new group, or for the delay at most, and the freshest of them sets the clock, so a
	/// joiner runs at the delay from its first output. The output then keeps the source's
	/// pace and muxes every track in `(DTS, PID)` order whatever the arrival skew between
	/// them. A frame that arrives after its deadline is dropped, and a video track that
	/// dropped one resumes at its next keyframe. A stalled group is skipped once it falls
	/// half the delay behind the newest content (see [`Consumer`](crate::container::Consumer)),
	/// so the group after it still arrives with the other half to spare.
	///
	/// It is also how far ahead of its decode time a frame may go out, as early as the
	/// receiver's buffers for its PID admit, so a heavy passage rides the slots before it
	/// and the output trails the source by twice the delay. With a multiplex rate, a frame
	/// that cannot arrive by its decode time fails the export. Defaults to
	/// [`Duration::ZERO`], which holds nothing and sends each frame by its decode time.
	pub fn with_delay(mut self, delay: Duration) -> Self {
		self.delay = delay;
		self.jitter = jitter::Buffer::new(delay);
		#[cfg(test)]
		if self.replay {
			self.jitter = jitter::Buffer::new(delay).replay();
		}
		self.schedule = Schedule::new(delay);
		self.schedule.set_rate(self.mux_rate);
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
		for si in self.si.values_mut() {
			si.poll(waiter);
		}

		// 2. Read every frame the sources have: into the jitter buffer, or before the
		// program tables are built, one per track.
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
			// Hand the held frames to the jitter buffer in arrival order, so the
			// earliest anchors its clock, then read on.
			let mut held: Vec<(web_async::time::Instant, String)> = self
				.tracks
				.iter()
				.filter_map(|(name, t)| t.pending.as_ref().map(|p| (p.arrived, name.clone())))
				.collect();
			held.sort();
			for (_, name) in held {
				let track = self.tracks.get_mut(&name).unwrap();
				let pending = track.pending.take().unwrap();
				track.queue(&name, pending, &mut self.jitter)?;
			}
			self.fill(waiter)?;
		}

		// Once every source has ended, the tail goes out late if it must: the end of the
		// stream is not a missed deadline.
		if !self.tracks.is_empty() && self.tracks.values().all(|track| track.finished) {
			self.schedule.end();
		}

		// 4. Mux each frame the jitter buffer lets go (the first carries the buffered
		// PAT/PMT), then lay out every grid slot whose time has come ([`Self::lay_due`]).
		loop {
			if let Some(out) = self.pop() {
				return Poll::Ready(Ok(Some(out)));
			}
			let ready = match self.held.take() {
				Some(ready) => ready,
				None => match self.jitter.poll_next(waiter) {
					Poll::Ready(ready) => ready,
					Poll::Pending => break,
				},
			};
			if ready.generation > self.generation {
				if !self.schedule.is_empty() {
					// A boundary ends valid media rather than reneging it.
					// Return that tail under the old generation before adopting the new one.
					self.lay(None)?;
					self.held = Some(ready);
					continue;
				}
				self.rewind();
				self.generation = ready.generation;
			}
			debug_assert!(
				ready.generation >= self.generation,
				"the jitter buffer released an older generation after a newer one"
			);
			let name = self
				.tracks
				.iter()
				.find(|(_, t)| t.pid == ready.track)
				.map(|(name, _)| name.clone())
				.context("frame for an unknown PID")?;
			self.last_timestamp = Some(ready.item.frame.timestamp);
			let decode = ready.item.decode.as_nanos();
			self.mux(&name, ready.item)?;
			if self.delay.is_zero() {
				// Without a clock to lay slots on, a frame settles every slot before its own: a
				// later frame decodes no earlier, so it can be due no earlier than this, less the
				// longest drain of any track.
				let drain = self.max_drain();
				self.lay(Some(schedule::slot(decode.saturating_sub(drain.as_nanos()))))?;
			}
		}
		self.lay_due(waiter)?;
		if let Some(out) = self.pop() {
			return Poll::Ready(Ok(Some(out)));
		}

		// 5. Once every track has drained, nothing more can ride the slots still open, so
		// they go out: at their times on the clock, or at once without one. That's
		// independent of the catalog: a retained track finishes while the broadcast stays
		// live, and holding its tail until the catalog closed would strand it indefinitely.
		let drained = !self.tracks.is_empty()
			&& self.tracks.values().all(|t| t.finished)
			&& self.jitter.is_empty()
			&& self.held.is_none();
		if drained {
			if self.slot_due().is_none() {
				self.lay(None)?;
			}
			if let Some(out) = self.pop() {
				return Poll::Ready(Ok(Some(out)));
			}
			if !self.schedule.is_empty() {
				return Poll::Pending;
			}
			// SI emission rides media frames, so a snapshot that arrived behind the
			// last one gets one trailing flush before the stream ends.
			if self.catalog.is_none() && !self.si_flushed {
				self.si_flushed = true;
				if let Some(out) = self.write_si_tail()? {
					return Poll::Ready(Ok(Some(out)));
				}
			}
		}

		// End of stream once the catalog is closed too: nothing more can appear.
		if self.catalog.is_none() && (drained || self.tracks.is_empty()) {
			return Poll::Ready(Ok(None));
		}

		Poll::Pending
	}

	/// Return the next queued frame, committing what it carries to the output boundary.
	fn pop(&mut self) -> Option<Frame> {
		let (out, tally) = self.queue.pop_front()?;
		self.emitted_epoch = self.epoch;
		let (pcr, discontinuity) = tally.pcr;
		if discontinuity {
			self.liveness.discontinuity();
		}
		self.liveness.written_pcr(pcr);
		for pid in tally.units {
			self.liveness.delivered(pid, 1);
		}
		Some(out)
	}

	/// The trailing SI frame for end of stream: every entry's current sections,
	/// re-emitted once. Duplicates of already-emitted sections are harmless
	/// repetitions; `None` when there are no tables or no program was ever built.
	fn write_si_tail(&mut self) -> anyhow::Result<Option<Frame>> {
		if self.psi.is_none() {
			return Ok(None);
		}
		let pending: Vec<(u16, Vec<Bytes>)> = self
			.si
			.iter()
			.filter(|(_, si)| !si.active.is_empty())
			.map(|((pid, _), si)| (*pid, si.active.sections().cloned().collect()))
			.collect();
		if pending.is_empty() {
			return Ok(None);
		}
		let timestamp = self.last_timestamp.unwrap_or(Timestamp::ZERO);
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

	/// Read every frame the sources have ready.
	///
	/// Before the program tables are built, each track holds its first frame instead,
	/// and slices that arrive before their codec config resolves are dropped: a receiver
	/// joining mid-GOP can't use them, and parking them would stop us polling for the
	/// keyframe that carries the parameter sets. [`ExportSource`] has already transformed
	/// Annex-B avc3/hev1 into length-prefixed form and resolved the avcC/hvcC.
	fn fill(&mut self, waiter: &kio::Waiter) -> crate::Result<()> {
		let waiting_for_header = self.psi.is_none();
		let video_start = self.video_start;
		for (name, track) in self.tracks.iter_mut() {
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
						let pending = Pending {
							frame,
							restart: track.source.restarts(),
							skip: track.source.skips(),
							arrived: track.empty,
							read: web_async::time::Instant::now(),
						};
						// A new timeline must reach the reset before tune-in alignment can drop it.
						if let Some(start) = video_start
							&& !is_video && pending.restart == track.restart
							&& pending.frame.timestamp < start
						{
							continue;
						}
						if waiting_for_header {
							track.pending = Some(pending);
							break;
						}
						track.queue(name, pending, &mut self.jitter)?;
					}
					Poll::Ready(None) => {
						track.finished = true;
						break;
					}
					Poll::Pending => {
						track.empty = web_async::time::Instant::now();
						break;
					}
				}
			}
			// Nothing more can come in below the frames still held for their DTS.
			if track.finished && !waiting_for_header {
				track.release(name, &mut self.jitter, true)?;
			}
		}
		Ok(())
	}

	fn update_catalog(&mut self, mut catalog: Catalog<E>) -> anyhow::Result<()> {
		self.source.retain_valid(&mut catalog);

		// The MPEG-TS section lives in the extension. The trait only exposes
		// `mpegts_mut`, and this snapshot is owned, so clone it out (`()` yields the
		// empty default: no verbatim streams, no preserved PIDs/descriptors).
		let mpegts = catalog.ext.mpegts_mut().cloned().unwrap_or_default();
		self.program_descriptors = mpegts.program_descriptors.clone();
		self.program = mpegts.program.clone();
		// An explicit override wins even when it is refused (leaving the output
		// unpadded), so a bad flag cannot silently fall back to the catalog rate.
		let mux_rate = match self.mux_rate_override {
			Some(override_rate) => sanitize_mux_rate(override_rate),
			None => mpegts.mux_rate.and_then(|rate| {
				sanitize_mux_rate(rate).or_else(|| {
					tracing::warn!(mux_rate = rate, "ignoring invalid MPEG-TS multiplex rate in catalog");
					None
				})
			}),
		};
		self.mux_rate = mux_rate;
		self.schedule.set_rate(mux_rate);

		// Reconcile the SI subscriptions with the catalog's map. Entries may appear
		// after the PAT/PMT is built (a table acquired late): they ride standalone
		// PIDs outside the program, so unlike the elementary tracks below they are
		// not part of the latched layout.
		self.si
			.retain(|key, _| mpegts.si.get(&key.0).is_some_and(|t| t.contains_key(&key.1)));
		reject_colliding_si_pids(&mpegts, self.pmt_pid(), &[])?;
		for (pid, tables) in mpegts.si.iter() {
			for (table_id, entry) in tables.iter() {
				match self.si.get_mut(&(*pid, *table_id)) {
					// A repointed entry (same key, new track) rebuilds the subscription:
					// the old track is a restarted or replaced publisher's leftover, and
					// staying attached would repeat its stale sections forever. The last
					// snapshot carries across so emission never goes dark mid-swap.
					Some(existing) if existing.track != entry.track => {
						let mut replacement = SiTrack::new(&self.source, entry, self.delay);
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
						self.si
							.insert((*pid, *table_id), SiTrack::new(&self.source, entry, self.delay));
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
			if track.verbatim.is_some() {
				active.insert(name.clone(), ());
			}
		}

		// The program tables are written once; reject a track added afterwards.
		//
		// A track that leaves the catalog is not a layout change: it keeps its PID and is
		// read to its own end. A publisher retires a rendition as its track finishes or
		// drops, and the catalog update races that end on another stream, so the leaving
		// cannot say which it was. The track's end does: a finish ends it cleanly, and a
		// drop is the error the export reports.
		if self.psi.is_some() {
			for name in active.keys() {
				anyhow::ensure!(
					self.tracks.contains_key(name),
					"TS track layout changed after PAT/PMT was emitted: '{name}' added"
				);
			}
			if !self.stale.is_empty() {
				self.resubscribe(&catalog, &mpegts)?;
			}
			let es_pids: Vec<u16> = self.tracks.values().map(|t| t.pid).collect();
			reject_colliding_si_pids(&mpegts, self.pmt_pid(), &es_pids)?;
			// The layout is locked but the timing is not: a `jitter` or `framerate` published
			// after the tables still sizes the decode clock.
			for (name, config) in catalog.video.renditions.iter() {
				if let Some(track) = self.tracks.get_mut(name) {
					track.timing.configure(config, name);
				}
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
		let es_pids: Vec<u16> = pids.values().copied().collect();
		reject_colliding_si_pids(&mpegts, self.pmt_pid(), &es_pids)?;

		// Reuse each track's existing source (and any pending frame) by name; refresh
		// its PID, kind, and descriptors from this snapshot. Drop tracks no longer present.
		let mut old = std::mem::take(&mut self.tracks);
		for (name, config) in catalog.video.renditions.iter() {
			let kind = video_kind(config, name)?;
			let mut track = match old.remove(name) {
				Some(track) => track,
				None => match ExportSource::for_video(&self.source, name, config, self.budget())?
					.map(|s| at_edge(s, self.replay))
				{
					Some(source) => Track::new(source, kind.clone()),
					None => continue,
				},
			};
			track.kind = kind;
			track.timing.configure(config, name);
			self.refresh(name, track, pids[name], track_descriptors(&mpegts, name));
		}
		for (name, config) in catalog.audio.renditions.iter() {
			let kind = audio_kind(config, name)?;
			let mut track = match old.remove(name) {
				Some(track) => track,
				None => match ExportSource::for_audio(&self.source, name, config, self.budget())?
					.map(|s| at_edge(s, self.replay))
				{
					Some(source) => Track::new(source, kind.clone()),
					None => continue,
				},
			};
			track.buffer = Some(audio_buffer(config, &kind));
			track.kind = kind;
			self.refresh(name, track, pids[name], track_descriptors(&mpegts, name));
		}
		for (name, entry) in mpegts.tracks.iter() {
			let Some(verbatim) = &entry.verbatim else {
				continue;
			};
			let kind = Kind::Verbatim {
				stream_type: verbatim.stream_type,
				framing: verbatim.framing,
				stream_id: verbatim.stream_id,
			};
			let mut track = match old.remove(name) {
				Some(track) => track,
				None => Track::new(
					ExportSource::for_stream(&self.source, name, self.budget())?,
					kind.clone(),
				),
			};
			track.buffer = verbatim_buffer(&kind, &entry.descriptors);
			track.kind = kind;
			self.refresh(name, track, pids[name], entry.descriptors.clone());
		}
		// The clock anchors on whichever track a source sends latest against its decode time
		// (audio just in time while video runs most of a second ahead), so it waits to hear
		// from each continuous one: audio, video, and the passthrough streams the schedule
		// models (DVB AC-3, teletext). Not from sparse ones such as SCTE-35, subtitles or ID3,
		// which would hold every join for the full bound.
		let continuous = self
			.tracks
			.values()
			.filter(|track| !matches!(track.kind, Kind::Verbatim { .. }) || track.buffer.is_some());
		self.jitter.expect(continuous.map(|track| track.pid));
		Ok(())
	}

	/// Keep `track` under `name` with this snapshot's PID and descriptors.
	fn refresh(&mut self, name: &str, mut track: Track, pid: u16, descriptors: Vec<catalog::Descriptor>) {
		track.pid = pid;
		track.descriptors = descriptors;
		self.tracks.insert(name.to_string(), track);
	}

	/// Point each stale track this snapshot lists at the returned broadcast.
	///
	/// The PIDs and PMT stay as announced. A track the returned catalog does not list stays
	/// finished, silent on its PID.
	fn resubscribe(&mut self, catalog: &Catalog<E>, mpegts: &catalog::Mpegts) -> anyhow::Result<()> {
		let budget = self.budget();
		for (name, track) in self.tracks.iter_mut() {
			if !self.stale.contains(name) {
				continue;
			}
			let source = if let Some(config) = catalog.video.renditions.get(name) {
				ExportSource::for_video(&self.source, name, config, budget)?.map(|s| at_edge(s, self.replay))
			} else if let Some(config) = catalog.audio.renditions.get(name) {
				ExportSource::for_audio(&self.source, name, config, budget)?.map(|s| at_edge(s, self.replay))
			} else if mpegts.tracks.get(name).is_some_and(|t| t.verbatim.is_some()) {
				Some(ExportSource::for_stream(&self.source, name, budget)?)
			} else {
				None
			};
			let Some(source) = source else {
				continue;
			};
			self.stale.remove(name);
			track.source = source;
			track.finished = false;
			track.empty = web_async::time::Instant::now();
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
			dropped: self.jitter.dropped(),
			drift: self.jitter.drift().map(|drift| drift * 1e6),
			out_of_tolerance: self.jitter.out_of_tolerance(),
			..Default::default()
		};
		if self.psi.is_none() {
			return stats;
		}
		for track in self.tracks.values() {
			let (units, quiet) = self.liveness.stream(track.pid);
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

	/// When the next queued frame is due, or the next grid slot while media is queued or
	/// every source has finished and the tail runs out.
	#[cfg(test)]
	pub(super) fn next_due(&self) -> Option<web_async::time::Instant> {
		let finished = !self.tracks.is_empty() && self.tracks.values().all(|track| track.finished);
		let slot = self
			.slot_due()
			.filter(|_| self.schedule.queued() || finished)
			.map(|(_, at)| at);
		self.jitter.next_deadline().into_iter().chain(slot).min()
	}

	/// The discontinuity counter of the most recently returned output frame.
	/// Compare across reads and re-anchor pacing when it changes. Renditions have
	/// independent source counters; this counter describes the emitted program.
	pub fn discontinuity(&self) -> u64 {
		self.emitted_epoch
	}

	/// Carry on with the broadcast that replaced the one this export was reading.
	///
	/// Call it once [`Source::returned`](crate::Source::returned) has resolved, after
	/// [`next`](Self::next) returned `None` or an error. Whatever the ended broadcast left
	/// unwritten is dropped and the program clock restarts, so the next output flags the
	/// break and re-emits the PAT/PMT. The returned catalog resubscribes each track as it
	/// lists it, under the program already announced; one that adds a track fails as a
	/// layout change.
	pub async fn resume(&mut self) -> Result<(), crate::Error> {
		self.catalog = Some(self.source.catalog::<E>(self.catalog_format).await?);
		self.si_flushed = false;
		// A name no entry carries, so the returned catalog repoints every SI entry.
		for si in self.si.values_mut() {
			si.track.clear();
		}
		if self.psi.is_none() {
			// Nothing announced yet: the returned catalog builds the program afresh.
			self.tracks.clear();
		}
		// Whatever the ended broadcast left is done, including a track whose error ended
		// the export: polled again, it would end this one too.
		for (name, track) in self.tracks.iter_mut() {
			track.finished = true;
			track.pending = None;
			track.restart = 0;
			track.clock = DecodeClock::default();
			self.stale.insert(name.clone());
		}
		// The replacement's clock starts afresh at its own first frame.
		self.jitter.clear();
		self.held = None;
		self.generation = 0;
		self.rewind();
		Ok(())
	}

	/// Discard what has not gone out and restart the program clock. Every rendition
	/// joins the new generation: no track is fenced across a declared marker, since a
	/// latency skip on one track says nothing about another's timeline. The continuity
	/// counters run on, since they number only what went out.
	fn rewind(&mut self) {
		self.epoch += 1;
		self.schedule.clear();
		self.queue.clear();
		self.last_psi = None;
		for si in self.si.values_mut() {
			si.last_emit = None;
		}
		self.video_start = None;
		self.pcr_discontinuity = true;
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
						Kind::Opus { channel_config_code } => opus_descriptors(*channel_config_code),
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

		self.schedule.set_buffer(0, Buffer::SYSTEM);
		self.schedule.set_buffer(pmt_pid, Buffer::SYSTEM);
		for track in self.tracks.values() {
			self.liveness.register(track.pid);
		}
		self.psi = Some(Psi {
			pat,
			pmt,
			pcr_pid,
			pmt_pid,
		});
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
		} = queued;
		let is_video = matches!(self.tracks.get(name).context("missing track")?.kind, Kind::Video(_));
		// Refresh PSI at keyframes or after the interval lapses.
		let psi = (is_video && frame.keyframe) || due(frame.timestamp, self.last_psi, PSI_INTERVAL);
		if psi {
			for track in self.tracks.values_mut() {
				if let Kind::Aac { repeat, .. } = &mut track.kind {
					*repeat = true;
				}
			}
		}
		let track = self.tracks.get_mut(name).context("missing track")?;
		let pid = track.pid;
		let kind = track.kind.clone();
		if let Kind::Aac { repeat, .. } = &mut track.kind {
			*repeat = false;
		}
		let keyframe = frame.keyframe;

		// Build the elementary-stream payload for this frame. Video needs the
		// resolved avcC/hvcC to rewrite length-prefixed NALs as Annex-B. Section-framed
		// verbatim streams carry no PES payload; the section is written separately below.
		let es_payload = match &kind {
			Kind::Video(stream_type) => Some(video_es_payload(*stream_type, description.as_ref(), &frame)?),
			Kind::Aac { config, repeat } => {
				let pce = if *repeat {
					config.program_config.as_deref().unwrap_or_default()
				} else {
					&[]
				};
				let raw_len = pce.len() + frame.payload.len();
				let header =
					adts::write_header(config.object_type, config.sample_rate, config.channel_config, raw_len)?;
				let mut framed = Vec::with_capacity(header.len() + raw_len);
				framed.extend_from_slice(&header);
				framed.extend_from_slice(pce);
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

		let mut out = Vec::with_capacity(TsPacket::SIZE);

		if psi {
			let psi = self.psi.as_ref().context("PSI not built")?;
			let pmt_pid = psi.pmt_pid;
			let pat = TsPayload::Pat(psi.pat.clone());
			let pmt = TsPayload::Pmt(psi.pmt.clone());
			self.write_packet(&mut out, Pid::PAT, None, pat)?;
			self.write_packet(&mut out, pmt_pid, None, pmt)?;
			self.last_psi = Some(frame.timestamp);
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
			self.schedule.set_buffer(pid, Buffer::SYSTEM);
			for section in &sections {
				self.write_section(&mut out, pid, section)?;
			}
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
				// A passed-through PES of several AC-3 sync frames goes out a frame a PES, each
				// due at its own decode time: a receiver's buffer holds and decodes each on its
				// own, and the whole PES would overflow it.
				let track = self.tracks.get(name).context("missing track")?;
				let frames = track
					.carries_ac3()
					.then(|| ac3_frames(frame.timestamp, &es_payload))
					.flatten()
					.unwrap_or_else(|| vec![(frame.timestamp, es_payload.as_slice())]);
				if frames.is_empty() {
					// Nothing left of the PES but the tables muxed ahead of it.
					return match out.is_empty() {
						true => Ok(()),
						false => self.push_unit(name, pid, decode, out, false),
					};
				}
				let mut at = decode;
				for (k, (timestamp, payload)) in frames.into_iter().enumerate() {
					if k > 0 {
						let unit = std::mem::take(&mut out);
						self.push_unit(name, pid, at, unit, keyframe && k == 1)?;
						at = timestamp;
					}
					let unit = PesUnit {
						pid,
						is_video,
						keyframe: frame.keyframe && k == 0,
						timestamp,
						dts: dts.filter(|_| k == 0),
						stream_id,
					};
					self.write_pes(&mut out, &unit, payload)?;
				}
				return self.push_unit(name, pid, at, out, keyframe && at == decode);
			}
		}
		self.push_unit(name, pid, decode, out, keyframe)
	}

	/// Queue one unit of `name`'s packets on `pid`, decoding at `decode`, on the [`Schedule`].
	fn push_unit(
		&mut self,
		name: &str,
		pid: u16,
		decode: Timestamp,
		out: Vec<u8>,
		keyframe: bool,
	) -> anyhow::Result<()> {
		let track = self.tracks.get(name).context("missing track")?;
		if let Some(buffer) = track.buffer() {
			self.schedule.set_buffer(pid, buffer);
		}
		let by = decode.as_nanos().saturating_sub(track.drain().as_nanos());
		self.schedule.push(pid, by, out, keyframe);
		Ok(())
	}

	/// The longest any track's receiver takes to pass one of its packets on.
	fn max_drain(&self) -> Duration {
		self.tracks.values().map(Track::drain).max().unwrap_or_default()
	}

	/// When the next grid slot is due on the jitter buffer's clock: once every frame that could
	/// ride it has gone out of the jitter buffer, so its contents depend on the frames alone,
	/// never on when they arrived, and the output keeps the clock's pace while media is queued
	/// or still to decode. `None` without a delay, where frames settle the slots instead.
	fn slot_due(&self) -> Option<(u128, web_async::time::Instant)> {
		if self.delay.is_zero() || self.psi.is_none() || self.schedule.is_empty() {
			return None;
		}
		let (_, known) = self.schedule.upcoming()?;
		let settled = known * PCR_INTERVAL.as_nanos() + self.max_drain().as_nanos();
		let settled = Timestamp::from_micros(u64::try_from(settled / 1_000).ok()?).ok()?;
		Some((known, self.jitter.at(settled)?))
	}

	/// How far a stalled group may fall behind the newest content before each source skips it:
	/// half the delay. A skip comes once the next group's start is that far behind, and its
	/// frames are only read then, so with the whole delay they would arrive at their deadline.
	fn budget(&self) -> Duration {
		self.delay / 2
	}

	/// Lay out every grid slot whose time has come, and wake when the next one is due.
	fn lay_due(&mut self, waiter: &kio::Waiter) -> anyhow::Result<()> {
		while let Some((known, at)) = self.slot_due() {
			if web_async::time::Instant::now() < at {
				let timer = self
					.slot_timer
					.get_or_insert_with(|| Box::pin(web_async::time::sleep_until(at)));
				if timer.deadline() != at {
					timer.as_mut().reset(at);
				}
				if waiter.poll_future(timer.as_mut()).is_pending() {
					return Ok(());
				}
			}
			self.lay(Some(known))?;
		}
		self.slot_timer = None;
		Ok(())
	}

	/// Lay out every grid slot the schedule has settled, one [`Frame`] each: `known` is
	/// the first slot a frame still to be muxed could be due in, or `None` when nothing
	/// more is coming.
	///
	/// Each frame opens with its slot's clock packet. At a multiplex rate its value is the
	/// time of its own byte position at the rate, so a consumer holding only the byte stream
	/// (which is every MPEG-TS tool) recovers the same clock the values assert. Each frame is
	/// stamped at its slot boundary, so a pacing caller releases the clock at the instant it
	/// asserts.
	///
	/// The PES units cannot carry the clock themselves: a PCR sampled from the DTS would
	/// step with the frame cadence, and no downstream CBR stage can repair that, because a
	/// groomer can only place the clock samples it receives. So the PCR follows its own
	/// grid instead: slots on the media timeline, shared by every exporter of the
	/// broadcast, like [`due`].
	fn lay(&mut self, known: Option<u128>) -> anyhow::Result<()> {
		let Some(psi) = self.psi.as_ref() else {
			return Ok(());
		};
		let (pcr_pid, pmt_pid) = (psi.pcr_pid, psi.pmt_pid);
		while let Some(slot) = self.schedule.next(known)? {
			let mut payload = pcr_packet(pcr_pid, slot.pcr)?;
			let discontinuity = std::mem::take(&mut self.pcr_discontinuity);
			if discontinuity {
				payload[5] |= 0x80;
			}
			payload.extend_from_slice(&slot.layout(pmt_pid, &NULL_PACKET));
			self.number(&mut payload);
			let frame = Frame {
				timestamp: slot_stamp(slot.index)?,
				duration: None,
				payload: Bytes::from(payload),
				keyframe: slot.keyframe,
			};
			let tally = Tally {
				pcr: (slot.pcr, discontinuity),
				units: slot.units,
			};
			self.queue.push_back((frame, tally));
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
			let next = self.counters.entry(pid).or_default();
			let cc = match packet[3] & 0x10 != 0 {
				true => std::mem::replace(next, (*next + 1) & ContinuityCounter::MAX),
				false => next.wrapping_sub(1) & ContinuityCounter::MAX,
			};
			packet[3] = (packet[3] & 0xf0) | cc;
		}
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

	/// Serialize one TS packet into `out`. Its continuity counter is numbered when it goes
	/// out ([`Self::number`]), since the schedule decides that order.
	fn write_packet(
		&mut self,
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
	let (base, extension) = ((pcr / 300) & TS_TIMESTAMP_MASK, pcr % 300);
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
	// program_clock_reference_base (33 bits), 6 reserved '1' bits, and the 9-bit extension.
	p.push((base >> 25) as u8);
	p.push((base >> 17) as u8);
	p.push((base >> 9) as u8);
	p.push((base >> 1) as u8);
	p.push(((base as u8) << 7) | 0x7e | (extension >> 8) as u8);
	p.push(extension as u8);
	p.resize(TsPacket::SIZE, 0xff);
	Ok(p)
}

/// Optional PES header region carrying PTS only: 2 flag bytes + 1 length byte + 5 PTS bytes.
const PES_OPTIONAL_LEN: usize = 3 + 5;
/// Extra bytes when the optional region also carries a DTS (5 DTS bytes).
const PES_DTS_LEN: usize = 5;
/// Largest reorder delay or lookahead an SPS or observed reordering can raise a video decode
/// clock to (2 s, in 90 kHz ticks): far past any real reorder depth, so a corrupt SPS or a
/// timestamp stepping back within a timeline cannot hold frames or drag the DTS back without
/// bound.
const MAX_REORDER: u64 = 2 * 90_000;

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
			cpb: 1_200 * 175,
			factor: 1_200,
		}
	})
}

/// An audio stream's buffers in a receiver (H.222.0 2.4.2.3): by channel count for ADTS
/// AAC (and Opus, which borrows them), 2 Mb/s and the codec's main buffer for the rest.
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
	Buffer { rate, size: Some(size) }
}

/// The DVB AC-3 descriptor's tag (ETSI EN 300 468 6.2.1), marking AC-3 as private data.
const AC3_DESCRIPTOR: u8 = 0x6a;

/// The DVB teletext descriptor's tag (ETSI EN 300 468 6.2.43).
const TELETEXT_DESCRIPTOR: u8 = 0x56;

/// A verbatim stream's buffers, for the kinds its descriptors identify: AC-3 as DVB private
/// data (ATSC A/52 Annex A 5.4), and teletext, whose transport buffer drains at 6.75 Mb/s
/// (ETSI EN 300 472 5). Any other stream has none the schedule knows.
fn verbatim_buffer(kind: &Kind, descriptors: &[catalog::Descriptor]) -> Option<Buffer> {
	let Kind::Verbatim {
		stream_type: 0x06,
		framing: catalog::Framing::Pes,
		..
	} = kind
	else {
		return None;
	};
	descriptors.iter().find_map(|descriptor| match descriptor.tag {
		AC3_DESCRIPTOR => Some(Buffer {
			rate: 2_000_000,
			size: Some(5_696),
		}),
		TELETEXT_DESCRIPTOR => Some(Buffer {
			rate: 6_750_000,
			size: None,
		}),
		_ => None,
	})
}

/// The AC-3 sync frames `payload` holds, each with its presentation time counted on from
/// `start`, or `None` if it does not parse as AC-3. A trailing frame shorter than its header
/// says is dropped, as a source cut off mid-frame leaves one: no decoder can use it.
fn ac3_frames(start: Timestamp, payload: &[u8]) -> Option<Vec<(Timestamp, &[u8])>> {
	let mut frames = Vec::new();
	let (mut rest, mut samples) = (payload, 0u64);
	while !rest.is_empty() {
		let header = (crate::codec::ac3::DESCRIPTOR.parse)(rest).ok()?;
		let Some((frame, tail)) = rest.split_at_checked(header.len) else {
			tracing::warn!(
				missing = header.len - rest.len(),
				"dropped an AC-3 sync frame cut short of its length"
			);
			return Some(frames);
		};
		let offset = Timestamp::from_micros(samples * 1_000_000 / u64::from(header.sample_rate.max(1))).ok()?;
		frames.push((start.checked_add(offset).ok()?, frame));
		samples += header.samples;
		rest = tail;
	}
	Some(frames)
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
fn opus_descriptors(channel_config_code: u8) -> Vec<Descriptor> {
	vec![
		Descriptor {
			tag: 0x05,
			data: b"Opus".to_vec(),
		},
		Descriptor {
			tag: 0x7f,
			// extension_descriptor_tag 0x80, then the plain channel_config_code.
			data: vec![0x80, channel_config_code],
		},
	]
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

/// A video rendition's decode clock: a decode timestamp (DTS) for each frame from its PTS.
///
/// [`Frame`] carries only a presentation timestamp and frames reach the muxer in decode order,
/// so a B-frame stream arrives with non-monotonic PTS and no decode time. An encoder decodes
/// the n-th frame in decode order at the n-th presentation time, a fixed reorder delay early,
/// so that is the DTS authored here: with decode-order PTS 0, 120, 40, 80 and a delay of one
/// 40 ms frame, the frames decode at -40, 0, 40, 80. It keeps the encoder's own spacing, a
/// frame's or a field's, through any switch between frame and field coding, and the delay is
/// a time, the least that keeps every DTS at or before its PTS (or the SPS's declared depth,
/// when that is more). On a source muxed that way this is its DTS, frame for frame.
///
/// A frame's slot is settled once no frame still to come can be presented before it. A later
/// frame decodes after this one, and is presented at most the catalog `jitter` after it
/// decodes, so the slot is settled once the highest PTS read is that far past it. Frames wait
/// here until then, as long as the reordering spans and well inside the jitter buffer's delay.
/// Without a `jitter`, the SPS's depth bounds it, and so does how far a frame was presented
/// below the highest PTS read before it. A frame that still comes in below a settled slot
/// (reordering deeper than all of those) is nudged one tick past the last DTS, so the clock
/// stays strictly increasing.
///
/// Ticks are continuous (unwrapped) 90 kHz, so the clock never wraps mid-stream; the 33-bit
/// wire wrap happens once at emission.
struct DecodeClock<T> {
	/// Frames read but not yet given a DTS, in decode order, with their PTS.
	held: VecDeque<(T, u64)>,
	/// The held frames' PTS, ascending: the slots they decode in, in turn.
	slots: Vec<u64>,
	/// The highest PTS read.
	high: Option<u64>,
	/// The furthest a frame was presented below the highest PTS read before it, plus a tick.
	reach: u64,
	/// The reorder delay in effect. It never shrinks: that would step the clock back.
	delay: u64,
	/// The last DTS handed out.
	last: Option<u64>,
}

impl<T> Default for DecodeClock<T> {
	fn default() -> Self {
		Self {
			held: VecDeque::new(),
			slots: Vec::new(),
			high: None,
			reach: 0,
			delay: 0,
			last: None,
		}
	}
}

impl<T> DecodeClock<T> {
	/// Hold the next frame in decode order, presented at `pts`.
	fn push(&mut self, item: T, pts: u64) {
		if let Some(high) = self.high
			&& pts < high
		{
			self.reach = self.reach.max(high - pts + 1).min(MAX_REORDER);
		}
		self.high = Some(self.high.map_or(pts, |high| high.max(pts)));
		let at = self.slots.partition_point(|&slot| slot <= pts);
		self.slots.insert(at, pts);
		self.held.push_back((item, pts));
	}

	/// The next frame and its DTS, once its slot is settled `lookahead` past (the catalog
	/// `jitter`), or at once when `flush`. `floor` is the least reorder delay, in ticks.
	fn pop(&mut self, lookahead: u64, floor: u64, flush: bool) -> Option<(T, u64)> {
		let high = self.high?;
		let lookahead = lookahead.max(self.reach);
		let settled = |slot: u64| flush || high >= slot.saturating_add(lookahead);
		if !settled(*self.slots.first()?) {
			return None;
		}
		// Every settled frame bounds the delay, so one decoding later never steps it back.
		let mut delay = self.delay.max(floor);
		for (&slot, (_, pts)) in self.slots.iter().zip(&self.held) {
			if !settled(slot) {
				break;
			}
			delay = delay.max(slot.saturating_sub(*pts));
		}
		self.delay = delay.min(MAX_REORDER);

		let slot = self.slots.remove(0);
		let (item, _) = self.held.pop_front()?;
		let dts = slot.saturating_sub(self.delay);
		let dts = match self.last {
			Some(last) if dts <= last => last + 1,
			_ => dts,
		};
		self.last = Some(dts);
		Some((item, dts))
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

	use super::{Buffer, DecodeClock, Kind, PSI_INTERVAL, due, is_complete_section, si_due, slot, verbatim_buffer};

	fn ms(value: u64) -> Timestamp {
		Timestamp::from_millis(value).unwrap()
	}

	/// Teletext passed through as DVB private data takes its transport buffer's rate from
	/// EN 300 472, so the schedule spreads it no faster than a receiver passes it on; private
	/// data nothing identifies has no buffers.
	#[test]
	fn teletext_drains_at_its_own_rate() {
		let kind = Kind::Verbatim {
			stream_type: 0x06,
			framing: super::catalog::Framing::Pes,
			stream_id: None,
		};
		let descriptor = |tag| super::catalog::Descriptor {
			tag,
			data: bytes::Bytes::from_static(b"eng\x09\x00"),
		};
		assert_eq!(
			verbatim_buffer(&kind, &[descriptor(0x56)]),
			Some(Buffer {
				rate: 6_750_000,
				size: None,
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
			clock.push(i, p);
			while let Some((i, d)) = clock.pop(jitter, floor, false) {
				dts[i] = d;
			}
		}
		while let Some((i, d)) = clock.pop(jitter, floor, true) {
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
}
