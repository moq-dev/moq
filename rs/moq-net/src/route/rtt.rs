//! A link's min RTT, as the route layer adds it to the routes learned over the link.

use std::{collections::VecDeque, time::Duration};

/// Reads a link's smoothed RTT once per [`Self::INTERVAL`] and reports the lowest of the
/// last 30 reads in whole milliseconds, changed only once it moves past the larger of
/// 5 ms and a quarter of the value reported.
///
/// The smoothed RTT follows every queue, and a lifetime minimum never rises after a
/// path change; a windowed minimum ignores a queue shorter than the window and turns a
/// path change into one new value.
#[derive(Debug, Default)]
pub(crate) struct LinkRtt {
	reads: VecDeque<Duration>,
	reported: Option<Duration>,
	/// Reads in a row that found no RTT.
	misses: usize,
}

impl LinkRtt {
	/// How often to [read](Self::read) the smoothed RTT.
	pub const INTERVAL: Duration = Duration::from_secs(1);
	/// How many reads the minimum covers.
	const WINDOW: usize = 30;
	/// The smallest change reported.
	const FLOOR: Duration = Duration::from_millis(5);

	/// The min RTT last reported.
	pub fn reported(&self) -> Option<Duration> {
		self.reported
	}

	/// Whether to keep reading: false once a window of reads found no RTT, since a
	/// transport that reports none that long, past a first ping, never will.
	pub fn reading(&self) -> bool {
		self.misses < Self::WINDOW
	}

	/// Take one read of the smoothed RTT, `None` when the transport reports none, and
	/// return the min RTT to report.
	pub fn read(&mut self, rtt: Option<Duration>) -> Option<Duration> {
		let Some(rtt) = rtt else {
			self.reads.clear();
			self.reported = None;
			self.misses += 1;
			return None;
		};
		self.misses = 0;
		if self.reads.len() == Self::WINDOW {
			self.reads.pop_front();
		}
		self.reads.push_back(rtt);
		let min = self.reads.iter().min().expect("just pushed");
		let min = Duration::from_millis(u64::try_from(min.as_micros().div_ceil(1000)).unwrap_or(u64::MAX));
		match self.reported {
			Some(reported) if min.abs_diff(reported) <= (reported / 4).max(Self::FLOOR) => {}
			_ => self.reported = Some(min),
		}
		self.reported
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn ms(ms: u64) -> Option<Duration> {
		Some(Duration::from_millis(ms))
	}

	/// Feed `reads` one per interval, returning each report.
	fn feed(link: &mut LinkRtt, reads: impl IntoIterator<Item = Option<Duration>>) -> Vec<Option<Duration>> {
		reads.into_iter().map(|rtt| link.read(rtt)).collect()
	}

	#[test]
	fn the_first_read_reports_at_once() {
		let mut link = LinkRtt::default();
		assert_eq!(link.read(ms(30)), ms(30));
	}

	#[test]
	fn a_queue_shorter_than_the_window_reports_nothing() {
		let mut link = LinkRtt::default();
		link.read(ms(30));
		// 29 seconds of a growing queue, then it drains.
		let queued = (1..30).map(|i| ms(30 + i * 3));
		assert!(feed(&mut link, queued).iter().all(|report| *report == ms(30)));
		assert_eq!(link.read(ms(31)), ms(30));
	}

	#[test]
	fn a_rise_reports_once_the_window_passes_it() {
		let mut link = LinkRtt::default();
		link.read(ms(30));
		let reports = feed(&mut link, std::iter::repeat_n(ms(55), 30));
		// The window still holds the last 30 ms read until the 30th read of 55 ms.
		assert!(reports[..29].iter().all(|report| *report == ms(30)));
		assert_eq!(reports[29], ms(55));
	}

	#[test]
	fn a_drop_reports_at_once() {
		let mut link = LinkRtt::default();
		link.read(ms(80));
		assert_eq!(link.read(ms(40)), ms(40));
	}

	#[test]
	fn a_change_within_the_band_reports_nothing() {
		let mut link = LinkRtt::default();
		link.read(ms(10));
		// A quarter of 10 ms is below the 5 ms floor.
		assert_eq!(link.read(ms(5)), ms(10));
		assert_eq!(link.read(ms(4)), ms(4));
		let mut link = LinkRtt::default();
		link.read(ms(100));
		assert_eq!(link.read(ms(75)), ms(100));
		assert_eq!(link.read(ms(74)), ms(74));
	}

	#[test]
	fn a_transport_without_rtt_reports_none_and_starts_over() {
		let mut link = LinkRtt::default();
		link.read(ms(80));
		assert_eq!(link.read(None), None);
		assert_eq!(link.read(ms(90)), ms(90));
	}

	#[test]
	fn a_window_without_rtt_stops_reading() {
		let mut link = LinkRtt::default();
		feed(&mut link, std::iter::repeat_n(None, 29));
		assert!(link.reading());
		link.read(None);
		assert!(!link.reading());
	}

	#[test]
	fn a_fraction_of_a_millisecond_rounds_up() {
		let mut link = LinkRtt::default();
		assert_eq!(link.read(Some(Duration::from_micros(300))), ms(1));
	}
}
