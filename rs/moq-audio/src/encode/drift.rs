//! Keeps captured audio on the host clock its video is stamped on.
//!
//! A device counts samples on its own crystal, typically tens of ppm off the host's, while the
//! encoder stamps by sample count. Left alone, a 100 ppm device drifts 0.36 s against its video
//! every hour. [`Drift`] compares each buffer's capture stamp with the PTS its first sample will
//! get, and steers the resampler's ratio until the two agree, the way an adaptive resampler
//! (ffmpeg's `aresample=async`, GStreamer's `slave-method=resample`) follows a source clock.

use std::time::Duration;

use moq_net::Timestamp;

use crate::resample::MAX_ADJUST;

/// How much input each measurement spans. Capture stamps are only ever late, delayed by the
/// callback or the scheduler, so the earliest error in a window is the one that measures the
/// clocks rather than the delivery.
const WINDOW: Duration = Duration::from_secs(1);

/// The loop's time constant, in windows: a critically damped loop whose double pole sits at
/// `1 - 1 / TAU`. Ten seconds settles within a minute, and holds a 1000 ppm device within 8 ms
/// while it learns the rate.
const TAU: f64 = 10.0;

/// An error, held for a whole window, too large to slew at [`MAX_ADJUST`]: ffmpeg's
/// `min_hard_comp` default. Behind by more, time passed that no sample covers, such as a
/// suspended host or a device that stalled without reporting a gap, and the timeline re-anchors
/// so the gap lands in the PTS. Ahead by more, the audio already published claims time not yet
/// captured, so input is dropped until the stamps catch up.
const RESYNC: Duration = Duration::from_millis(100);

/// The resample ratio that cancels a capture clock's drift.
#[derive(Debug)]
pub(super) struct Drift {
	state: State,
	/// The integral term: the device's rate offset as learned so far, kept across re-anchors since
	/// the device's clock does not change with the timeline.
	rate: f64,
}

#[derive(Debug)]
enum State {
	/// Measuring the current window. Errors are in seconds, positive when samples were captured
	/// later than their PTS.
	Tracking {
		/// The earliest and latest errors in the window.
		low: f64,
		high: f64,
		/// Input the window has covered.
		covered: Duration,
	},
	/// The published audio runs ahead of the stamps: input is dropped until they catch up.
	Ahead,
}

impl State {
	const START: Self = Self::Tracking {
		low: f64::INFINITY,
		high: f64::NEG_INFINITY,
		covered: Duration::ZERO,
	};
}

/// What [`Drift::observe`] asks of the producer.
#[derive(Debug, PartialEq)]
pub(super) enum Steer {
	/// Encode the buffer at the current ratio.
	Hold,
	/// Encode the buffer at this ratio relative to nominal.
	Ratio(f64),
	/// Re-anchor on this buffer's stamp, then encode it.
	Resync,
	/// Drop the buffer: the published audio already covers its time.
	Drop,
}

impl Drift {
	#[cfg(any(feature = "capture", test))]
	pub(super) fn new() -> Self {
		Self {
			state: State::START,
			rate: 0.0,
		}
	}

	/// A buffer `duration` long was captured at `captured`, and its first sample will be stamped
	/// `pts`: decide what to do with it, steering once per [`WINDOW`].
	pub(super) fn observe(&mut self, captured: Timestamp, pts: Timestamp, duration: Duration) -> Steer {
		let (Ok(captured), Ok(pts)) = (captured.as_micros(), pts.as_micros()) else {
			return Steer::Hold;
		};
		let error = (captured as f64 - pts as f64) / 1e6;
		let State::Tracking { low, high, covered } = &mut self.state else {
			if error < 0.0 {
				return Steer::Drop;
			}
			self.state = State::START;
			return Steer::Resync;
		};
		*low = low.min(error);
		*high = high.max(error);
		*covered += duration;
		if *covered < WINDOW {
			return Steer::Hold;
		}

		let (low, high) = (*low, *high);
		self.state = State::START;
		if low > RESYNC.as_secs_f64() {
			return Steer::Resync;
		}
		if high < -RESYNC.as_secs_f64() {
			self.state = State::Ahead;
			return Steer::Drop;
		}
		// Critically damped PI on the error per window: 2 / TAU proportional and 1 / TAU² integral
		// gain. The integral clamps with the ratio, so a device past the bound cannot wind it up.
		let (kp, ki) = (2.0 / TAU, 1.0 / (TAU * TAU));
		let lag = low / WINDOW.as_secs_f64();
		self.rate = (self.rate + ki * lag).clamp(-MAX_ADJUST, MAX_ADJUST);
		Steer::Ratio(1.0 + (self.rate + kp * lag).clamp(-MAX_ADJUST, MAX_ADJUST))
	}

