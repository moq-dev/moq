//! Measuring a transport stream's multiplex rate from its PCR clock.
//!
//! A constant-rate multiplexer paces its packets on the PCR clock, so the packets
//! between two PCRs over the ticks between them is the rate of the whole
//! multiplex, stuffing included. [`Meter`] takes every packet and every PCR on
//! the clock PID and decides when that rate is worth recording: a full window
//! whose samples agree on one value publishes it, and a window that no longer
//! agrees clears it, so a VBR source (whose samples never agree) records nothing
//! and a rate change lands once rather than as churn.

/// The PCR clock, in Hz.
const PCR_HZ: u64 = 27_000_000;
/// Bits per transport packet.
const PACKET_BITS: u64 = 188 * 8;
/// The 33-bit base times 300 plus the 9-bit extension: the PCR wraps here.
const PCR_WRAP: u64 = (1 << 33) * 300;
/// How much PCR time one window spans before it is judged.
const WINDOW: u64 = 2 * PCR_HZ;
/// Intervals are pooled into samples this long before they are compared. One
/// packet of rounding, or a clock a hardware muxer stamps at half-millisecond
/// granularity, is a few percent of one 25 ms interval but well inside the
/// tolerance over half a second.
const SAMPLE: u64 = WINDOW / 4;
/// A PCR interval longer than this is a gap or a wrap gone wrong, not a sample:
/// TR 101 290 already flags anything over 100 ms.
const MAX_INTERVAL: u64 = PCR_HZ;
/// How far one sample may sit from a rate and still agree with it, in thousandths.
/// A hardware multiplexer's rate wanders more than a percent between half-second
/// samples while its whole-window average holds far tighter; a VBR source is off by
/// multiples.
const AGREEMENT: u64 = 20;
/// How far a stable window must move before the published rate follows it, in
/// thousandths, so measurement noise never touches the catalog.
const DRIFT: u64 = 10;

/// Packets over the PCR ticks they were paced across.
#[derive(Clone, Copy, Default)]
struct Interval {
	packets: u64,
	ticks: u64,
}

impl Interval {
	fn add(&mut self, other: Interval) {
		self.packets += other.packets;
		self.ticks += other.ticks;
	}

	fn rate(&self) -> u64 {
		rate(self.packets, self.ticks)
	}

	fn agrees(&self, rate: u64) -> bool {
		self.rate().abs_diff(rate) * 1000 <= rate * AGREEMENT
	}
}

/// Bits per second for `packets` spread over `ticks` of the 27 MHz clock.
fn rate(packets: u64, ticks: u64) -> u64 {
	(u128::from(packets) * u128::from(PACKET_BITS) * u128::from(PCR_HZ) / u128::from(ticks)) as u64
}

/// Tracks the multiplex rate and the value currently recorded for it.
#[derive(Default)]
pub(super) struct Meter {
	/// Packets since the last PCR, every PID and null stuffing included.
	packets: u64,
	/// The last PCR seen, in 27 MHz ticks.
	last: Option<u64>,
	/// The sample being pooled from PCR intervals.
	sample: Interval,
	/// The completed samples of the window being collected.
	window: Vec<Interval>,
	/// The rate recorded in the catalog, if any.
	published: Option<u64>,
}

impl Meter {
	/// The rate recorded in the catalog.
	pub fn published(&self) -> Option<u64> {
		self.published
	}

	/// One transport packet went by.
	pub fn packet(&mut self) {
		self.packets += 1;
	}

	/// A PCR arrived on the clock PID, carried by the packet just counted. Returns
	/// whether [`published`](Self::published) changed.
	pub fn pcr(&mut self, pcr: u64) -> bool {
		let packets = std::mem::take(&mut self.packets);
		let Some(last) = self.last.replace(pcr) else {
			return false;
		};
		// The clock wraps at 2^33 * 300 ticks, so a small forward step past the wrap
		// reads as a huge one here and is caught by the interval cap below.
		let ticks = (pcr + PCR_WRAP - last) % PCR_WRAP;
		if ticks == 0 || ticks > MAX_INTERVAL {
			// A stalled or backwards clock with no discontinuity flag is a break all the same.
			return self.discontinuity();
		}
		self.sample.add(Interval { packets, ticks });
		if self.sample.ticks < SAMPLE {
			return false;
		}
		self.window.push(std::mem::take(&mut self.sample));
		if self.window.iter().map(|s| s.ticks).sum::<u64>() < WINDOW {
			return false;
		}
		self.judge()
	}

	/// The clock PID declared a system time-base discontinuity, or the clock PID
	/// itself changed. The window is worthless and so is a rate measured before it.
	/// Returns whether [`published`](Self::published) changed.
	pub fn discontinuity(&mut self) -> bool {
		self.packets = 0;
		self.last = None;
		self.sample = Interval::default();
		self.window.clear();
		self.published.take().is_some()
	}

	/// Judge the full window and discard it.
	fn judge(&mut self) -> bool {
		let window = std::mem::take(&mut self.window);
		let mut whole = Interval::default();
		window.iter().for_each(|s| whole.add(*s));
		let rate = whole.rate();
		let stable = window.iter().all(|s| s.agrees(rate));
		match self.published {
			None if stable => {
				self.published = Some(rate);
				true
			}
			None => false,
			// A stable window at a new rate replaces the old one once it has drifted.
			Some(published) if stable => {
				let drifted = rate.abs_diff(published) * 1000 > published * DRIFT;
				if drifted {
					self.published = Some(rate);
				}
				drifted
			}
			// An unstable window still holding samples at the published rate is a
			// glitch in a source that is otherwise the same; one holding none is a
			// source that stopped being constant-rate.
			Some(published) => {
				let gone = !window.iter().any(|s| s.agrees(published));
				if gone {
					self.published = None;
				}
				gone
			}
		}
	}
}

