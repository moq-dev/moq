//! The counters [`Import::stats`](super::Import::stats) and
//! [`Export::stats`](super::Export::stats) return, and reporting on them.
//!
//! [`Report`] carries them in a media report, as the `mpegts` and `release` sections beside
//! `transport` and `renditions`. Counters are cumulative and omitted at zero.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

fn is_zero(value: &u64) -> bool {
	*value == 0
}

/// The sections an MPEG-TS import or export adds to its media report: the `E` of
/// [`hang::stats::Snapshot`] or [`hang::echo::Snapshot`], flattened beside `transport` and
/// `renditions`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct Report {
	/// What each elementary stream delivered and the transport stream's errors: an import's
	/// [`Snapshot`], or the rows of an [`Export`]. A passthrough has none.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub mpegts: Option<Snapshot>,
	/// How an export's release clock keeps up with the source. An import has none.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub release: Option<Release>,
}

/// What each demuxed elementary stream delivered, and the audio frame sync it lost or could
/// not verify, keyed by elementary stream PID; the PSI sections the stream as a whole lost to
/// corruption; and its TR 101 290 errors.
///
/// Take one with [`Import::stats`](super::Import::stats). Every count is cumulative for the
/// life of the importer, so what an operator alarms on is the rate: a feed that resyncs once an
/// hour is healthy, one that resyncs every second is losing audio, and one whose access units
/// stop advancing while the mux keeps flowing has lost that stream.
///
/// The fields named for ETSI TR 101 290 (V1.4.1) checks count them at that standard's fixed
/// limits. They grade the stream as the importer received it, not the wire a receiver sees
/// downstream. TR 101 290's `PID_error` is covered, more strictly, by each row's `units` and
/// `quiet`: a dead video path behind a live mux still sends PCR-only packets on its PID, which
/// a packet count reads as live. `PCR_accuracy_error` is not measured.
///
/// [`Export`] carries the same rows for the streams an exporter writes, so one schema reads
/// both edges.
#[serde_with::serde_as]
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
#[non_exhaustive]
pub struct Snapshot {
	/// One row per PID, ordered by PID: each elementary stream the PMT lists, carried or
	/// refused, and each PID no table claims that the importer dropped packets on. A dropped
	/// PID's row names no track, is [`Class::Data`], and counts its packets in
	/// [`Stream::refused`]. A PID keeps one row for its life: a PMT that claims it, releases it,
	/// or claims it again moves the row with its counters, and while dropped it names no track.
	/// Packets before every listed PMT has arrived are acquisition and not counted; so are null
	/// packets. An import of one program from a multiplex that lists
	/// others counts no PID no table claims, since another program's PMT may. An SI PID has a
	/// `.si` row only once it counts [`Stream::unplaced`] sections.
	// Keys read from strings: a flattened section loses JSON's integer-key parsing.
	#[serde_as(as = "BTreeMap<serde_with::DisplayFromStr, _>")]
	#[serde(skip_serializing_if = "BTreeMap::is_empty")]
	pub streams: BTreeMap<u16, Stream>,
	/// PAT and PMT sections dropped because their CRC-32 did not match. Each one left the
	/// table already in force standing, or, before any, delayed the program's start to the
	/// next good repetition. Counted for the stream rather than per elementary stream, since
	/// the PSI PIDs have no row of their own. TR 101 290 2.2 `CRC_error`, on the PAT and PMT
	/// only: the reduced-SI set of its table 5.1b. Captured SI is not CRC-checked.
	#[serde(skip_serializing_if = "is_zero")]
	pub crc_error: u64,
	/// 1.1 `TS_sync_loss`: sync lost after two consecutive corrupt sync bytes on the packet
	/// grid, and acquired again after five correct ones (ISO/IEC 13818-1 G.1).
	#[serde(skip_serializing_if = "is_zero")]
	pub ts_sync_loss: u64,
	/// 1.2 `Sync_byte_error`: a byte other than 0x47 where the grid expects a sync byte, while
	/// in sync.
	#[serde(skip_serializing_if = "is_zero")]
	pub sync_byte_error: u64,
	/// 1.3 `PAT_error_2`: each 0.5 s of program clock without a PAT section on PID 0, and each
	/// packet on PID 0 that is scrambled or starts a section other than a PAT.
	#[serde(skip_serializing_if = "is_zero")]
	pub pat_error: u64,
	/// 1.4 `Continuity_count_error`, on every PID but the null PID: a counter gap, or a third
	/// copy of a packet. A gap declared by `discontinuity_indicator`, the one duplicate ISO/IEC
	/// 13818-1 2.4.3.3 permits, and a payload-less packet repeating its counter are not errors.
	/// A loss of exactly 16 packets leaves the counter in step, and only a byte comparison
	/// tells a loss of 15 from a duplicate, so the count is a floor.
	#[serde(skip_serializing_if = "is_zero")]
	pub continuity_count_error: u64,
	/// 1.5 `PMT_error_2`: each 0.5 s of program clock without a PMT section on a PMT PID the
	/// PAT names for the imported program, and each scrambled packet on one.
	#[serde(skip_serializing_if = "is_zero")]
	pub pmt_error: u64,
	/// 2.1 `Transport_error`: packets with `transport_error_indicator` set, on any PID. Such a
	/// packet counts nothing else.
	#[serde(skip_serializing_if = "is_zero")]
	pub transport_error: u64,
	/// 2.3a `PCR_repetition_error`: consecutive PCR values on the PCR PID more than 100 ms
	/// apart. Graded on the values, not on arrival: that speaks for the encoder's insertion,
	/// which is what ingest can, while the network in front of the importer re-times arrival.
	/// The interval across a `discontinuity_indicator` is not graded.
	#[serde(skip_serializing_if = "is_zero")]
	pub pcr_repetition_error: u64,
	/// 2.3b `PCR_discontinuity_indicator_error`: consecutive PCR values outside 0 to 100 ms
	/// apart, backwards included, without `discontinuity_indicator`. On values a run of missing
	/// PCRs and an unsignalled jump are the same difference, so a forward step over 100 ms
	/// counts here and as a repetition error both.
	#[serde(skip_serializing_if = "is_zero")]
	pub pcr_discontinuity_indicator_error: u64,
	/// 2.5 `PTS_error`: each 700 ms of program clock without a PTS on an elementary stream
	/// that has carried one. Verbatim streams are not graded: SCTE-35 and subtitles have no
	/// cadence to hold.
	#[serde(skip_serializing_if = "is_zero")]
	pub pts_error: u64,
}