	/// A new timeline begins: measure afresh, keeping the learned rate.
	pub(super) fn restart(&mut self) {
		self.state = State::START;
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	/// A simulated capture: a device `ppm` fast of the host, 10 ms buffers stamped late by up to
	/// `jitter` (a fixed LCG, so every run is the same), and `stalls` of host time no sample covers,
	/// as `(at, length)` in host seconds.
	struct Device {
		ppm: f64,
		jitter: f64,
		stalls: Vec<(f64, f64)>,
	}

	/// What the loop did over `seconds` of a [`Device`]: the error, PTS less capture time, of every
	/// encoded buffer as `(host seconds, error seconds)`, and how often it re-anchored or dropped.
	struct Run {
		errors: Vec<(f64, f64)>,
		/// When each re-anchor happened, in host seconds.
		resyncs: Vec<f64>,
		drops: usize,
	}

	impl Run {
		/// The worst error from `after` host seconds on, in milliseconds.
		fn worst(&self, after: f64) -> f64 {
			let errors = self.errors.iter().filter(|(at, _)| *at >= after);
			errors.map(|(_, error)| error.abs() * 1e3).fold(0.0, f64::max)
		}
	}

	fn run(device: &Device, seconds: f64) -> Run {
		let block = Duration::from_millis(10);
		let mut drift = Drift::new();
		let mut seed = 0x2545_f491_u64;
		// The producer anchors on the first buffer's stamp, and steers from the second on.
		let (mut host, mut anchor, mut position, mut relative) = (0.0, None, 0.0, 1.0);
		let mut stalls = device.stalls.iter().peekable();
		let mut out = Run {
			errors: Vec::new(),
			resyncs: Vec::new(),
			drops: 0,
		};
		while host < seconds {
			if let Some((_, length)) = stalls.next_if(|(at, _)| host >= *at) {
				host += length;
			}
			seed = seed
				.wrapping_mul(6_364_136_223_846_793_005)
				.wrapping_add(1_442_695_040_888_963_407);
			let late = device.jitter * (seed >> 11) as f64 / (1u64 << 53) as f64;
			let micros = |seconds: f64| Timestamp::from_micros((seconds * 1e6).round() as u64).unwrap();
			let captured = 1.0 + host + late;
			let at = *anchor.get_or_insert(captured);
			let steer = match position == 0.0 {
				true => Steer::Hold,
				false => drift.observe(micros(captured), micros(at + position), block),
			};
			match steer {
				Steer::Hold => {}
				Steer::Ratio(ratio) => relative = ratio,
				Steer::Resync => {
					out.resyncs.push(host);
					drift.restart();
					(anchor, position) = (Some(captured), 0.0);
				}
				Steer::Drop => out.drops += 1,
			}
			// The device's 10 ms covers less host time when it runs fast.
			let span = block.as_secs_f64() / (1.0 + device.ppm * 1e-6);
			if matches!(drift.state, State::Tracking { .. }) {
				out.errors.push((host, anchor.unwrap() + position - (1.0 + host)));
				position += block.as_secs_f64() * relative;
			}
			host += span;
		}
		out
	}

	/// One 20 ms audio frame: how far the audio may drift from its video over a session.
	const FRAME: f64 = 20.0;

	/// Over a simulated hour, every device within the bound stays within a frame of its capture
	/// time. Uncorrected, 1000 ppm drifts 3.6 s.
	#[test]
	fn follows_every_device_within_the_bound() {
		for ppm in [-1000.0, -300.0, -50.0, 0.0, 12.0, 100.0, 1000.0] {
			// Buffers delivered on time, or each late by up to a scheduler's 50 ms. The first
			// buffer anchors the timeline late by its own delay, which the loop then removes, so
			// a late source is judged once that has settled.
			for (jitter, settled) in [(0.0, 0.0), (0.05, 60.0)] {
				let run = run(
					&Device {
						ppm,
						jitter,
						stalls: Vec::new(),
					},
					3600.0,
				);
				let worst = run.worst(settled);
				assert_eq!((run.resyncs.len(), run.drops), (0, 0), "{ppm} ppm, {jitter} s late");
				assert!(worst < FRAME, "{ppm} ppm, {jitter} s late: {worst:.2} ms");
			}
		}
	}

	/// A host suspended for five seconds, which no capture gap reports, re-anchors once rather than
	/// slewing for hours, and the learned rate carries over.
	#[test]
	fn a_stall_re_anchors_once() {
		let run = run(
			&Device {
				ppm: 300.0,
				jitter: 0.002,
				stalls: vec![(600.0, 5.0)],
			},
			1200.0,
		);
		assert_eq!((run.resyncs.len(), run.drops), (1, 0));
		// Within two windows of the stall, the first of which still held errors from before it,
		// and on time from there.
		assert!(run.resyncs[0] < 607.0, "re-anchored at {} s", run.resyncs[0]);
		assert!(
			run.worst(run.resyncs[0]) < FRAME,
			"{:.2} ms after the stall",
			run.worst(run.resyncs[0])
		);
	}

	/// Past [`MAX_ADJUST`] the loop runs at it and corrects the rest in steps: a slow device re-anchors
	/// on the gap it leaves, and a fast one drops what runs ahead. Either way the error stays
	/// within the resync threshold rather than growing for the whole session.
	#[test]
	fn a_device_past_the_bound_stays_bounded() {
		for ppm in [-3000.0, 3000.0] {
			let run = run(
				&Device {
					ppm,
					jitter: 0.0,
					stalls: Vec::new(),
				},
				600.0,
			);
			assert!(!run.resyncs.is_empty() || run.drops > 0, "{ppm} ppm");
			assert!(
				run.worst(0.0) < 1e3 * RESYNC.as_secs_f64() + 15.0,
				"{ppm} ppm: {:.2} ms",
				run.worst(0.0)
			);
		}
	}
}
