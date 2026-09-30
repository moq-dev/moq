//! Reporting on the per-stream counters [`Import::stats`](super::Import::stats) and
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
	/// Logs a stream whose access units did not advance since the last sample, once per
	/// silence, and any stream whose frame-sync counters moved.
	pub fn sample(&mut self, latest: Stats) {
		self.sync(&latest);
		for (pid, stream) in &latest.streams {
			let previous = self.previous.as_ref().and_then(|previous| previous.streams.get(pid));
			if previous.is_none_or(|previous| previous.units != stream.units) {
				self.quiet.remove(pid);
			} else if self.quiet.insert(*pid) {
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
	/// It covers less than an interval, so only the frame sync is compared: the drain at end
	/// of input can publish a frame nothing vouched for.
	pub fn finish(&self, latest: &Stats) {
		self.sync(latest);
	}

	/// Report the streams whose frame-sync counters moved, one line each.
	///
	/// The importer already warns on each individual resync; this is the running total, which
	/// is what an operator turns into a rate.
	fn sync(&self, latest: &Stats) {
		let lost = |stream: &StreamStats| (stream.resyncs, stream.discarded, stream.unconfirmed);
		for (pid, stream) in &latest.streams {
			let previous = self.previous.as_ref().and_then(|previous| previous.streams.get(pid));
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
			for (pid, track, units) in [(VIDEO, ".avc3", video), (AUDIO, ".mp2", audio)] {
				let stream = StreamStats {
					track,
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

	/// A resync is reported on the sample that saw it, and the end of input still reports one
	/// the last partial interval found.
	#[test]
	#[tracing_test::traced_test]
	fn reports_frame_sync_lost_at_the_end() {
		let sample = |resyncs: u64, units: u64| {
			let stream = StreamStats {
				track: ".mp2",
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
}