#[cfg(test)]
mod test {
	use super::*;

	/// Feed `intervals` of (packets, ticks), returning the published rate after each
	/// call that changed it.
	fn feed(meter: &mut Meter, intervals: impl IntoIterator<Item = (u64, u64)>) -> Vec<Option<u64>> {
		let mut changes = Vec::new();
		let mut pcr = meter.last.unwrap_or(0);
		if meter.last.is_none() {
			meter.packet();
			assert!(!meter.pcr(pcr));
		}
		for (packets, ticks) in intervals {
			for _ in 0..packets {
				meter.packet();
			}
			pcr = (pcr + ticks) % PCR_WRAP;
			if meter.pcr(pcr) {
				changes.push(meter.published());
			}
		}
		changes
	}

	/// 2.5 Mb/s: 44 packets per 714_701 ticks, like the kyrion fixture.
	const CBR: (u64, u64) = (44, 714_701);
	/// Intervals of that length that fill one window exactly, so each feed below
	/// starts and ends on a window boundary.
	const PER_WINDOW: usize = 4 * SAMPLE.div_ceil(CBR.1) as usize;

	#[test]
	fn cbr_publishes_once_per_window() {
		let mut meter = Meter::default();
		let changes = feed(&mut meter, std::iter::repeat_n(CBR, PER_WINDOW * 3));
		assert_eq!(changes, vec![Some(2_499_999)], "published once, then held");
	}

	#[test]
	fn vbr_never_publishes() {
		let mut meter = Meter::default();
		// A file-paced source: the clock is a grid but the packets come in bursts.
		let intervals = (0..PER_WINDOW as u64 * 3).map(|i| (if (i / 7) % 2 == 0 { 10 } else { 60 }, 714_701));
		assert!(feed(&mut meter, intervals).is_empty());
		assert_eq!(meter.published(), None);
	}

	#[test]
	fn a_glitch_holds_the_rate_and_a_new_rate_replaces_it() {
		let mut meter = Meter::default();
		assert_eq!(
			feed(&mut meter, std::iter::repeat_n(CBR, PER_WINDOW)),
			vec![Some(2_499_999)]
		);

		// One outlier interval throws its sample; the other samples still agree.
		let glitch = std::iter::once((200, 714_701)).chain(std::iter::repeat_n(CBR, PER_WINDOW - 1));
		assert!(feed(&mut meter, glitch).is_empty());
		assert_eq!(meter.published(), Some(2_499_999));

		// Half a percent is noise; the catalog does not follow it.
		let noise = std::iter::repeat_n((44, 711_000), PER_WINDOW);
		assert!(feed(&mut meter, noise).is_empty());

		// A stable window at a clearly different rate replaces the published one.
		let faster = std::iter::repeat_n((88, 714_701), PER_WINDOW);
		assert_eq!(feed(&mut meter, faster), vec![Some(4_999_998)]);
	}

	#[test]
	fn instability_clears_and_a_fresh_window_republishes() {
		let mut meter = Meter::default();
		assert_eq!(
			feed(&mut meter, std::iter::repeat_n(CBR, PER_WINDOW)),
			vec![Some(2_499_999)]
		);

		// A full window with nothing at the published rate clears it once.
		let vbr = (0..PER_WINDOW as u64).map(|i| (if (i / 7) % 2 == 0 { 10 } else { 30 }, 714_701));
		assert_eq!(feed(&mut meter, vbr), vec![None]);

		// The stream settles again: published after one fresh stable window, not before.
		let changes = feed(&mut meter, std::iter::repeat_n(CBR, PER_WINDOW - 1));
		assert!(changes.is_empty(), "no publish before the window fills");
		assert_eq!(feed(&mut meter, [CBR]), vec![Some(2_499_999)]);
	}

	#[test]
	fn discontinuity_clears_immediately() {
		let mut meter = Meter::default();
		assert_eq!(
			feed(&mut meter, std::iter::repeat_n(CBR, PER_WINDOW)),
			vec![Some(2_499_999)]
		);
		assert!(meter.discontinuity(), "a published rate is cleared");
		assert_eq!(meter.published(), None);
		assert!(!meter.discontinuity(), "nothing left to clear");

		// A backwards clock without the flag is a discontinuity too.
		assert_eq!(
			feed(&mut meter, std::iter::repeat_n(CBR, PER_WINDOW)),
			vec![Some(2_499_999)]
		);
		meter.packet();
		assert!(meter.pcr(1), "the clock jumped backwards");
		assert_eq!(meter.published(), None);
	}

	#[test]
	fn survives_the_pcr_wrap() {
		let mut meter = Meter {
			last: Some(PCR_WRAP - CBR.1 * 3),
			..Default::default()
		};
		let changes = feed(&mut meter, std::iter::repeat_n(CBR, PER_WINDOW));
		assert_eq!(changes, vec![Some(2_499_999)]);
	}
}