impl Snapshot {
	/// Whether no elementary stream has been registered yet and nothing has been lost.
	pub fn is_empty(&self) -> bool {
		self.streams.is_empty() && self.crc_error == 0 && self.health() == [0; 9]
	}
}

/// What one elementary stream delivered, lost, or could not verify. See [`Snapshot`].
#[serde_with::serde_as]
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
#[non_exhaustive]
pub struct Stream {
	/// The current or most recent MoQ track suffix for this PID (`.avc3`, `.mp2`, `.ts`, ...),
	/// or empty for a PID that publishes no track: one dropped, or MPEG-1/2 video refused and
	/// read only for its clock. At export, the suffix an import of the output would give the PID.
	#[serde(skip_serializing_if = "String::is_empty")]
	pub track: String,
	/// How this PID was classified. See [`Class`].
	pub class: Class,
	/// Access units the stream delivered: frames published for decoded media, whole PES or
	/// sections carried verbatim, and PES read on MPEG-1/2 video. At export, the PES or
	/// sections written for each frame.
	#[serde(skip_serializing_if = "is_zero")]
	pub units: u64,
	/// Transport time since the stream last delivered an access unit, or since the PMT
	/// declared it, measured on the program clock (PCR): the one read at import, the one
	/// written at export. `None` until the first PCR. Whole milliseconds in a report.
	///
	/// No threshold applies: a sparse stream such as SCTE-35 is legitimately quiet for
	/// seconds, so how long is too long is for whoever alarms to decide.
	#[serde_as(as = "Option<serde_with::DurationMilliSeconds<u64>>")]
	#[serde(skip_serializing_if = "Option::is_none")]
	pub quiet: Option<std::time::Duration>,
	/// Completed resyncs: the stream lost frame sync and locked onto a confirmed frame
	/// again. Each one is a gap in the audio.
	#[serde(skip_serializing_if = "is_zero")]
	pub resyncs: u64,
	/// Bytes thrown away scanning for sync, across every resync and any scan still in
	/// progress.
	#[serde(skip_serializing_if = "is_zero")]
	pub discarded: u64,
	/// Frames published that nothing vouched for: the offset came from a scan, or the bytes
	/// were joined onto a tail carried across a PES boundary, and no successor frame could
	/// confirm it. That substitutes audio rather than leaving a gap, which is why it is
	/// counted separately from a resync.
	#[serde(skip_serializing_if = "is_zero")]
	pub unconfirmed: u64,
	/// Damaged packets, PES, or access units refused whole on this PID. Export stays zero.
	#[serde(skip_serializing_if = "is_zero")]
	pub damaged: u64,
	/// Whole PES or sections, SI included, dropped because the program clock could not place
	/// them: before the first PCR, after the last with no interval to extrapolate from, or
	/// before the broadcast clock's zero. Any PES lands before that zero while its stream is
	/// still behind a wrap the import's first PTS already crossed. Export stays zero.
	#[serde(skip_serializing_if = "is_zero")]
	pub unplaced: u64,
	/// What this PID lost to a route the import does not carry. Once for each time its stream
	/// was refused: a listed stream the import cannot carry (an ambiguous 0x86 without a CUEI
	/// registration, an Opus configuration it cannot map, any undecoded stream without the
	/// `mpegts` catalog section or in a program with no PCR to place it by), or a whole-PES
	/// stream with a clock field only the PTS and DTS of are rebased (ESCR, a
	/// `pack_header_field`, a TREF), PES-level scrambling, an unbounded non-video PES, or a
	/// header export refuses, whose track ends and leaves the catalog. Once for each
	/// TS-scrambled packet on a carried stream, which the raw lane could carry. And once for
	/// each packet on a PID no table claims ([`Snapshot::streams`]). Export stays zero.
	#[serde(skip_serializing_if = "is_zero")]
	pub refused: u64,
	/// This PID's share of [`Snapshot::continuity_count_error`].
	#[serde(skip_serializing_if = "is_zero")]
	pub continuity_count_error: u64,
	/// This PID's share of [`Snapshot::transport_error`].
	#[serde(skip_serializing_if = "is_zero")]
	pub transport_error: u64,
	/// This PID's share of [`Snapshot::pts_error`].
	#[serde(skip_serializing_if = "is_zero")]
	pub pts_error: u64,
}

