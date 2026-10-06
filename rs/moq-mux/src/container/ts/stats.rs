//! Reporting on the counters [`Import::stats`](super::Import::stats) and
//! [`Export::stats`](super::Export::stats) return.

use std::collections::BTreeSet;

use super::{Stats, StreamStats};

/// Logs what moved in an importer's or exporter's [`Stats`] from one sample to the next, so
/// every front door that runs one reports the same lines.
///
/// Feed it a snapshot every [`INTERVAL`](Self::INTERVAL) of wall time. The caller owns the
/// timer, since what counts as "now" differs between a CLI, a server, and a test.
#[derive(Default)]
pub struct Log {
	previous: Option<Stats>,
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
	pub fn sample(&mut self, latest: Stats) {
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
	pub fn finish(&self, latest: &Stats) {
		self.sync(latest);
	}

	/// Report the streams whose damage or frame-sync counters moved, one line each, the PSI
	/// sections dropped for a bad CRC if that count moved, and the TR 101 290 counters if any
	/// moved.
	///
	/// The importer already warns on each individual resync and dropped section; this is the
	/// running total, which is what an operator turns into a rate.
	fn sync(&self, latest: &Stats) {
		if self.previous.as_ref().map_or(0, |previous| previous.crc_error) != latest.crc_error {
			tracing::info!(
				crc_error = latest.crc_error,
				"PAT or PMT sections dropped for a bad CRC"
			);
		}
		if self.previous.as_ref().map_or([0; 9], Stats::health) != latest.health() {
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
		let lost = |stream: &StreamStats| (stream.resyncs, stream.discarded, stream.unconfirmed);
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
	use super::super::StreamClass;
	use super::*;

	/// A stream whose count stops across a sample is logged once, with its silence, and again
	/// only after it has delivered in between; a stream that keeps counting never is, and
	/// neither reports frame sync it never lost.
	#[test]
	#[tracing_test::traced_test]
	fn reports_a_stream_that_stopped() {
		const VIDEO: u16 = 0x100;
		const AUDIO: u16 = 0x101;
		let sample = |video: u64, audio: u64| {
			let mut stats = Stats::default();
			for (pid, track, class, units) in [
				(VIDEO, ".avc3", StreamClass::Video, video),
				(AUDIO, ".mp2", StreamClass::Audio, audio),
			] {
				let stream = StreamStats {
					track,
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
			let mut stats = Stats::default();
			stats.streams.insert(
				VIDEO,
				StreamStats {
					track: ".avc3",
					class: StreamClass::Video,
					units: video,
					quiet: Some(std::time::Duration::from_secs(1)),
					..Default::default()
				},
			);
			stats.streams.insert(
				CUE,
				StreamStats {
					track: ".ts",
					class: StreamClass::Data,
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
			let stream = StreamStats {
				track: ".mp2",
				class: StreamClass::Audio,
				units,
				resyncs,
				..Default::default()
			};
			let mut stats = Stats::default();
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
		let sample = |crc_error: u64| Stats {
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
		let sample = |transport_error: u64, pcr_repetition_error: u64| Stats {
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

	/// Damage is reported per PID when its cumulative count moves, including the final sample.
	#[test]
	#[tracing_test::traced_test]
	fn reports_damaged_units_when_they_move() {
		let sample = |damaged| {
			let mut stats = Stats::default();
			stats.streams.insert(
				256,
				StreamStats {
					track: ".avc3",
					damaged,
					..Default::default()
				},
			);
			stats.streams.insert(
				257,
				StreamStats {
					track: ".mp2",
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
