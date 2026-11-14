//! The received-audio time stretch doc/concept/audio-jitter.md specifies for
//! this crate and `@moq/watch`: a rate proportional to the playhead's lag
//! behind the shared clock, played as waveform-similarity overlap-add (WSOLA)
//! splices, whatever the content.

/// The most the stretch speeds up or slows down, as a fraction of real time.
pub(crate) const CAP: f64 = 0.1;

/// The lag, in milliseconds, whose rate reaches the cap: the rate is the lag
/// over this.
const GAIN: f64 = 500.0;

/// What a hop boundary decides on, in frames at the device rate.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Inputs {
	/// How far the playhead trails the shared clock: negative when it plays
	/// ahead of it.
	pub lag: f64,
	/// Received audio ahead of the playhead.
	pub depth: f64,
	/// How far audio may trail the clock before it snaps onto it. Zero turns
	/// the stretch off.
	pub bound: f64,
	/// What the device reads at once.
	pub read: f64,
}

/// How a hop may move the playhead, in frames: a positive shift skips ahead, a
/// negative one repeats.
#[derive(Clone, Copy, Debug, PartialEq)]
#[cfg_attr(test, derive(serde::Deserialize))]
pub(crate) struct Decision {
	/// The rate the playhead moves at, less real time: positive plays faster,
	/// zero holds.
	pub pace: f64,
	/// The least and most one splice may shift the playhead.
	pub low: i64,
	pub high: i64,
	/// Splice as soon as the cap allows, rather than once the pace adds up to
	/// a hop, as audio about to run out must.
	pub urgent: bool,
}

/// Play as received.
pub(crate) const HOLD: Decision = Decision {
	pace: 0.0,
	low: 0,
	high: 0,
	urgent: false,
};

/// The frames of one hop at `rate`: 10 ms, the crossfade a splice plays and
/// the spacing of decisions.
pub(crate) fn hop(rate: u32) -> usize {
	(rate as usize * 10 / 1000).max(1)
}

/// The shared decision rule. Audio about to run out, with less received ahead
/// of the playhead than two hops and a device read, slows down at the cap,
/// never trailing the clock past the bound. Otherwise a lag past half a hop
/// moves the playhead toward the clock at a rate proportional to it. A splice
/// moves the playhead at least half a hop, never carries it more than half a
/// hop past the clock, and never speeds up into that cushion.
pub(crate) fn decide(rate: u32, inputs: Inputs) -> Decision {
	let Inputs {
		lag,
		depth,
		bound,
		read,
	} = inputs;
	if bound <= 0.0 {
		return HOLD;
	}
	let frames = hop(rate) as f64;
	let cushion = 2.0 * frames + read;
	let least = (frames / 2.0).floor();
	if depth < cushion && lag < bound {
		return Decision {
			pace: -CAP,
			low: (lag - bound).ceil() as i64,
			high: -least as i64,
			urgent: true,
		};
	}
	if lag.abs() <= least {
		return HOLD;
	}
	let pace = (lag * 1000.0 / (rate as f64 * GAIN)).clamp(-CAP, CAP);
	if lag > 0.0 {
		Decision {
			pace,
			low: least as i64,
			high: (lag + least).min(depth - cushion).floor() as i64,
			urgent: false,
		}
	} else {
		Decision {
			pace,
			low: (lag - least).ceil() as i64,
			high: -least as i64,
			urgent: false,
		}
	}
}

/// Received PCM on its way out, with the history and lookahead a splice
/// searches, allocated before entering the callback.
///
/// Output copies the input until a hop boundary decides to splice: the next
/// hop then crossfades from the input's natural continuation into the stretch
/// of it most similar to that continuation, a shift of at most `span` either
/// way, and continues from there.
pub(crate) struct Stretch {
	channels: usize,
	/// One hop: the crossfade a splice plays, and the output between decisions.
	pub hop: usize,
	/// Two hops: the farthest one splice moves the playhead, and the history
	/// kept behind it.
	pub span: usize,
	/// Three hops: the received audio past the playhead a splice can need.
	pub window: usize,