impl Stream {
	pub(super) fn new(track: &str, class: Class) -> Self {
		Self {
			track: track.to_string(),
			class,
			..Default::default()
		}
	}

	/// Add a newer route's frame-sync counters while naming the route that is active now.
	/// Delivery and damage are kept per PID rather than per route, so they need no merging.
	pub(super) fn merge(&mut self, current: &Self) {
		self.track.clone_from(&current.track);
		self.class = current.class;
		self.resyncs += current.resyncs;
		self.discarded += current.discarded;
		self.unconfirmed += current.unconfirmed;
	}

	/// Set each delivery, damage and frame-sync count to `f` of it and `other`'s, keeping this
	/// row's track, class and quiet. The TR 101 290 shares are left alone: they are written
	/// from the merged checks.
	pub(super) fn combine(&mut self, other: &Self, f: impl Fn(u64, u64) -> u64) {
		for (ours, theirs) in [
			(&mut self.units, other.units),
			(&mut self.resyncs, other.resyncs),
			(&mut self.discarded, other.discarded),
			(&mut self.unconfirmed, other.unconfirmed),
			(&mut self.damaged, other.damaged),
			(&mut self.unplaced, other.unplaced),
			(&mut self.refused, other.refused),
		] {
			*ours = f(*ours, theirs);
		}
	}
}

/// Audio, video, or other data, as import or export already resolved the PID.
///
/// Not the PMT `stream_type`. `0x86` is DTS audio or, with a CUEI registration, an SCTE-35
/// section, and those take different routes. The stopped-stream log grades audio and video
/// only; a sparse data PID stays counted.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Class {
	/// Continuous audio.
	Audio = 1,
	/// Continuous video, including MPEG-1/2 video carried whole or read only for its clock.
	Video = 2,
	/// Undecoded or sparse data (SCTE-35, ID3, private sections, private PES), a PCR PID that
	/// carries no elementary stream, or a dropped PID. A quiet second is not a stall. A class a later
	/// version adds reads as data. It must stay last for `#[serde(other)]`; the explicit
	/// discriminants keep every variant's `as` value.
	#[default]
	#[serde(other)]
	Data = 0,
}

impl Class {
	/// Audio and video have a cadence. Data does not, so a quiet second is not a stall.
	pub(super) fn graded(self) -> bool {
		matches!(self, Self::Audio | Self::Video)
	}
}

/// What each elementary stream an [`Export`](super::Export) writes has delivered, keyed by
/// PID, and how the release clock is keeping up with the source.
///
/// Take one with [`Export::stats`](super::Export::stats). Its rows are an import's, but only
/// `units` and `quiet` move: the exporter builds every frame header itself, so it has no frame
/// sync to lose and it runs no TR 101 290 checks. [`Log`] still grades an audio or video row
/// whose `units` stay still.
#[derive(Clone, Debug, Default, PartialEq)]
#[non_exhaustive]
pub struct Export {
	/// One row per elementary stream PID the exporter writes, ordered by PID.
	pub streams: BTreeMap<u16, Stream>,
	/// How the release clock is keeping up with the source.
	pub release: Release,
}

/// How an export keeps up with the source: the frames it drops, trims, and never receives,
/// and how its release clock tracks the source's. What [`Export::stats`](super::Export::stats)
/// reports beside its rows, and [`Release::stats`](super::Release::stats) on its own.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
#[non_exhaustive]
pub struct Release {
	/// Frames dropped after their stream's first went out: for arriving after their deadline,
	/// for falling due before a new timeline's clock was acquired, and the frames a video track
	/// then dropped waiting for its next keyframe or leading the keyframe it resumed at, late
	/// or after a hole upstream once the output held frames ahead of the hole.
	#[serde(skip_serializing_if = "is_zero")]
	pub dropped: u64,
	/// Frames dropped before their stream's first went out, so the output starts on a whole
	/// group led by the video keyframe: what a joiner is handed older than the delay, a start
	/// group cut short upstream, the first keyframe's leading pictures, and what precedes the
	/// first keyframe or the codec config.
	/// Nothing is missing from the output for these.
	#[serde(skip_serializing_if = "is_zero")]
	pub trimmed: u64,
	/// Times a source skipped media it will never deliver: a group lost or expired upstream,
	/// or a resubscription. A declared break is not one. Each is logged with the span it
	/// skipped.
	#[serde(skip_serializing_if = "is_zero")]
	pub gaps: u64,
	/// The least time any frame arrived before its deadline over the last whole second, in
	/// milliseconds: how much more latency the path could take before frames drop. Negative
	/// when a frame missed its deadline. `None` until a second has passed, and without a delay.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub margin: Option<i64>,
	/// How far the output clock sits from the source's plus the delay, in milliseconds, as last
	/// measured: positive holds frames longer than the delay, negative shorter. The clock pulls
	/// it shut over ten minutes. `None` until there is a measurement.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub offset: Option<i64>,
	/// How fast the source's clock runs against ours, in parts per million (positive runs
	/// fast), as last measured. `None` until there is a measurement.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub drift: Option<f64>,
	/// How many measurements found the source's clock further off ours than the 30 ppm an
	/// MPEG-TS system clock may be (ISO/IEC 13818-1 2.4.2.1), which the output cannot follow.
	#[serde(skip_serializing_if = "is_zero")]
	pub out_of_tolerance: u64,
}