	/// Interleaved input frames held, and the playhead among them. During a
	/// crossfade the playhead stays where it started, and moves on past the
	/// shifted hop once the crossfade ends.
	input: Vec<f32>,
	filled: usize,
	head: usize,
	/// The crossfade in progress, a zero shift for none, and its frames played.
	shift: i64,
	faded: usize,
	/// How far the pace has moved the playhead that no splice took yet, and
	/// which way it moves.
	owed: f64,
	direction: i8,
	/// How far splices may still move the playhead: refilled at the cap each
	/// hop, up to a span.
	allowance: f64,
	/// Output frames until the next hop boundary.
	until: usize,

	/// The crossfade's weights, and the analysis scratch: the channels' sum and
	/// its running energy.
	weights: Vec<f64>,
	mono: Vec<f64>,
	energy: Vec<f64>,
	/// The coarse search's stride: every shift on an 8 kHz grid.
	step: usize,
}

impl Stretch {
	pub fn new(rate: u32, channels: usize) -> Self {
		let hop = hop(rate);
		let span = 2 * hop;
		let window = 3 * hop;
		let weights = (0..hop)
			.map(|k| 0.5 - 0.5 * (std::f64::consts::PI * (k + 1) as f64 / (hop + 1) as f64).cos())
			.collect();
		Self {
			channels,
			hop,
			span,
			window,
			// The history, the window, and a hop of room to top it up.
			input: vec![0.0; (span + window + hop) * channels],
			filled: 0,
			head: 0,
			shift: 0,
			faded: 0,
			owed: 0.0,
			direction: 0,
			allowance: span as f64,
			until: 0,
			weights,
			mono: vec![0.0; 2 * span + hop],
			energy: vec![0.0; 2 * span + hop + 1],
			step: (rate as usize / 8000).max(1),
		}
	}

	/// Whether a hop boundary is next, so [`next`](Self::next) must decide
	/// before anything more plays.
	pub fn due(&self) -> bool {
		self.until == 0
	}

	/// Output frames left in what the stretch holds, as no further splice
	/// changes them.
	pub fn available(&self) -> usize {
		(self.filled as i64 - self.head as i64 - self.shift - self.faded as i64) as usize
	}

	/// Input frames past where the playhead, or the crossfade it plays, starts.
	pub fn queued(&self) -> usize {
		self.filled - self.head
	}

	/// Input frames the stretch has room for, keeping its history.
	pub fn room(&mut self) -> usize {
		self.compact(self.span);
		self.input.len() / self.channels - self.filled
	}

	/// The interleaved room for the next `frames` input frames, which
	/// [`commit`](Self::commit) appends once filled. At most [`room`](Self::room).
	pub fn spare(&mut self, frames: usize) -> &mut [f32] {
		let channels = self.channels;
		&mut self.input[self.filled * channels..(self.filled + frames) * channels]
	}

	/// Append the first `frames` of the [`spare`](Self::spare) room.
	pub fn commit(&mut self, frames: usize) {
		self.filled += frames;
	}

	/// Drop the history, so no splice reaches back across a boundary in what
	/// the queue holds.
	pub fn forget(&mut self) {
		self.compact(0);
	}

	/// Drop everything held, a crossfade in progress included.
	pub fn clear(&mut self) {
		self.filled = 0;
		self.head = 0;
		self.shift = 0;
		self.faded = 0;
		self.owed = 0.0;
		self.direction = 0;
		self.allowance = self.span as f64;
		self.until = 0;
	}