impl Release {
	pub(crate) fn of<K: Ord + Clone, T>(jitter: &crate::jitter::Buffer<K, T>) -> Self {
		let jitter = jitter.reporting();
		let millis = |seconds: f64| (seconds * 1e3).round() as i64;
		Self {
			dropped: jitter.dropped(),
			trimmed: jitter.trimmed(),
			gaps: 0,
			margin: jitter.margin().map(millis),
			offset: jitter.offset().map(millis),
			drift: jitter.drift().map(|drift| drift * 1e6),
			out_of_tolerance: jitter.out_of_tolerance(),
		}
	}
}

/// An export's rows with every stream-wide counter zero, so one [`Log`] reads either edge.
impl From<Export> for Snapshot {
	fn from(export: Export) -> Self {
		Self {
			streams: export.streams,
			..Default::default()
		}
	}
}

/// Logs what moved in an importer's [`Snapshot`], or an exporter's [`Export`] converted into
/// one, from one sample to the next, so every front door that runs one reports the same lines.
///
/// Feed it a snapshot every [`INTERVAL`](Self::INTERVAL) of wall time. The caller owns the
/// timer, since what counts as "now" differs between a CLI, a server, and a test.
#[derive(Default)]
pub struct Log {
	previous: Option<Snapshot>,
	/// Streams already reported quiet, so a silence is logged when it starts and not again
	/// until the stream has delivered.
	quiet: BTreeSet<u16>,
}