	/// Decide the hop starting now, returning the shift a splice took, or zero.
	/// The pace adds up until it reaches a hop, or at once when urgent, and a
	/// splice then takes the most similar shift within a hop of that, inside
	/// the decision's range, the cap's allowance, and what the stretch holds.
	pub fn next(&mut self, decision: Decision) -> i64 {
		let hop = self.hop as f64;
		let span = self.span as f64;
		self.until = self.hop;
		self.allowance = (self.allowance + CAP * hop).min(span);
		let direction = if decision.pace > 0.0 {
			1
		} else if decision.pace < 0.0 {
			-1
		} else {
			0
		};
		if direction != self.direction {
			self.direction = direction;
			self.owed = 0.0;
		}
		if direction == 0 {
			return 0;
		}
		self.owed = (self.owed + decision.pace * hop).clamp(-span, span);
		if !decision.urgent && f64::from(direction) * self.owed < hop {
			return 0;
		}

		// The crossfade reads a hop of natural continuation and a hop past the shift.
		let Some(ahead) = self.queued().checked_sub(self.hop) else {
			return 0;
		};
		let allowed = self.allowance.floor() as i64;
		let mut low = decision.low.max(-allowed).max(-(self.head as i64));
		let mut high = decision.high.min(allowed).min(ahead as i64);
		if !decision.urgent {
			low = low.max(self.owed.ceil() as i64 - self.hop as i64);
			high = high.min(self.owed.floor() as i64 + self.hop as i64);
		}
		let shift = self.search(low, high, direction);
		if shift == 0 {
			return 0;
		}
		self.shift = shift;
		self.faded = 0;
		self.owed = (self.owed - shift as f64).clamp(-span, span);
		self.allowance -= shift.unsigned_abs() as f64;
		shift
	}

	/// Play up to `output`'s frames from the playhead, stopping at the next hop
	/// boundary, and return how many.
	pub fn read(&mut self, output: &mut [f32]) -> usize {
		self.advance(output.len() / self.channels, Some(output))
	}

	/// Drop up to `frames` of output as though played, deciding to hold at
	/// each hop boundary they cross, and return how many.
	pub fn discard(&mut self, frames: usize) -> usize {
		let mut done = 0;
		loop {
			done += self.advance(frames - done, None);
			if done == frames || !self.due() || self.queued() == 0 {
				return done;
			}
			self.next(HOLD);
		}
	}

	// Play, or drop without `output`, up to `frames` from the playhead,
	// stopping at the next hop boundary.
	fn advance(&mut self, frames: usize, mut output: Option<&mut [f32]>) -> usize {
		let channels = self.channels;
		let mut done = 0;
		while done < frames && self.until > 0 {
			if self.shift != 0 {
				let count = (frames - done).min(self.until);
				if let Some(output) = output.as_deref_mut() {
					let from = self.head + self.faded;
					let to = (from as i64 + self.shift) as usize;
					for i in 0..count {
						let weight = self.weights[self.faded + i];
						for channel in 0..channels {
							let a = self.input[(from + i) * channels + channel] as f64;
							let b = self.input[(to + i) * channels + channel] as f64;
							output[(done + i) * channels + channel] = (a * (1.0 - weight) + b * weight) as f32;
						}
					}
				}
				self.faded += count;
				self.until -= count;
				done += count;
				if self.faded == self.hop {
					self.head = (self.head as i64 + self.shift) as usize + self.hop;
					self.shift = 0;
					self.faded = 0;
				}
				continue;
			}
			let count = (frames - done).min(self.until).min(self.filled - self.head);
			if count == 0 {
				break;
			}
			if let Some(output) = output.as_deref_mut() {
				output[done * channels..(done + count) * channels]
					.copy_from_slice(&self.input[self.head * channels..(self.head + count) * channels]);
			}
			self.head += count;
			self.until -= count;
			done += count;
		}
		done
	}

	/// Keep at most `history` frames behind the playhead.
	fn compact(&mut self, history: usize) {
		let Some(drop) = self.head.checked_sub(history).filter(|&drop| drop > 0) else {
			return;
		};
		let channels = self.channels;
		self.input.copy_within(drop * channels..self.filled * channels, 0);
		self.head -= drop;
		self.filled -= drop;
	}