impl Log {
	/// How often to [`sample`](Self::sample).
	///
	/// Long enough that every continuous stream delivers many access units in between, so a
	/// count that did not move is a stream that stopped rather than one between frames.
	pub const INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);

	/// Compare a snapshot taken [`INTERVAL`](Self::INTERVAL) after the last.
	///
	/// Logs an audio or video stream whose access units did not advance since the last
	/// sample, once per silence. Sparse data (SCTE-35, ID3, other verbatim PIDs) is counted
	/// and not graded. Also logs any stream whose loss counters moved, and PSI sections
	/// dropped since.
	pub fn sample(&mut self, latest: Snapshot) {
		self.sync(&latest);
		for (pid, stream) in &latest.streams {
			let previous = self.previous.as_ref().and_then(|previous| previous.streams.get(pid));
			if previous.is_none_or(|previous| previous.units != stream.units) {
				self.quiet.remove(pid);
			} else if stream.class.graded() && self.quiet.insert(*pid) {
				tracing::info!(
					pid = *pid,
					track = stream.track,
					units = stream.units,
					quiet = ?stream.quiet,
					"elementary stream stopped delivering access units"
				);
			}
		}
		self.previous = Some(latest);
	}

	/// Compare the snapshot taken after [`Import::finish`](super::Import::finish).
	///
	/// It covers less than an interval, so only the losses are compared: the drain at end of
	/// input can publish a frame nothing vouched for.
	pub fn finish(&self, latest: &Snapshot) {
		self.sync(latest);
	}

	/// Report the streams whose damage or frame-sync counters moved, one line each, the PSI
	/// sections dropped for a bad CRC if that count moved, and the TR 101 290 counters if any
	/// moved.
	///
	/// The importer already warns on each individual resync and dropped section; this is the
	/// running total, which is what an operator turns into a rate.
	fn sync(&self, latest: &Snapshot) {
		if self.previous.as_ref().map_or(0, |previous| previous.crc_error) != latest.crc_error {
			tracing::info!(
				crc_error = latest.crc_error,
				"PAT or PMT sections dropped for a bad CRC"
			);
		}
		if self.previous.as_ref().map_or([0; 9], Snapshot::health) != latest.health() {
			tracing::info!(
				ts_sync_loss = latest.ts_sync_loss,
				sync_byte_error = latest.sync_byte_error,
				pat_error = latest.pat_error,
				continuity_count_error = latest.continuity_count_error,
				pmt_error = latest.pmt_error,
				transport_error = latest.transport_error,
				crc_error = latest.crc_error,
				pcr_repetition_error = latest.pcr_repetition_error,
				pcr_discontinuity_indicator_error = latest.pcr_discontinuity_indicator_error,
				pts_error = latest.pts_error,
				"TR 101 290 errors in the received transport stream"
			);
		}
		let lost = |stream: &Stream| (stream.resyncs, stream.discarded, stream.unconfirmed);
		for (pid, stream) in &latest.streams {
			let previous = self.previous.as_ref().and_then(|previous| previous.streams.get(pid));
			if previous.map_or(0, |previous| previous.damaged) != stream.damaged {
				tracing::info!(
					pid = *pid,
					track = stream.track,
					damaged = stream.damaged,
					"damaged TS units refused"
				);
			}
			let dropped = |stream: &Stream| (stream.unplaced, stream.refused);
			if previous.map_or((0, 0), dropped) != dropped(stream) {
				tracing::info!(
					pid = *pid,
					track = stream.track,
					unplaced = stream.unplaced,
					refused = stream.refused,
					"private PES not carried"
				);
			}
			if previous.map_or((0, 0, 0), lost) == lost(stream) {
				continue;
			}
			tracing::info!(
				pid = *pid,
				track = stream.track,
				resyncs = stream.resyncs,
				discarded = stream.discarded,
				unconfirmed = stream.unconfirmed,
				"audio frame sync lost"
			);
		}
	}
}

#[cfg(test)]
mod test {
	use super::*;