	/// The shift in `low..=high` whose hop of the channels' sum best matches the
	/// natural continuation's, by normalized correlation, nearest first so a tie
	/// keeps the smaller shift: every shift on the coarse grid, then every one
	/// within a step of the best. Zero when the range is empty.
	fn search(&mut self, low: i64, high: i64, direction: i8) -> i64 {
		if low > high {
			return 0;
		}
		let hop = self.hop;
		let channels = self.channels;
		let from = self.head as i64 + low.min(0);
		let length = (high.max(0) + hop as i64 - low.min(0)) as usize;
		for i in 0..length {
			let frame = (from as usize + i) * channels;
			let mut sum = 0.0;
			for channel in 0..channels {
				sum += self.input[frame + channel] as f64;
			}
			self.mono[i] = sum;
			self.energy[i + 1] = self.energy[i] + sum * sum;
		}
		let natural = (self.head as i64 - from) as usize;
		let mono = &self.mono;
		let energy = &self.energy;
		let power = energy[natural + hop] - energy[natural];
		let similarity = |shift: i64| {
			let at = (natural as i64 + shift) as usize;
			let mut dot = 0.0;
			for k in 0..hop {
				dot += mono[natural + k] * mono[at + k];
			}
			let product = power * (energy[at + hop] - energy[at]);
			if product > 0.0 { dot / product.sqrt() } else { 0.0 }
		};

		let direction = i64::from(direction);
		let (near, far) = if direction > 0 { (low, high) } else { (high, low) };
		let step = self.step as i64;
		let mut best = near;
		let mut score = f64::NEG_INFINITY;
		let mut shift = near;
		while direction * (far - shift) >= 0 {
			let value = similarity(shift);
			if value > score {
				score = value;
				best = shift;
			}
			shift += direction * step;
		}
		let (start, end) = if direction > 0 {
			(low.max(best - step + 1), high.min(best + step - 1))
		} else {
			(high.min(best + step - 1), low.max(best - step + 1))
		};
		score = f64::NEG_INFINITY;
		let mut shift = start;
		while direction * (end - shift) >= 0 {
			let value = similarity(shift);
			if value > score {
				score = value;
				best = shift;
			}
			shift += direction;
		}
		best
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use serde::Deserialize;

	#[derive(Deserialize)]
	struct Rule {
		name: String,
		rate: u32,
		lag: f64,
		depth: f64,
		bound: f64,
		read: f64,
		decision: Decision,
	}

	#[derive(Deserialize)]
	struct Case {
		name: String,
		rate: u32,
		channels: usize,
		input: Vec<f32>,
		decisions: Vec<Decision>,
		shifts: Vec<i64>,
		output: Vec<f32>,
	}

	/// Play interleaved `input` the way the ring does: topped up to a window
	/// before each hop boundary decides.
	fn play(stretch: &mut Stretch, channels: usize, input: &[f32], decisions: &[Decision]) -> (Vec<f32>, Vec<i64>) {
		let frames = input.len() / channels;
		let mut output = Vec::new();
		let mut chunk = vec![0.0; stretch.hop * channels];
		let mut shifts = Vec::new();
		let mut fed = 0;
		loop {
			let count = (frames - fed)
				.min(stretch.window.saturating_sub(stretch.queued()))
				.min(stretch.room());
			stretch
				.spare(count)
				.copy_from_slice(&input[fed * channels..(fed + count) * channels]);
			stretch.commit(count);
			fed += count;
			if stretch.due() {
				if stretch.queued() == 0 {
					break;
				}
				let decision = decisions[shifts.len().min(decisions.len() - 1)];
				shifts.push(stretch.next(decision));
			}
			let played = stretch.read(&mut chunk);
			if played == 0 {
				break;
			}
			output.extend_from_slice(&chunk[..played * channels]);
		}
		(output, shifts)
	}

	#[test]
	fn shared_decision_vectors() {
		let rules: Vec<Rule> =
			serde_json::from_str(include_str!("../../../doc/concept/audio-jitter/stretch/decisions.json")).unwrap();
		for rule in rules {
			let inputs = Inputs {
				lag: rule.lag,
				depth: rule.depth,
				bound: rule.bound,
				read: rule.read,
			};
			assert_eq!(decide(rule.rate, inputs), rule.decision, "{}", rule.name);
		}
	}

	#[test]
	fn shared_pcm_vectors() {
		let cases: Vec<Case> =
			serde_json::from_str(include_str!("../../../doc/concept/audio-jitter/stretch/corpus.json")).unwrap();
		for case in cases {
			let mut stretch = Stretch::new(case.rate, case.channels);
			let (output, shifts) = play(&mut stretch, case.channels, &case.input, &case.decisions);
			assert_eq!(shifts, case.shifts, "{}", case.name);
			assert_eq!(output.len(), case.output.len(), "{}", case.name);
			for (index, (&actual, &expected)) in output.iter().zip(&case.output).enumerate() {
				assert!(
					(actual - expected).abs() <= 0.000001,
					"{} sample {index}: {actual} != {expected}",
					case.name
				);
			}
		}
	}

	/// A 100 Hz tone at 8 kHz, whose 80-frame period is exactly one hop.
	fn tone(frames: usize) -> Vec<f32> {
		(0..frames)
			.map(|i| 0.25 * (2.0 * std::f32::consts::PI * i as f32 / 80.0).sin())
			.collect()
	}

	fn bridge(rate: u32) -> Decision {
		decide(
			rate,
			Inputs {
				lag: 0.0,
				depth: 0.0,
				bound: rate as f64,
				read: 128.0,
			},
		)
	}

	#[test]
	fn splices_no_faster_than_the_cap_refills_its_allowance() {
		// A bridge asks to splice every hop: past a burst of one span, any run of
		// hops repeats at most a tenth of what it plays.
		let mut stretch = Stretch::new(8000, 1);
		let (_, shifts) = play(&mut stretch, 1, &tone(8000), &[bridge(8000)]);
		assert!(shifts.iter().filter(|&&shift| shift != 0).count() > 10);
		for from in 0..shifts.len() {
			let mut repeated = 0;
			for (to, shift) in shifts.iter().enumerate().skip(from) {
				repeated -= shift;
				let limit = stretch.span as f64 + 0.1 * (to - from + 1) as f64 * stretch.hop as f64;
				assert!(repeated as f64 <= limit, "hops {from}..={to} repeated {repeated}");
			}
		}
	}

	#[test]
	fn never_reaches_back_across_a_boundary_it_forgot() {
		let mut stretch = Stretch::new(8000, 1);
		let input = tone(2 * stretch.window);
		let window = stretch.window;
		stretch.spare(window).copy_from_slice(&input[..window]);
		stretch.commit(window);
		stretch.next(HOLD);
		let mut out = vec![0.0; stretch.hop];
		stretch.read(&mut out);
		let hop = stretch.hop;
		stretch.spare(hop).copy_from_slice(&input[window..window + hop]);
		stretch.commit(hop);
		stretch.forget();
		assert_eq!(
			stretch.next(bridge(8000)),
			0,
			"without history there is nowhere to repeat from"
		);
	}

	#[test]
	fn a_discard_finishes_a_crossfade_and_holds_past_it() {
		let mut stretch = Stretch::new(8000, 1);
		let input = tone(stretch.window + stretch.hop);
		let (window, hop) = (stretch.window, stretch.hop);
		stretch.spare(window).copy_from_slice(&input[..window]);
		stretch.commit(window);
		stretch.next(HOLD);
		stretch.read(&mut vec![0.0; hop]);
		stretch.spare(hop).copy_from_slice(&input[window..]);
		stretch.commit(hop);
		assert_eq!(stretch.next(bridge(8000)), -80);
		let available = stretch.available();
		assert_eq!(stretch.discard(hop / 2), hop / 2);
		assert_eq!(stretch.available(), available - hop / 2);
		// Past the crossfade and across the next boundary, to the end of what it holds.
		assert_eq!(stretch.discard(10 * hop), available - hop / 2);
		assert_eq!(stretch.available(), 0);
	}

	#[test]
	fn plays_without_allocating() {
		let mut stretch = Stretch::new(48_000, 2);
		let input = vec![0.25; stretch.window * 2];
		let mut output = vec![0.0; 2 * stretch.hop];
		let (_, allocations) = crate::allocation::measure(|| {
			let window = stretch.window;
			stretch.spare(window).copy_from_slice(&input);
			stretch.commit(window);
			stretch.next(Decision {
				pace: 0.1,
				low: 240,
				high: 480,
				urgent: true,
			});
			stretch.read(&mut output);
		});
		assert_eq!(allocations.operations, 0);
	}
}