	/// A class a later version adds reads as data.
	#[test]
	fn an_unknown_class_reads_as_data() {
		let class: Class = serde_json::from_str(r#""subtitle""#).unwrap();
		assert_eq!(class, Class::Data);
	}

	/// A stream whose count stops across a sample is logged once, with its silence, and again
	/// only after it has delivered in between; a stream that keeps counting never is, and
	/// neither reports frame sync it never lost.
	#[test]
	#[tracing_test::traced_test]
	fn reports_a_stream_that_stopped() {
		const VIDEO: u16 = 0x100;
		const AUDIO: u16 = 0x101;
		let sample = |video: u64, audio: u64| {
			let mut stats = Snapshot::default();
			for (pid, track, class, units) in [
				(VIDEO, ".avc3", Class::Video, video),
				(AUDIO, ".mp2", Class::Audio, audio),
			] {
				let stream = Stream {
					track: track.to_string(),
					class,
					units,
					quiet: Some(std::time::Duration::from_millis(40)),
					..Default::default()
				};
				stats.streams.insert(pid, stream);
			}
			stats
		};

		let mut log = Log::default();
		for (video, audio) in [(10, 10), (10, 20), (10, 30), (11, 40), (11, 50)] {
			log.sample(sample(video, audio));
		}

		logs_assert(|lines: &[&str]| {
			let stopped: Vec<_> = lines
				.iter()
				.filter(|line| line.contains("stopped delivering access units"))
				.collect();
			match stopped.as_slice() {
				[first, second] if [first, second].iter().all(|line| line.contains("pid=256")) => Ok(()),
				_ => Err(format!("expected two lines for the video PID, got {stopped:?}")),
			}
		});
		assert!(!logs_contain("audio frame sync lost"), "nothing lost frame sync");
	}

	/// A CUEI-marked 0x86 section is sparse data. A second without a section is not a stall,
	/// even beside a video PID whose access units stopped.
	#[test]
	#[tracing_test::traced_test]
	fn sparse_data_pid_beside_a_stalled_video_is_not_logged() {
		const VIDEO: u16 = 0x100;
		const CUE: u16 = 0x21;
		let sample = |video: u64, cue: u64| {
			let mut stats = Snapshot::default();
			stats.streams.insert(
				VIDEO,
				Stream {
					track: ".avc3".to_string(),
					class: Class::Video,
					units: video,
					quiet: Some(std::time::Duration::from_secs(1)),
					..Default::default()
				},
			);
			stats.streams.insert(
				CUE,
				Stream {
					track: ".ts".to_string(),
					class: Class::Data,
					units: cue,
					quiet: Some(std::time::Duration::from_secs(1)),
					..Default::default()
				},
			);
			stats
		};

		let mut log = Log::default();
		log.sample(sample(4, 1));
		log.sample(sample(4, 1));
		log.sample(sample(4, 2));
		log.sample(sample(4, 2));

		logs_assert(|lines: &[&str]| {
			let stopped: Vec<_> = lines
				.iter()
				.filter(|line| line.contains("stopped delivering access units"))
				.collect();
			match stopped.as_slice() {
				[line] if line.contains("pid=256") && !line.contains("pid=33") => Ok(()),
				_ => Err(format!("expected only the stalled video PID, got {stopped:?}")),
			}
		});
	}

	/// A resync is reported on the sample that saw it, and the end of input still reports one
	/// the last partial interval found.
	#[test]
	#[tracing_test::traced_test]
	fn reports_frame_sync_lost_at_the_end() {
		let sample = |resyncs: u64, units: u64| {
			let stream = Stream {
				track: ".mp2".to_string(),
				class: Class::Audio,
				units,
				resyncs,
				..Default::default()
			};
			let mut stats = Snapshot::default();
			stats.streams.insert(0x101, stream);
			stats
		};

		let mut log = Log::default();
		log.sample(sample(0, 10));
		log.sample(sample(0, 20));
		log.finish(&sample(1, 21));

		logs_assert(|lines: &[&str]| {
			match lines
				.iter()
				.filter(|line| line.contains("audio frame sync lost"))
				.count()
			{
				1 => Ok(()),
				n => Err(format!("expected one frame-sync line, got {n}")),
			}
		});
		assert!(!logs_contain("stopped delivering"), "the stream kept counting");
	}

	/// Dropped PSI sections are reported when the count moves, with the running total, and
	/// not again while it holds.
	#[test]
	#[tracing_test::traced_test]
	fn reports_crc_errors_when_they_move() {
		let sample = |crc_error: u64| Snapshot {
			crc_error,
			..Default::default()
		};

		let mut log = Log::default();
		log.sample(sample(0));
		log.sample(sample(2));
		log.sample(sample(2));
		log.finish(&sample(3));

		logs_assert(|lines: &[&str]| {
			let dropped: Vec<_> = lines.iter().filter(|line| line.contains("bad CRC")).collect();
			match dropped.as_slice() {
				[first, second] if first.contains("crc_error=2") && second.contains("crc_error=3") => Ok(()),
				_ => Err(format!("expected one line per move, got {dropped:?}")),
			}
		});
		assert!(!logs_contain("TR 101 290"), "a CRC error is reported on its own line");
	}

	/// The TR 101 290 counters are reported together, with their totals, when any of them
	/// moves, and not while they hold.
	#[test]
	#[tracing_test::traced_test]
	fn reports_tr_101_290_errors_when_they_move() {
		let sample = |transport_error: u64, pcr_repetition_error: u64| Snapshot {
			transport_error,
			pcr_repetition_error,
			..Default::default()
		};

		let mut log = Log::default();
		log.sample(sample(0, 0));
		log.sample(sample(1, 0));
		log.sample(sample(1, 0));
		log.sample(sample(1, 2));
		log.finish(&sample(1, 2));

		logs_assert(|lines: &[&str]| {
			let moved: Vec<_> = lines.iter().filter(|line| line.contains("TR 101 290")).collect();
			match moved.as_slice() {
				[first, second]
					if [first, second].iter().all(|line| line.contains(" transport_error=1"))
						&& first.contains(" pcr_repetition_error=0")
						&& second.contains(" pcr_repetition_error=2") =>
				{
					Ok(())
				}
				_ => Err(format!("expected one line per move, got {moved:?}")),
			}
		});
	}

	/// A report carries the counters beside `transport` and `renditions`, camelCase, keyed by
	/// PID, `quiet` in milliseconds, and zero counters omitted; it reads back whole, and a class
	/// a later version adds reads as data.
	#[test]
	fn a_report_flattens_beside_the_media_counters() {
		let mut mpegts = Snapshot {
			continuity_count_error: 2,
			..Default::default()
		};
		mpegts.streams.insert(
			256,
			Stream {
				track: ".avc3".to_string(),
				class: Class::Video,
				units: 120,
				quiet: Some(std::time::Duration::from_micros(40_400)),
				continuity_count_error: 2,
				..Default::default()
			},
		);
		let release = Release {
			margin: Some(-12),
			offset: Some(3),
			drift: Some(1.5),
			..Default::default()
		};
		let mut snapshot = hang::stats::Snapshot::<Report>::default();
		snapshot.ext.mpegts = Some(mpegts);
		snapshot.ext.release = Some(release);

		let json = serde_json::to_value(&snapshot).unwrap();
		assert_eq!(
			json,
			serde_json::json!({
				"mpegts": {
					"streams": {
						"256": { "track": ".avc3", "class": "video", "units": 120, "quiet": 40, "continuityCountError": 2 }
					},
					"continuityCountError": 2
				},
				"release": { "margin": -12, "offset": 3, "drift": 1.5 }
			})
		);
		let mut read: hang::stats::Snapshot<Report> = serde_json::from_value(json).unwrap();
		let quiet = &mut read.ext.mpegts.as_mut().unwrap().streams.get_mut(&256).unwrap().quiet;
		assert_eq!(*quiet, Some(std::time::Duration::from_millis(40)));
		*quiet = snapshot.ext.mpegts.as_ref().unwrap().streams[&256].quiet;
		assert_eq!(read, snapshot);

		let row: Stream = serde_json::from_value(serde_json::json!({ "class": "subtitle", "later": 1 })).unwrap();
		assert_eq!(row.class, Class::Data);
	}

	/// Damage is reported per PID when its cumulative count moves, including the final sample.
	#[test]
	#[tracing_test::traced_test]
	fn reports_damaged_units_when_they_move() {
		let sample = |damaged| {
			let mut stats = Snapshot::default();
			stats.streams.insert(
				256,
				Stream {
					track: ".avc3".to_string(),
					damaged,
					..Default::default()
				},
			);
			stats.streams.insert(
				257,
				Stream {
					track: ".mp2".to_string(),
					..Default::default()
				},
			);
			stats
		};
		let mut log = Log::default();
		log.sample(sample(0));
		log.sample(sample(1));
		log.sample(sample(1));
		log.finish(&sample(2));
		logs_assert(|lines: &[&str]| {
			let refused: Vec<_> = lines
				.iter()
				.filter(|line| line.contains("damaged TS units refused"))
				.collect();
			match refused.as_slice() {
				[first, second]
					if first.contains("pid=256")
						&& first.contains("damaged=1")
						&& second.contains("pid=256")
						&& second.contains("damaged=2") =>
				{
					Ok(())
				}
				_ => Err(format!("expected damage increases on the video PID, got {refused:?}")),
			}
		});
	}
}
