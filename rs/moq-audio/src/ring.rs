//! The queue between a PCM writer and a reader running on its own clock.
//!
//! The writer resamples to the reader's rate on the way in, so the queue holds
//! reader-rate frames and both its size and what it admits are counted in them.
//! It opens primed with silence to its latency, can pad back up to it when the
//! writer falls behind, and can trim back down to it when the writer runs ahead.
//! Native playback instead enables received-audio stretch, which plays the
//! queue onto the writer's clock (see [`Producer::due`]) and skips audio that
//! trails it too far onto it; an actual underrun refills with emergency silence,
//! which never enters the pitch search, until one window is queued again. A
//! clock with a zero stretch turns the stretch off: the queue plays as written,
//! and a reader more than a window off the clock skips onto it or pads silence
//! until the clock reaches it.
//! A silence the writer declared is not an underrun: the reader fills it with
//! comfort noise, and the next utterance plays one latency after it resumes.
//! Neither end allocates, locks, or blocks after construction, so either may
//! run on an audio callback.

// The bus alone never flushes a ring or reads its depth; playback does, and the
// tests below cover both in every build.
#![cfg_attr(
	not(feature = "playback"),
	allow(dead_code, reason = "only playback flushes a ring or reads its depth")
)]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;

use ringbuf::traits::{Consumer as _, Observer as _, Producer as _, Split as _};
use ringbuf::{HeapCons, HeapProd, HeapRb};
use rubato::audioadapter_buffers::direct::InterleavedSlice;
use rubato::{Async, FixedAsync, PolynomialDegree, Resampler as _};

/// The most input frames resampled at once.
pub(crate) const CHUNK: usize = 512;

/// An instant on the host clock. The reader runs on the device thread, outside
/// any runtime, so both ends count time on the clock its callbacks play on.
#[expect(clippy::disallowed_types, reason = "the device thread runs on the host clock")]
pub(crate) type Instant = std::time::Instant;

/// The host clock's current instant.
#[expect(clippy::disallowed_methods, reason = "the device thread runs on the host clock")]
pub(crate) fn host() -> Instant {
	Instant::now()
}

/// How a queue is sized and corrected, in time at the reader's rate.
pub(crate) struct Config {
	/// Audio held between a push and the read that returns it.
	pub latency: Duration,
	/// The most audio held before pushes are dropped.
	pub capacity: Duration,
	/// Pad with silence back up to the latency once the queue falls to a
	/// quarter of it.
	pub pad: bool,
	/// Trim back down to the latency once the queue fills three quarters of the
	/// room above it.
	pub trim: bool,
	/// Measure what plays, so a declared silence fills with comfort noise at
	/// its level. Without it a declared silence is plain silence, for a writer
	/// that never declares one.
	pub comfort: bool,
}

/// What a [`Producer::push`] did with its input.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Push {
	/// Every frame was queued.
	Ok,
	/// The reader has not read yet, so nothing was queued.
	NotReady,
	/// The queue filled, so only the first `accepted` input frames were queued.
	Overflow { accepted: usize },
	/// Every frame was queued, behind `frames` of silence padding an underflow.
	Padded { frames: usize },
}

/// What a [`Consumer::read`] returned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Read {
	/// The output was filled.
	Ok,
	/// The writer has not pushed yet, so the output is silence.
	NotReady,
	/// Only the first `frames` were queued; the rest of the output is silence.
	Underflow { frames: usize },
	/// Only the first `frames` were queued; the rest is a declared silence,
	/// filled with comfort noise.
	Comfort { frames: usize },
	/// The output was filled, then `frames` were discarded to trim the queue.
	Trimmed { frames: usize },
}

/// Which ends have started, so neither queues or plays audio for the other
/// before it exists.
#[derive(Default)]
struct Shared {
	pushed: AtomicBool,
	read: AtomicBool,
	/// Effective input frames played or discarded, including stretch's length
	/// correction. One publication avoids sampling the ring and staging buffer
	/// on opposite sides of a window transfer.
	consumed: AtomicUsize,
	/// Frames of the writer's stream the reader has played or skipped:
	/// `consumed` before the stretch's length correction.
	played: AtomicUsize,
	/// The writer's end less `played`, at its latest push. Adding the reader's
	/// `played` less `consumed` gives the depth that push left, less what the
	/// stretch removed since: the level the stretch decides on.
	level: AtomicUsize,
	/// The writer's clock, once [`clocked`](Self::clocked): the writer's end
	/// less the frames from [`Producer::base`] to when it is due, as an `i32`
	/// in the low half, and how far the reader may trail it before it skips
	/// ahead in the high half, zero turning the stretch off. One word, so the reader never pairs one anchor's
	/// clock with another's bound. Adding the frames to now less `consumed` to
	/// the clock gives how far the reader trails it.
	anchor: AtomicU64,
	clocked: AtomicBool,
	/// Frames the writer's clock jumped ahead by moving its reference earlier,
	/// since the reader last looked. See [`Producer::due`].
	jumped: AtomicUsize,
	target: AtomicUsize,
	finished: AtomicBool,
	stretch: AtomicBool,
	/// Declared ends, counted. See [`Producer::end`].
	ends: AtomicU64,
	/// The resume the reader has yet to reach, [`PENDING`] set while there is one,
	/// with the count of ends reaching it clears in the other bits. See
	/// [`Producer::next`].
	resume: AtomicU64,
	/// Where that resume starts, in written frames, and the comfort noise it
	/// plays there before the next utterance. Written only while none is pending.
	resume_at: AtomicUsize,
	resume_frames: AtomicUsize,
}

/// Set in [`Shared::resume`] while a resume waits for the reader.
const PENDING: u64 = 1 << 63;

/// A queue resampling `input_rate` to `output_rate`, interleaved at `channels`.
///
/// # Panics
///
/// If either rate or `channels` is zero.
pub(crate) fn channel(channels: usize, input_rate: u32, output_rate: u32, config: Config) -> (Producer, Consumer) {
	assert!(channels > 0 && input_rate > 0 && output_rate > 0);

	let convert = (input_rate != output_rate).then(|| Box::new(Convert::new(channels, input_rate, output_rate)));

	// A zero latency primes nothing and keeps no depth, for a writer whose clock
	// already carries its own.
	let latency = if config.latency.is_zero() {
		0
	} else {
		let latency = ((config.latency.as_secs_f64() * output_rate as f64).round() as usize).max(1);
		// What the resampler holds back already delays the output, so the queue
		// primes only the remainder.
		let delay = convert.as_ref().map_or(0, |convert| convert.resampler.output_delay());
		latency.checked_sub(delay).filter(|&latency| latency > 0).unwrap_or(1)
	};
	// Below 50 Hz a chunk is one input frame, up to a second of output, so the
	// room above the latency must hold one chunk to admit it at all, and two so
	// that admitting one does not trigger a trim.
	let chunk = convert
		.as_ref()
		.map_or(0, |convert| convert.resampler.output_frames_max());
	let capacity = ((config.capacity.as_secs_f64() * output_rate as f64).round() as usize)
		.max(2 * latency)
		.max(latency + 2 * chunk);

	let (mut prod, cons) = HeapRb::<f32>::new(capacity * channels).split();
	prod.push_iter(std::iter::repeat_n(0.0, latency * channels));

	let shared = Arc::new(Shared {
		target: AtomicUsize::new(latency),
		level: AtomicUsize::new(latency),
		..Default::default()
	});
	let base = host();
	let producer = Producer {
		ring: prod,
		channels,
		rate: output_rate,
		base,
		convert,
		latency: config.latency,
		written: latency,
		pad: config.pad.then(|| latency * channels / 4),
		declared: false,
		next: false,
		shared: shared.clone(),
	};
	let consumer = Consumer {
		ring: cons,
		channels,
		rate: output_rate,
		base,
		now: 0,
		played: 0,
		stretch: None,
		staged_prime: false,
		refill: false,
		prime: latency,
		demand: 0,
		consumed: 0,
		position: 0,
		cleared: 0,
		cushioned: 0,
		comfort: crate::comfort::Comfort::new(output_rate, channels),
		measure: config.comfort,
		trim: config.trim,
		jumped: 0,
		padding: false,
		joined: false,
		skips: 0,
		trimmed: 0,
		accelerated: 0,
		shared,
	};
	(producer, consumer)
}

/// The writing end of a queue.
pub(crate) struct Producer {
	ring: HeapProd<f32>,
	channels: usize,
	/// The reader's rate, which every queued frame is at.
	rate: u32,
	/// Where both ends count time from.
	base: Instant,
	/// Boxed, since the resampler state dwarfs the rest and a producer moves
	/// through the mixer's command channel by value.
	convert: Option<Box<Convert>>,
	/// The requested duration, preserved across device rate changes.
	latency: Duration,
	/// Output-rate frames admitted, including the initial cushion. Together
	/// with consumed this is a wrapping position, not a pair of queue snapshots.
	written: usize,
	/// Pad back up to the latency at or below this many samples.
	pad: Option<usize>,
	/// An end was declared and no next utterance pushed since.
	declared: bool,
	/// The next push starts the next utterance. See [`next`](Self::next).
	next: bool,
	shared: Arc<Shared>,
}

impl Producer {
	/// Declare that what was pushed so far is followed by a silence the writer
	/// announced. Once it has played, the reader fills with comfort noise rather
	/// than underflowing, until the utterance [`next`](Self::next) starts plays.
	/// Pushes before then continue the audio before the silence.
	pub(crate) fn end(&mut self) {
		self.declared = true;
		self.shared.ends.fetch_add(1, Ordering::Release);
	}

	/// Start the next utterance with the next push. It plays one latency after
	/// that push, behind whatever is still queued, with comfort noise filling the
	/// wait, as a fresh queue's first push would behind its primed silence.
	pub(crate) fn next(&mut self) {
		self.next = true;
	}

	/// Carry a declared silence over from the queue this one replaces.
	pub(crate) fn carry(&mut self, from: &Producer) {
		if from.declared {
			self.end();
		}
		self.next = from.next;
	}

	/// Queue the comfort noise that starts the next utterance one latency out,
	/// once the reader reaches what is queued now.
	fn resume(&mut self) {
		// The resampler's tail belongs to the audio before the silence.
		if let Some(convert) = &mut self.convert {
			let before = convert.emitted;
			convert.flush(&mut self.ring);
			self.written = self.written.wrapping_add((convert.emitted - before) as usize);
		}
		self.declared = false;
		let queued = self.written.wrapping_sub(self.shared.consumed.load(Ordering::Relaxed));
		let frames = self.shared.target.load(Ordering::Relaxed).saturating_sub(queued);
		let ends = self.shared.ends.load(Ordering::Relaxed);
		let mut state = self.shared.resume.load(Ordering::Acquire);
		while state & PENDING != 0 {
			// The reader has yet to reach the previous resume, so two declared
			// silences fall within one latency. That resume clears this end too,
			// and it already topped the queue up to the latency, so only a target
			// raised since leaves anything to wait, in plain silence: a resume's
			// place is published only while none is pending.
			match self
				.shared
				.resume
				.compare_exchange(state, PENDING | ends, Ordering::AcqRel, Ordering::Acquire)
			{
				Ok(_) => {
					let samples = self.ring.push_iter(std::iter::repeat_n(0.0, frames * self.channels));
					self.written = self.written.wrapping_add(samples / self.channels);
					return;
				}
				Err(current) => state = current,
			}
		}
		self.shared.resume_at.store(self.written, Ordering::Relaxed);
		self.shared.resume_frames.store(frames, Ordering::Relaxed);
		self.shared.resume.store(PENDING | ends, Ordering::Release);
		self.written = self.written.wrapping_add(frames);
	}

	/// Queue interleaved input, resampling it on the way in.
	///
	/// Input frames are admitted only while their output has room, so a full
	/// queue drops the newest audio rather than overwriting what is queued.
	pub(crate) fn push(&mut self, input: &[f32]) -> Push {
		self.shared.pushed.store(true, Ordering::Relaxed);
		if !self.shared.read.load(Ordering::Relaxed) {
			return Push::NotReady;
		}
		let push = self.queue(input);
		let level = self.head().wrapping_sub(self.shared.played.load(Ordering::Relaxed));
		self.shared.level.store(level, Ordering::Relaxed);
		push
	}

	fn queue(&mut self, input: &[f32]) -> Push {
		if input.len() >= self.channels && std::mem::take(&mut self.next) {
			self.resume();
		}

		let frames = input.len() / self.channels;
		let input = &input[..frames * self.channels];
		let accepted = match &mut self.convert {
			Some(convert) => {
				let before = convert.emitted;
				let accepted = convert.push(&mut self.ring, input);
				self.written = self.written.wrapping_add((convert.emitted - before) as usize);
				accepted
			}
			None => {
				let accepted = frames.min(self.ring.vacant_len() / self.channels);
				self.ring.push_slice(&input[..accepted * self.channels]);
				self.written = self.written.wrapping_add(accepted);
				accepted
			}
		};
		if accepted < frames {
			return Push::Overflow { accepted };
		}

		if let Some(pad) = self.pad.filter(|_| !self.shared.stretch.load(Ordering::Relaxed)) {
			let queued = self.ring.occupied_len();
			let target = self.shared.target.load(Ordering::Relaxed) * self.channels;
			if queued <= pad && queued < target {
				let samples = self.ring.push_iter(std::iter::repeat_n(0.0, target - queued));
				self.written = self.written.wrapping_add(samples / self.channels);
				return Push::Padded {
					frames: samples / self.channels,
				};
			}
		}

		Push::Ok
	}

	/// Where the audio pushed so far ends, in frames: what was queued, and what
	/// the resampler still owes for input it holds.
	fn head(&self) -> usize {
		let owed = self.convert.as_ref().map_or(0, |convert| convert.owed());
		self.written.wrapping_add(owed)
	}

	/// Anchor what was pushed so far to the writer's clock: its end is due to
	/// play at `at`. The stretch then plays the queue onto that clock, and the
	/// reader skips audio trailing it by more than `stretch` onto it. A zero
	/// `stretch` turns the stretch off.
	/// `jumped` is how far the clock's reference moved earlier since the last
	/// anchor, which the reader skips too, as far as it trails the clock.
	/// Without an anchor, the latency is the clock.
	pub(crate) fn due(&mut self, at: Instant, stretch: Duration, jumped: Duration) {
		let frames = |duration: Duration| (duration.as_secs_f64() * self.rate as f64).round() as usize;
		let at = match at.checked_duration_since(self.base) {
			Some(since) => frames(since),
			None => frames(self.base - at).wrapping_neg(),
		};
		self.due_frame(at, frames(stretch), frames(jumped));
	}

	/// [`due`](Self::due) on a clock counted in frames, wrapping, for a writer
	/// whose reader passes the same clock to [`Consumer::read_at_frame`].
	pub(crate) fn due_frame(&mut self, at: usize, stretch: usize, jumped: usize) {
		// Within a few seconds of the writer's end, unless the caller's clock is
		// off by hours, which saturates rather than wraps.
		let clock = (self.head().wrapping_sub(at) as isize).clamp(i32::MIN as isize, i32::MAX as isize) as i32;
		let stretch = u32::try_from(stretch).unwrap_or(u32::MAX);
		// Released, so a reader that sees it also sees the target set before it.
		self.shared
			.anchor
			.store(u64::from(stretch) << 32 | u64::from(clock as u32), Ordering::Release);
		self.shared.clocked.store(true, Ordering::Release);
		// After the anchor, so a reader that sees the jump also sees the clock it moved.
		self.shared.jumped.fetch_add(jumped, Ordering::Release);
	}

	/// Convert silence until the resampler has emitted the full duration of
	/// everything pushed so far, returning false if the queue filled first.
	///
	/// The resampler only converts whole chunks and its filter lags its input,
	/// so the end of a stream otherwise sits inside it until more input arrives.
	pub(crate) fn flush(&mut self) -> bool {
		let flushed = match &mut self.convert {
			Some(convert) if self.shared.read.load(Ordering::Relaxed) => {
				let before = convert.emitted;
				let flushed = convert.flush(&mut self.ring);
				self.written = self.written.wrapping_add((convert.emitted - before) as usize);
				flushed
			}
			_ => true,
		};
		self.shared.finished.store(true, Ordering::Relaxed);
		flushed
	}

	/// Change the target without reallocating the queue. Leave one stretch window
	/// of headroom so the callback can still receive new PCM at the target.
	pub(crate) fn set_latency(&mut self, latency: Duration) -> bool {
		let frames = (latency.as_secs_f64() * self.rate as f64).round() as usize;
		if frames == 0 {
			return false;
		}
		let delay = self
			.convert
			.as_ref()
			.map_or(0, |convert| convert.resampler.output_delay());
		let frames = frames.saturating_sub(delay).max(1);
		let window = self.rate as usize * 30 / 1000;
		if frames == 0 || frames + window >= self.ring.capacity().get() / self.channels {
			return false;
		}
		self.shared.target.store(frames, Ordering::Relaxed);
		self.latency = latency;
		true
	}

	pub(crate) fn latency(&self) -> Duration {
		self.latency
	}

	/// Frames at the reader's rate the queue still has room for.
	pub(crate) fn vacant(&self) -> usize {
		self.ring.vacant_len() / self.channels
	}

	/// How much audio is queued ahead of the reader.
	pub(crate) fn buffered(&self) -> Duration {
		Duration::from_secs_f64(
			self.written.wrapping_sub(self.shared.consumed.load(Ordering::Relaxed)) as f64 / self.rate as f64,
		)
	}
}

/// Converts whole chunks of input to the reader's rate.
struct Convert {
	resampler: Async<f32>,
	channels: usize,
	/// Input frames per conversion.
	chunk: usize,
	/// One chunk of interleaved input, filled across pushes.
	input: Vec<f32>,
	/// Frames of `input` filled so far.
	pending: usize,
	/// Room for the most interleaved output one chunk produces.
	output: Vec<f32>,
	input_rate: u32,
	output_rate: u32,
	/// Input frames admitted and output frames emitted, which together say how
	/// much output the input is still owed.
	fed: u64,
	emitted: u64,
}

impl Convert {
	fn new(channels: usize, input_rate: u32, output_rate: u32) -> Self {
		// At most 20 ms of input, so one chunk's output stays bounded by the
		// reader's rate however low the input's rate is.
		let chunk = (input_rate as usize / 50).clamp(1, CHUNK);
		let resampler = Async::new_poly(
			output_rate as f64 / input_rate as f64,
			1.0,
			PolynomialDegree::Cubic,
			chunk,
			channels,
			FixedAsync::Input,
		)
		.expect("nonzero rates and chunk");
		let output = vec![0.0; resampler.output_frames_max() * channels];

		Self {
			resampler,
			channels,
			chunk,
			input: vec![0.0; chunk * channels],
			pending: 0,
			output,
			input_rate,
			output_rate,
			fed: 0,
			emitted: 0,
		}
	}

	/// Buffer `input` and convert each chunk it completes, returning how many
	/// input frames were admitted.
	///
	/// A chunk is completed only when the ring has room for exactly what
	/// converting it will produce, so admission never outruns the ring in
	/// either direction of conversion.
	fn push(&mut self, ring: &mut HeapProd<f32>, input: &[f32]) -> usize {
		let channels = self.channels;
		let frames = input.len() / channels;
		let mut accepted = 0;
		while accepted < frames {
			let take = (self.chunk - self.pending).min(frames - accepted);
			if self.pending + take == self.chunk && ring.vacant_len() < self.resampler.output_frames_next() * channels {
				break;
			}

			self.input[self.pending * channels..(self.pending + take) * channels]
				.copy_from_slice(&input[accepted * channels..(accepted + take) * channels]);
			self.pending += take;
			accepted += take;

			if self.pending == self.chunk {
				self.convert(ring);
			}
		}
		self.fed += accepted as u64;
		accepted
	}

	/// Output frames the input admitted so far is still owed, the filter's lag
	/// included.
	fn owed(&self) -> usize {
		let owed = (self.fed as u128 * self.output_rate as u128 / self.input_rate as u128) as u64
			+ self.resampler.output_delay() as u64;
		owed.saturating_sub(self.emitted) as usize
	}

	/// Pad with silence a chunk at a time until the output covers the filter's
	/// lag and the input's whole duration, returning false if the ring filled
	/// first.
	fn flush(&mut self, ring: &mut HeapProd<f32>) -> bool {
		// A stream that never started owes no tail, not even the filter's lag.
		if self.fed == 0 {
			return true;
		}

		let owed = (self.fed as u128 * self.output_rate as u128).div_ceil(self.input_rate as u128)
			+ self.resampler.output_delay() as u128;
		while (self.emitted as u128) < owed {
			if ring.vacant_len() < self.resampler.output_frames_next() * self.channels {
				return false;
			}
			self.input[self.pending * self.channels..].fill(0.0);
			// The padding is input too, so a later flush, after the next utterance,
			// owes that utterance's tail rather than counting this padding against it.
			self.fed += (self.chunk - self.pending) as u64;
			self.convert(ring);
		}
		true
	}

	fn convert(&mut self, ring: &mut HeapProd<f32>) {
		let channels = self.channels;
		let input = InterleavedSlice::new(&self.input, channels, self.chunk).expect("input sized to one chunk");
		let room = self.output.len() / channels;
		let mut output =
			InterleavedSlice::new_mut(&mut self.output, channels, room).expect("output sized to the largest chunk");
		let (_, frames) = self
			.resampler
			.process_into_buffer(&input, &mut output, None)
			.expect("buffers sized at construction");

		let pushed = ring.push_slice(&self.output[..frames * channels]);
		debug_assert_eq!(pushed, frames * channels, "admission reserved room for the chunk");
		self.emitted += frames as u64;
		self.pending = 0;
	}
}

/// The reading end of a queue.
pub(crate) struct Consumer {
	ring: HeapCons<f32>,
	channels: usize,
	rate: u32,
	/// Where both ends count time from.
	base: Instant,
	/// Frames from `base` to the start of the current read.
	now: usize,
	/// See [`Shared::played`].
	played: usize,
	stretch: Option<Box<crate::stretch::Stretch>>,
	/// Whether the window staged in the stretcher is startup cushion.
	staged_prime: bool,
	/// An actual underrun waits for one window of received PCM and the read
	/// that plays it, emitting emergency silence directly rather than putting
	/// padding through the pitch search.
	refill: bool,
	/// The startup cushion is silence, not received audio to stretch.
	prime: usize,
	/// Frames left in the device's callback as of the current read.
	demand: usize,
	consumed: usize,
	/// Frames taken off the ring plus the comfort noise of each resume played
	/// out: the writer's `written`, as far as the reader has reached.
	position: usize,
	/// The count of declared ends the last resume reached clears.
	cleared: u64,
	/// Comfort noise already played at the pending resume.
	cushioned: usize,
	comfort: crate::comfort::Comfort,
	/// Whether the comfort noise measures what plays; unmeasured, it fills plain silence.
	measure: bool,
	/// Without stretch, trim only after filling three quarters of the current
	/// target's headroom. Stretch skips by the clock instead.
	trim: bool,
	/// Frames the writer's clock jumped ahead by moving its reference earlier,
	/// not yet skipped, as far as the queue holds audio past its target, which
	/// the early media that moved it leaves, and the reader still trails the
	/// clock.
	jumped: usize,
	/// Whether the last read padded silence toward the clock, which continues
	/// until the clock reaches the reader.
	padding: bool,
	/// Whether received audio has played on the clock. Until it has, nothing
	/// trailing the clock was heard, so a join lands on it rather than at the
	/// bound.
	joined: bool,
	/// Times [`Read::Trimmed`] was returned, ever.
	skips: u64,
	/// Received frames [`Read::Trimmed`] discarded, ever.
	trimmed: u64,
	/// Frames the stretch removed by accelerating, ever.
	accelerated: u64,
	shared: Arc<Shared>,
}

impl Consumer {
	/// Allocate the native playout stretcher before handing this ring to a device.
	pub(crate) fn stretch(&mut self, rate: u32) {
		self.stretch = Some(Box::new(crate::stretch::Stretch::new(rate, self.channels)));
		self.shared.stretch.store(true, Ordering::Relaxed);
	}

	/// Fill `output` with queued interleaved frames, and silence past them.
	pub(crate) fn read(&mut self, output: &mut [f32]) -> Read {
		let demand = output.len() / self.channels;
		self.read_at(output, host(), demand)
	}

	/// [`read`](Self::read), starting at `now`, with `demand` frames left in
	/// the device's callback from there, which may outlast `output`: a refill
	/// after an underrun waits until they are queued.
	pub(crate) fn read_at(&mut self, output: &mut [f32], now: Instant, demand: usize) -> Read {
		let now = (now.saturating_duration_since(self.base).as_secs_f64() * self.rate as f64).round() as usize;
		self.read_at_frame(output, now, demand)
	}

	/// [`read_at`](Self::read_at) on the frame clock [`Producer::due_frame`] counts.
	pub(crate) fn read_at_frame(&mut self, output: &mut [f32], now: usize, demand: usize) -> Read {
		self.start();
		self.demand = demand;
		self.now = now;
		let whole = output.len() - output.len() % self.channels;
		output[whole..].fill(0.0);
		if !self.shared.pushed.load(Ordering::Relaxed) {
			// A silence declared before any audio, as for a listener joining a muted talker.
			if self.declared() {
				self.comfort.fill(&mut output[..whole]);
				return Read::Comfort { frames: 0 };
			}
			output.fill(0.0);
			return Read::NotReady;
		}

		let skipped = self.skip();
		let padded = self.pad(&mut output[..whole]);
		let read = padded + self.fill(&mut output[padded..whole]);
		if read < whole {
			if self.declared() {
				self.comfort.fill(&mut output[read..whole]);
				return Read::Comfort {
					frames: read / self.channels,
				};
			}
			output[read..whole].fill(0.0);
			self.refill = self.stretch.is_some();
			return Read::Underflow {
				frames: read / self.channels,
			};
		}

		if skipped > 0 {
			return Read::Trimmed { frames: skipped };
		}

		if self.trim && self.stretch.is_none() {
			let queued = self.available();
			let latency = self.shared.target.load(Ordering::Relaxed);
			let capacity = self.ring.capacity().get() / self.channels;
			let trim = latency + (capacity - latency) * 3 / 4;
			if queued >= trim && queued > latency {
				let frames = queued - latency;
				self.skips += 1;
				self.trimmed += self.discard(frames) as u64;
				return Read::Trimmed { frames };
			}
		}

		Read::Ok
	}

	/// What a stretch window starting `elapsed` frames into this read decides on.
	fn inputs(&self, elapsed: usize) -> crate::stretch::Inputs {
		// The anchor before the target, so a new anchor brings the target set
		// before it.
		let anchor = self
			.shared
			.clocked
			.load(Ordering::Acquire)
			.then(|| self.shared.anchor.load(Ordering::Acquire));
		let target = self.shared.target.load(Ordering::Relaxed);
		let level = self
			.shared
			.level
			.load(Ordering::Relaxed)
			.wrapping_add(self.played)
			.wrapping_sub(self.consumed) as isize;
		let (lag, bound) = if let Some(anchor) = anchor {
			let clock = anchor as u32 as i32 as isize as usize;
			let lag = clock.wrapping_add(self.now + elapsed).wrapping_sub(self.consumed) as isize;
			(lag, (anchor >> 32) as usize)
		} else {
			// Without a clock, the latency is where the reader belongs, and
			// only an excess past three quarters of the headroom is skipped.
			let capacity = self.ring.capacity().get() / self.channels;
			(level - target as isize, capacity.saturating_sub(target) * 3 / 4)
		};
		crate::stretch::Inputs {
			target,
			level: level.max(0) as f64,
			raw: self.available(),
			lag: lag as i64,
			bound: bound as i64,
		}
	}

	/// Skip what trails the clock by more than its bound, or one window when
	/// that is larger, onto the clock, or the jumps of its reference past one
	/// window, returning
	/// the frames skipped. Less than that is the stretch's to catch up, as a
	/// writer's clock drifting fast moves the reference. A join has the bound
	/// of nothing heard yet, so it lands on the clock past whatever backlog it
	/// was handed, and a jump before it was never heard. A finished or silenced
	/// stream plays as written.
	fn skip(&mut self) -> usize {
		// Acquired before the anchor, so a jump comes with the clock it moved.
		let jumped = self.shared.jumped.swap(0, Ordering::Acquire);
		let Some(window) = self.stretch.as_ref().map(|stretch| stretch.window) else {
			return 0;
		};
		if self.shared.finished.load(Ordering::Relaxed) || self.declared() {
			return 0;
		}
		let inputs = self.inputs(0);
		let clocked = self.shared.clocked.load(Ordering::Acquire);
		let bound = if self.joined || !clocked { inputs.bound } else { 0 };
		self.jumped = if self.joined {
			// Never more than the audio held past the target, so a skip never
			// drains the queue below it.
			let excess = inputs.raw.saturating_sub(inputs.target);
			(self.jumped + jumped).min(excess).min(inputs.lag.max(0) as usize)
		} else {
			0
		};
		self.joined |= clocked && self.prime == 0;
		// Onto the clock, or without one back to the bound.
		let late = if inputs.lag > bound.max(window as i64) {
			(if clocked { inputs.lag } else { inputs.lag - bound }) as usize
		} else {
			0
		};
		let skip = if self.jumped > window {
			late.max(self.jumped)
		} else {
			late
		};
		if skip == 0 {
			return 0;
		}
		let available = self.available();
		let pushed = self.discard(skip);
		let skipped = available - self.available();
		// What a skip leaves of a jump, as past what is queued, skips once it
		// arrives rather than staying inside the bound for the stretch.
		self.jumped = self.jumped.saturating_sub(skipped);
		if skipped > 0 {
			self.skips += 1;
			self.trimmed += pushed as u64;
		}
		skipped
	}

	/// Whether the stretch may alter what plays: always, until a clock with a
	/// zero stretch turns it off.
	fn stretching(&self) -> bool {
		!self.shared.clocked.load(Ordering::Acquire) || self.shared.anchor.load(Ordering::Acquire) >> 32 != 0
	}

	/// With the stretch off, pad silence ahead of the queue once the reader is
	/// more than a window ahead of the clock, until the clock reaches it,
	/// returning the samples padded. The stretch would slow down instead.
	fn pad(&mut self, output: &mut [f32]) -> usize {
		let Some(window) = self.stretch.as_ref().map(|stretch| stretch.window) else {
			return 0;
		};
		let lead = -self.inputs(0).lag;
		let off =
			!self.stretching() && !self.refill && !self.shared.finished.load(Ordering::Relaxed) && !self.declared();
		self.padding = off && lead > if self.padding { 0 } else { window as i64 };
		if !self.padding {
			return 0;
		}
		let samples = (lead as usize).min(output.len() / self.channels) * self.channels;
		output[..samples].fill(0.0);
		samples
	}

	/// Whether running dry now is a silence the writer declared: an end it has
	/// not resumed from, on a queue that is not playing out its last samples.
	fn declared(&self) -> bool {
		self.shared.ends.load(Ordering::Acquire) != self.cleared && !self.shared.finished.load(Ordering::Relaxed)
	}

	/// The pending resume, as the frames queued ahead of it and its comfort noise
	/// still to play.
	///
	/// Read the ring's length before calling this: the writer publishes a resume
	/// before pushing what follows it, so a length that includes those frames
	/// guarantees the resume is visible too.
	fn resume(&self) -> Option<(usize, usize)> {
		if self.shared.resume.load(Ordering::Acquire) & PENDING == 0 {
			return None;
		}
		let at = self.shared.resume_at.load(Ordering::Relaxed);
		let frames = self.shared.resume_frames.load(Ordering::Relaxed);
		Some((at.wrapping_sub(self.position), frames - self.cushioned))
	}

	/// Play up to `frames` of the pending resume's comfort noise into `output`,
	/// or skip them when it is `None`, and clear its ends once it is done.
	fn cushion(&mut self, output: Option<&mut [f32]>, frames: usize) -> usize {
		let left = self.shared.resume_frames.load(Ordering::Relaxed) - self.cushioned;
		let frames = frames.min(left);
		if let Some(output) = output {
			self.comfort.fill(&mut output[..frames * self.channels]);
			self.played = self.played.wrapping_add(frames);
		}
		self.cushioned += frames;
		self.consumed = self.consumed.wrapping_add(frames);
		if frames == left {
			self.position = self.position.wrapping_add(self.cushioned);
			self.cushioned = 0;
			self.cleared = self.shared.resume.swap(0, Ordering::AcqRel) & !PENDING;
		}
		frames
	}

	/// Fill `output` from the queue, returning the samples written.
	fn fill(&mut self, output: &mut [f32]) -> usize {
		if self.refill && !self.shared.finished.load(Ordering::Relaxed) {
			// One window for the stretch and what is left of the device's
			// callback: the least that resumes without running dry again at
			// once, and never more than the start waits for. Expanding then
			// rebuilds the level rather than this wait.
			let window = self.stretch.as_ref().map_or(0, |stretch| stretch.window);
			let cushion = (window + self.demand).min(self.shared.target.load(Ordering::Relaxed));
			// Nothing more comes before a declared silence, so its tail plays now.
			if !self.declared() && self.available() < cushion {
				return 0;
			}
			self.refill = false;
			if let Some(stretch) = &mut self.stretch {
				stretch.level.reset();
			}
		}
		let channels = self.channels;
		let mut done = 0;
		while done < output.len() {
			if let Some(stretch) = self.stretch.as_mut().filter(|stretch| stretch.available() > 0) {
				let frames = stretch.read(&mut output[done..]);
				if self.measure && !self.staged_prime {
					self.comfort.played(&output[done..done + frames * channels]);
				}
				done += frames * channels;
				self.consumed = self.consumed.wrapping_add(frames);
				self.played = self.played.wrapping_add(frames);
				continue;
			}

			let mut frames = self.ring.occupied_len() / channels;
			if let Some((ahead, _)) = self.resume() {
				if ahead == 0 {
					let room = (output.len() - done) / channels;
					done += self.cushion(Some(&mut output[done..]), room) * channels;
					continue;
				}
				frames = frames.min(ahead);
			}
			// A declared end's tail drains as received: running dry after it is
			// silence, not an underrun, so nothing should expand toward the target.
			let declared = self.declared();
			let Some(window) = self.stretch.as_ref().map(|stretch| stretch.window) else {
				let frames = frames.min((output.len() - done) / channels);
				if frames == 0 {
					break;
				}
				let read = self.ring.pop_slice(&mut output[done..done + frames * channels]);
				debug_assert_eq!(read, frames * channels);
				let prime = frames.min(self.prime);
				self.prime -= prime;
				if self.measure {
					self.comfort.played(&output[done + prime * channels..done + read]);
				}
				self.position = self.position.wrapping_add(frames);
				self.consumed = self.consumed.wrapping_add(frames);
				self.played = self.played.wrapping_add(frames);
				done += read;
				continue;
			};
			let frames = frames.min(window);
			if frames == 0 {
				break;
			}
			// Never transform startup padding, a finite stream's tail, or anything with the stretch off.
			let enabled =
				self.prime == 0 && !self.shared.finished.load(Ordering::Relaxed) && !declared && self.stretching();
			let inputs = (enabled && frames == window).then(|| self.inputs(done / channels));
			let stretch = self.stretch.as_mut().expect("a window comes from the stretch");
			let frames = if self.prime > 0 { frames.min(self.prime) } else { frames };
			self.staged_prime = self.prime > 0;
			self.prime -= frames.min(self.prime);
			let read = self.ring.pop_slice(&mut stretch.input[..frames * channels]);
			debug_assert_eq!(read, frames * channels);
			self.position = self.position.wrapping_add(frames);
			stretch.process(frames, inputs);
			let removed = frames.saturating_sub(stretch.available());
			self.accelerated += removed as u64;
			// The writer still counts the input while it is being analysed.
			// Publish only the resulting duration change, then the rendered
			// frames. Moving PCM into staging never makes it disappear.
			self.consumed = self.consumed.wrapping_add(frames).wrapping_sub(stretch.available());
		}
		self.shared.consumed.store(self.consumed, Ordering::Relaxed);
		self.shared.played.store(self.played, Ordering::Relaxed);
		done
	}

	/// Frames of comfort noise read so far, ever.
	pub(crate) fn comfort(&self) -> u64 {
		self.comfort.filled()
	}

	/// Times the queue was trimmed back to its latency, ever.
	pub(crate) fn skips(&self) -> u64 {
		self.skips
	}

	/// Pushed frames discarded to trim the queue back to its latency, ever: not
	/// the startup silence or a resume's comfort noise, though silence padding
	/// a writer pushed itself, or [`Push::Padded`] added, counts.
	pub(crate) fn trimmed(&self) -> u64 {
		self.trimmed
	}

	/// Frames of received audio the stretch removed by accelerating, ever.
	pub(crate) fn accelerated(&self) -> u64 {
		self.accelerated
	}

	/// Accept pushes without reading first, as a read would.
	pub(crate) fn start(&self) {
		self.shared.read.store(true, Ordering::Relaxed);
	}

	/// Frames played or skipped so far, on the frame clock
	/// [`read_at_frame`](Self::read_at_frame) counts: a writer that maps its
	/// own clock onto this one lines `now` up with it while nothing is queued.
	pub(crate) fn consumed(&self) -> usize {
		self.consumed
	}

	/// Frames ready to read, or zero before the writer's first push.
	pub(crate) fn available(&self) -> usize {
		if !self.shared.pushed.load(Ordering::Relaxed) {
			return 0;
		}
		let buffered = self.ring.occupied_len() / self.channels;
		let cushion = self.resume().map_or(0, |(_, frames)| frames);
		buffered + cushion + self.stretch.as_ref().map_or(0, |stretch| stretch.available())
	}

	/// Drop up to `frames` queued frames unread, returning how many of them were
	/// pushed rather than the startup silence or a resume's comfort noise.
	pub(crate) fn discard(&mut self, frames: usize) -> usize {
		// Skipped audio leaves `played` behind, so the level drops with it.
		let staged = self.stretch.as_mut().map_or(0, |stretch| stretch.discard(frames));
		self.consumed = self.consumed.wrapping_add(staged);
		let mut pushed = if self.staged_prime { 0 } else { staged };
		let mut left = frames - staged;
		while left > 0 {
			let mut take = left;
			if let Some((ahead, _)) = self.resume() {
				if ahead == 0 {
					left -= self.cushion(None, left);
					continue;
				}
				take = take.min(ahead);
			}
			let skipped = self.ring.skip(take * self.channels) / self.channels;
			if skipped == 0 {
				break;
			}
			let prime = skipped.min(self.prime);
			self.prime -= prime;
			pushed += skipped - prime;
			self.position = self.position.wrapping_add(skipped);
			self.consumed = self.consumed.wrapping_add(skipped);
			left -= skipped;
		}
		self.shared.consumed.store(self.consumed, Ordering::Relaxed);
		self.shared.played.store(self.played, Ordering::Relaxed);
		pushed
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	const STEREO: usize = 2;

	/// A read whose device callback is `out` alone.
	fn pull(cons: &mut Consumer, out: &mut [f32], now: Instant) -> Read {
		let frames = out.len() / STEREO;
		cons.read_at(out, now, frames)
	}

	fn queue(input_rate: u32, output_rate: u32, capacity: Duration) -> (Producer, Consumer) {
		let (prod, cons) = channel(
			STEREO,
			input_rate,
			output_rate,
			Config {
				latency: Duration::from_millis(50),
				capacity,
				pad: false,
				trim: false,
				comfort: true,
			},
		);
		cons.start();
		(prod, cons)
	}

	/// Output frames the next chunk needs, or one frame when nothing converts.
	fn next_chunk(prod: &Producer) -> usize {
		prod.convert
			.as_ref()
			.map_or(1, |convert| convert.resampler.output_frames_next())
	}

	/// A write far larger than the queue fills it as far as whole chunks fit and
	/// drops the rest, whichever way the rate converts. Counting free output as
	/// input frames gets this wrong one way or the other.
	#[test]
	fn a_large_write_fills_the_queue_in_either_direction() {
		for (input_rate, output_rate) in [(44_100, 48_000), (48_000, 44_100), (8_000, 48_000), (96_000, 48_000)] {
			let (mut prod, _cons) = queue(input_rate, output_rate, Duration::from_secs(1));
			let frames = input_rate as usize * 4;
			let push = prod.push(&vec![0.25; frames * STEREO]);
			let Push::Overflow { accepted } = push else {
				panic!("{input_rate}->{output_rate}: {push:?}");
			};

			let room = prod.ring.vacant_len() / STEREO;
			assert!(
				room < next_chunk(&prod),
				"{input_rate}->{output_rate}: {room} frames left free"
			);
			// The queue holds a second of output, so it admits about a second of
			// input, whatever the input's rate.
			let admitted = accepted as f64 / input_rate as f64;
			assert!(
				(0.9..=1.0).contains(&admitted),
				"{input_rate}->{output_rate}: {admitted} s"
			);
		}
	}

	/// Rates a catalog can declare, far from the reader's, still convert:
	/// one input frame can be a second of output, and a second of output can
	/// take millions of input frames.
	#[test]
	fn extreme_ratios_convert_in_either_direction() {
		for (input_rate, frames) in [(1, 8), (49, 200), (4_000_000_000, 4_000_000)] {
			let (mut prod, mut cons) = queue(input_rate, 48_000, Duration::from_secs(20));
			assert_eq!(prod.push(&vec![0.25; frames * STEREO]), Push::Ok, "{input_rate} Hz");
			assert!(prod.flush(), "{input_rate} Hz");

			// Count frames past half amplitude, where the resampled edges cross,
			// so the count is the input's duration at the output rate.
			let want = frames as u64 * 48_000 / input_rate as u64;
			let mut out = vec![0.0; cons.available() * STEREO];
			assert_eq!(cons.read(&mut out), Read::Ok);
			let heard = out
				.as_chunks::<STEREO>()
				.0
				.iter()
				.filter(|frame| frame[0] > 0.125)
				.count() as u64;
			assert!(
				heard.abs_diff(want) <= 1,
				"{input_rate} Hz: heard {heard} of {want} frames"
			);
		}
	}

	/// A queue shorter than one chunk's output grows to admit it, rather than
	/// dropping every chunk of an input too slow to fill one in 20 ms.
	#[test]
	fn a_chunk_longer_than_the_capacity_is_heard() {
		let (mut prod, mut cons) = channel(
			STEREO,
			1,
			48_000,
			Config {
				latency: Duration::from_millis(50),
				capacity: Duration::from_millis(200),
				pad: false,
				trim: true,
				comfort: true,
			},
		);
		cons.start();
		// The cubic filter lags its input by a few frames, which at 1 Hz is
		// seconds, so only the last second is past it.
		let mut out = vec![0.0; 48_000 * STEREO];
		for second in 0..6 {
			assert_eq!(prod.push(&[0.25; STEREO]), Push::Ok, "second {second}");
			let read = cons.read(&mut out);
			assert!(!matches!(read, Read::Trimmed { .. }), "second {second}: {read:?}");
		}
		assert!(out.iter().all(|&sample| sample > 0.125), "the last second is silent");
	}

	/// Writes before the reader starts are dropped rather than played late, and
	/// a reader sees silence until the writer starts.
	#[test]
	fn neither_end_queues_for_an_absent_other() {
		let (mut prod, mut cons) = channel(
			STEREO,
			48_000,
			48_000,
			Config {
				latency: Duration::from_millis(10),
				capacity: Duration::from_millis(100),
				pad: false,
				trim: false,
				comfort: true,
			},
		);
		assert_eq!(prod.push(&[0.5; 2 * STEREO]), Push::NotReady);
		let mut out = [1.0; 4 * STEREO];
		assert_eq!(cons.read(&mut out), Read::Ok);
		assert_eq!(out, [0.0; 4 * STEREO], "primed with silence");

		let (mut prod, mut cons) = queue(48_000, 48_000, Duration::from_millis(100));
		assert_eq!(cons.available(), 0);
		assert_eq!(cons.read(&mut out), Read::NotReady);
		assert_eq!(prod.push(&[0.5; 2 * STEREO]), Push::Ok);
		assert_eq!(cons.available(), 2400 + 2);
	}

	/// Padding refills a queue that ran low to its latency, and trimming drains
	/// one that ran high back down to it.
	#[test]
	fn corrections_return_to_the_latency() {
		let (mut prod, mut cons) = channel(
			STEREO,
			48_000,
			48_000,
			Config {
				latency: Duration::from_millis(10),
				capacity: Duration::from_millis(100),
				pad: true,
				trim: true,
				comfort: true,
			},
		);
		cons.start();
		assert_eq!(prod.push(&[]), Push::Ok);
		let mut out = vec![0.0; 400 * STEREO];
		assert_eq!(cons.read(&mut out), Read::Ok);
		assert_eq!(prod.push(&[0.5; STEREO]), Push::Padded { frames: 480 - 81 });
		assert_eq!(cons.available(), 480);

		assert_eq!(prod.push(&vec![0.5; 3800 * STEREO]), Push::Ok);
		assert_eq!(cons.read(&mut out), Read::Trimmed { frames: 3880 - 480 });
		assert_eq!(cons.trimmed(), 3880 - 480);
		assert_eq!(cons.skips(), 1);
		assert_eq!(cons.available(), 480);
	}

	/// A trim that reaches into the startup silence counts only what was pushed.
	#[test]
	fn a_trim_counts_no_startup_silence() {
		let (mut prod, mut cons) = channel(
			STEREO,
			48_000,
			48_000,
			Config {
				latency: Duration::from_millis(10),
				capacity: Duration::from_millis(100),
				pad: false,
				trim: true,
				comfort: true,
			},
		);
		cons.start();
		assert_eq!(prod.push(&vec![0.5; 4000 * STEREO]), Push::Ok);
		let mut out = vec![0.0; 128 * STEREO];
		// 352 of the 480 startup frames are left behind the 128 read.
		assert_eq!(cons.read(&mut out), Read::Trimmed { frames: 4352 - 480 });
		assert_eq!(cons.trimmed(), 4352 - 480 - 352);
	}

	/// Construction allocates for the reader's rate and the queue's channels,
	/// never for a declared input rate.
	#[test]
	fn allocation_follows_the_output_rate() {
		let second = 48_000 * STEREO * size_of::<f32>();
		let mut baseline = None;
		for input_rate in [44_100, 1, 49, 8_000, 96_000, 4_000_000_000] {
			let (_, stats) = crate::allocation::measure(|| queue(input_rate, 48_000, Duration::from_secs(1)));
			let baseline = *baseline.get_or_insert(stats.bytes);
			// One chunk is at most 20 ms of input, or one frame below 50 Hz, which
			// is at most a second of output: held once to convert into, and twice
			// in the room the queue keeps for it.
			assert!(
				stats.bytes <= baseline + 3 * second,
				"{input_rate} Hz allocated {} bytes, baseline {baseline}",
				stats.bytes
			);
		}
	}

	/// A stretching queue fed 20 ms of a 200 Hz tone every `interval`
	/// microseconds against a 10 ms device period.
	fn tone(rate: u32, latency: Duration) -> (Producer, Consumer, Vec<f32>) {
		let (prod, mut cons) = channel(
			2,
			rate,
			rate,
			Config {
				latency,
				capacity: Duration::from_secs(3),
				pad: false,
				trim: false,
				comfort: true,
			},
		);
		cons.stretch(rate);
		cons.start();
		let mut pcm = vec![0.0; rate as usize / 50 * 2];
		for (i, frame) in pcm.as_chunks_mut::<2>().0.iter_mut().enumerate() {
			frame.fill((i as f32 * std::f32::consts::TAU * 200.0 / rate as f32).sin() * 0.25);
		}
		(prod, cons, pcm)
	}

	/// Packets on a clock that drifts from the device's, each due one target
	/// after it arrives. The queue plays onto that clock, in either direction
	/// of drift, and onto it again after the target rises (slowing down rather
	/// than parking) or falls (speeding up rather than skipping). Without a
	/// clock, the target alone is where it settles.
	#[test]
	fn stretch_plays_onto_the_clock_through_drift_and_target_changes() {
		for clocked in [true, false] {
			for rate in [8_000, 44_100, 48_000, 96_000] {
				for interval in [19_960u64, 20_040] {
					let (mut prod, mut cons, pcm) = tone(rate, Duration::from_millis(80));
					let base = prod.base;
					let mut out = vec![0.0; rate as usize / 100 * 2];
					let mut arrival = 0;
					let mut target = Duration::from_millis(80);
					for tick in 0..9000u64 {
						if tick == 3000 || tick == 6000 {
							target = Duration::from_millis(if tick == 3000 { 120 } else { 60 });
							assert!(prod.set_latency(target));
						}
						while arrival <= tick * 10_000 {
							assert_eq!(prod.push(&pcm), Push::Ok);
							if clocked {
								prod.due(
									base + Duration::from_micros(arrival) + target,
									Duration::from_secs(2),
									Duration::ZERO,
								);
							}
							arrival += interval;
						}
						let now = base + Duration::from_millis(tick * 10);
						let (status, allocations) = crate::allocation::measure(|| pull(&mut cons, &mut out, now));
						assert_eq!(allocations.operations, 0, "callback allocated");
						assert_eq!(status, Read::Ok, "{rate} Hz, {interval} us, tick {tick}");
						if tick % 3000 > 500 {
							let target = target.as_secs_f64() * 1000.0;
							let buffered = prod.buffered().as_secs_f64() * 1000.0;
							assert!(
								(target - 40.0..=target + 40.0).contains(&buffered),
								"{rate} Hz tick {tick}: {buffered} ms, target {target}"
							);
							if clocked {
								let lag = cons.inputs(0).lag as f64 * 1000.0 / rate as f64;
								assert!(
									lag.abs() <= 20.0,
									"{rate} Hz {interval} us tick {tick}: {lag} ms behind"
								);
							}
						}
					}
					assert_eq!(cons.skips(), 0);
				}
			}
		}
	}

	/// A 300 ms stall runs the queue dry. It resumes on one window and a device
	/// period rather than its target, then slows down to rebuild the target,
	/// falling behind the clock without running dry again. Media that keeps
	/// arriving that late leaves it there; media that arrives in a burst once
	/// the stall clears lets it speed back onto the clock.
	#[test]
	fn a_stall_resumes_early_and_catches_up_once_the_buffer_returns() {
		for burst in [false, true] {
			let rate = 48_000;
			let (mut prod, mut cons, pcm) = tone(rate, Duration::from_millis(80));
			let base = prod.base;
			let target = Duration::from_millis(80);
			let mut out = vec![0.0; 480 * 2];
			let mut underruns = Vec::new();
			let mut lag = 0.0;
			let mut sent = 0;
			for tick in 0..1500u64 {
				let now = tick * 10;
				// Packets sent every 20 ms, each due one target after it is sent.
				// From 1000 ms they arrive 300 ms late, or all at 1300 ms and
				// on time after.
				let arrival = |sent: u64| match sent {
					..1000 => sent,
					_ if burst => sent.max(1300),
					_ => sent + 300,
				};
				while arrival(sent) <= now {
					assert_eq!(prod.push(&pcm), Push::Ok);
					prod.due(
						base + Duration::from_millis(sent) + target,
						Duration::from_secs(2),
						Duration::ZERO,
					);
					sent += 20;
				}
				let read = pull(&mut cons, &mut out, base + Duration::from_millis(now));
				if matches!(read, Read::Underflow { .. }) {
					underruns.push(now);
				}
				lag = cons.inputs(0).lag as f64 * 1000.0 / rate as f64;
			}
			// Running dry once the buffer drains, until a cushion arrives: two
			// packets rather than the four of the target.
			assert!(
				underruns.iter().all(|&at| (1040..1330).contains(&at)),
				"{burst}: {underruns:?}"
			);
			if burst {
				assert!(lag.abs() <= 20.0, "back on the clock, {lag} ms behind");
			} else {
				assert!(
					(250.0..1000.0).contains(&lag),
					"settled behind the clock, {lag} ms behind"
				);
			}
			// Rebuilt into NetEq's band: from three quarters of the target to it.
			let level = cons.inputs(0).level * 1000.0 / rate as f64;
			assert!((60.0..=100.0).contains(&level), "{burst}: rebuilt to {level} ms");
			assert_eq!(cons.skips(), 0);
		}
	}

	/// Audio trailing the clock by more than its bound skips onto it; less than
	/// that is the stretch's to catch up.
	#[test]
	fn only_audio_past_the_bound_skips() {
		let rate = 48_000;
		let (mut prod, mut cons, pcm) = tone(rate, Duration::from_millis(10));
		let base = prod.base;
		let mut out = vec![0.0; 480 * 2];
		// Two seconds queued behind the 10 ms prime, played on time through the
		// prime and the first 10 ms of audio, which joins the clock.
		for _ in 0..100 {
			assert_eq!(prod.push(&pcm), Push::Ok);
		}
		let bound = Duration::from_millis(300);
		prod.due(base + Duration::from_millis(2010), bound, Duration::ZERO);
		assert_eq!(pull(&mut cons, &mut out, base), Read::Ok);
		assert_eq!(pull(&mut cons, &mut out, base + Duration::from_millis(10)), Read::Ok);
		// Now due 500 ms earlier: the reader trails by 500 ms.
		let due = base + Duration::from_millis(1510);
		prod.due(due, Duration::from_millis(510), Duration::ZERO);
		assert_eq!(pull(&mut cons, &mut out, base + Duration::from_millis(20)), Read::Ok);
		assert_eq!(cons.skips(), 0, "within the bound is the stretch's");
		prod.due(due, Duration::from_millis(490), Duration::ZERO);
		let read = pull(&mut cons, &mut out, base + Duration::from_millis(30));
		let Read::Trimmed { frames } = read else {
			panic!("{read:?}");
		};
		// Onto the clock, not back to the bound.
		assert!((23_520..=24_480).contains(&frames), "{frames} frames trimmed");
		assert!(cons.inputs(0).lag.abs() <= 480, "lag {}", cons.inputs(0).lag);
		// A zero stretch has no bound: whatever trails the clock goes, here
		// what is due before the second the end is due at.
		prod.due(base + Duration::from_secs(1), Duration::ZERO, Duration::ZERO);
		let read = pull(&mut cons, &mut out, base + Duration::from_millis(40));
		assert!(matches!(read, Read::Trimmed { .. }), "{read:?}");
		assert!(cons.inputs(0).lag <= 0);
		assert_eq!(cons.skips(), 2);
		let left = cons.available() as f64 * 1000.0 / rate as f64;
		assert!((940.0..960.0).contains(&left), "{left} ms left");
	}

	/// A zero stretch plays what was written unaltered: a lag under a window
	/// that the stretch would speed through stays, more skips onto the clock,
	/// and a reader more than a window ahead of it pads silence until the clock
	/// reaches it.
	#[test]
	fn a_zero_stretch_skips_or_pads_onto_the_clock() {
		let rate = 48_000;
		let (mut prod, mut cons, pcm) = tone(rate, Duration::from_millis(10));
		let base = prod.base;
		let mut out = vec![0.0; 480 * 2];
		for _ in 0..100 {
			assert_eq!(prod.push(&pcm), Push::Ok);
		}
		// Two seconds queued, trailing the clock by 20 ms: deep enough for the
		// stretch to speed through, short of a window.
		prod.due(base + Duration::from_millis(1990), Duration::ZERO, Duration::ZERO);
		let mut heard = Vec::new();
		let mut now = base;
		for _ in 0..50 {
			assert_eq!(pull(&mut cons, &mut out, now), Read::Ok);
			heard.extend_from_slice(&out);
			now += Duration::from_millis(10);
		}
		assert_eq!(cons.accelerated(), 0);
		assert_eq!(cons.skips(), 0);
		// The 10 ms prime, then the tone exactly as written.
		assert!(heard[..480 * 2].iter().all(|sample| *sample == 0.0));
		let written = pcm.iter().cycle();
		assert!(
			heard[480 * 2..]
				.iter()
				.zip(written)
				.all(|(heard, written)| heard == written)
		);

		// 100 ms more behind the clock skips onto it, the 20 ms included.
		prod.due(base + Duration::from_millis(1890), Duration::ZERO, Duration::ZERO);
		assert_eq!(pull(&mut cons, &mut out, now), Read::Trimmed { frames: 5760 });
		now += Duration::from_millis(10);

		// 50 ms ahead of it pads silence until the clock reaches the reader.
		prod.due(base + Duration::from_millis(1940), Duration::ZERO, Duration::ZERO);
		let available = cons.available();
		for _ in 0..5 {
			assert_eq!(pull(&mut cons, &mut out, now), Read::Ok);
			assert!(out.iter().all(|sample| *sample == 0.0));
			now += Duration::from_millis(10);
		}
		assert_eq!(cons.available(), available, "padding plays nothing written");
		assert_eq!(pull(&mut cons, &mut out, now), Read::Ok);
		assert!(out.iter().any(|sample| *sample != 0.0));
		assert_eq!((cons.accelerated(), cons.skips()), (0, 1));
	}

	/// A clock whose reference moved earlier skips as much, as far as the
	/// reader trails it, where the same lag from a fallen target is caught up
	/// by speeding up.
	#[test]
	fn a_reference_jump_skips_as_much() {
		for jumped in [false, true] {
			let rate = 48_000;
			let (mut prod, mut cons, pcm) = tone(rate, Duration::from_millis(200));
			let base = prod.base;
			let mut out = vec![0.0; 480 * 2];
			for _ in 0..100 {
				assert_eq!(prod.push(&pcm), Push::Ok);
			}
			// On time, through the 200 ms prime, which joins the clock.
			let late = Duration::from_secs(2);
			prod.due(base + Duration::from_millis(2200), late, Duration::ZERO);
			let mut now = base;
			for _ in 0..25 {
				assert_eq!(pull(&mut cons, &mut out, now), Read::Ok);
				now += Duration::from_millis(10);
			}
			// The clock moves 100 ms ahead: its reference, or a fall in the target.
			let at = base + Duration::from_millis(2100);
			if jumped {
				prod.due(at, late, Duration::from_millis(150));
			} else {
				assert!(prod.set_latency(Duration::from_millis(100)));
				prod.due(at, late, Duration::ZERO);
			}
			let read = pull(&mut cons, &mut out, now);
			let lag = cons.inputs(0).lag as f64 * 1000.0 / rate as f64;
			if jumped {
				// Never past the clock, however far the reference moved.
				assert!(matches!(read, Read::Trimmed { .. }), "{read:?}");
				assert!((-20.0..=5.0).contains(&lag), "{lag} ms behind");
			} else {
				assert_eq!(read, Read::Ok);
				assert!(lag >= 90.0, "{lag} ms behind");
				for _ in 0..50 {
					now += Duration::from_millis(10);
					assert_eq!(pull(&mut cons, &mut out, now), Read::Ok);
				}
				assert_eq!(cons.skips(), 0);
				assert!(cons.accelerated() > 0);
			}
		}
	}

	/// A jump counts only as far as the queue holds audio past its target,
	/// which the early media that moved the reference leaves it, so one on a
	/// queue running low is the stretch's to catch up, inside the bound.
	#[test]
	fn a_jump_skips_only_audio_held_past_the_target() {
		for deep in [false, true] {
			let rate = 48_000;
			let (mut prod, mut cons, pcm) = tone(rate, Duration::from_millis(80));
			let base = prod.base;
			let late = Duration::from_secs(2);
			let mut out = vec![0.0; 480 * 2];
			let packets = if deep { 50 } else { 5 };
			for _ in 0..packets {
				assert_eq!(prod.push(&pcm), Push::Ok);
			}
			// On time through the 80 ms prime, which joins the clock.
			let end = Duration::from_millis(80 + 20 * packets);
			prod.due(base + end, late, Duration::ZERO);
			let mut now = base;
			for _ in 0..9 {
				assert!(!matches!(pull(&mut cons, &mut out, now), Read::Trimmed { .. }));
				now += Duration::from_millis(10);
			}
			// The reference jumps 50 ms.
			let shift = Duration::from_millis(50);
			prod.due(base + end - shift, late, shift);
			let read = pull(&mut cons, &mut out, now);
			let lag = cons.inputs(0).lag as f64 * 1000.0 / rate as f64;
			if deep {
				assert!(matches!(read, Read::Trimmed { .. }), "{read:?}");
				assert!((-20.0..=5.0).contains(&lag), "{lag} ms behind");
			} else {
				assert_eq!(cons.skips(), 0, "{read:?}");
				assert!(lag >= 30.0, "{lag} ms behind");
			}
		}
	}

	/// Pending jumps never add up past the audio held over the target, so a
	/// skip never drains the queue below it.
	#[test]
	fn a_pending_jump_never_skips_below_the_target() {
		let rate = 48_000;
		let (mut prod, mut cons, pcm) = tone(rate, Duration::from_millis(20));
		let base = prod.base;
		let late = Duration::from_secs(2);
		let mut out = vec![0.0; 480 * 2];
		for _ in 0..5 {
			assert_eq!(prod.push(&pcm), Push::Ok);
		}
		// On time through the 20 ms prime, which joins the clock.
		let end = Duration::from_millis(120);
		prod.due(base + end, late, Duration::ZERO);
		let mut now = base;
		for _ in 0..3 {
			assert!(!matches!(pull(&mut cons, &mut out, now), Read::Trimmed { .. }));
			now += Duration::from_millis(10);
		}
		// 25 ms pending, short of a window.
		let mut shift = Duration::from_millis(25);
		prod.due(base + end - shift, late, shift);
		assert!(!matches!(pull(&mut cons, &mut out, now), Read::Trimmed { .. }));
		now += Duration::from_millis(10);
		// Drained to about 35 ms held, 15 ms past the target, then 15 ms more.
		while cons.available() > rate as usize * 45 / 1000 {
			assert!(!matches!(pull(&mut cons, &mut out, now), Read::Trimmed { .. }));
			now += Duration::from_millis(10);
		}
		shift += Duration::from_millis(15);
		prod.due(base + end - shift, late, Duration::from_millis(15));
		let read = pull(&mut cons, &mut out, now);
		assert!(
			!matches!(read, Read::Trimmed { .. } | Read::Underflow { .. }),
			"{read:?}"
		);
		assert_eq!(cons.skips(), 0);
	}

	/// A refill after an underrun waits for the whole device callback, not
	/// the slice of it one read fills.
	#[test]
	fn a_refill_waits_for_the_whole_callback() {
		let rate = 48_000;
		let (mut prod, mut cons, pcm) = tone(rate, Duration::from_millis(100));
		let mut out = vec![0.0; 1024 * 2];
		let callback = 4096;
		assert_eq!(prod.push(&pcm), Push::Ok);
		let now = host();
		while !matches!(cons.read_at(&mut out, now, callback), Read::Underflow { .. }) {}
		// 60 ms covers one window and a slice, but not the 85 ms callback.
		for _ in 0..3 {
			assert_eq!(prod.push(&pcm), Push::Ok);
		}
		assert_eq!(cons.read_at(&mut out, now, callback), Read::Underflow { frames: 0 });
		for _ in 0..2 {
			assert_eq!(prod.push(&pcm), Push::Ok);
		}
		assert_eq!(cons.read_at(&mut out, now, callback), Read::Ok);
	}

	/// Nothing trailing the clock was heard before the first audio plays on
	/// it, so a join lands on the clock past a backlog the bound would keep.
	#[test]
	fn a_join_lands_on_the_clock() {
		let rate = 48_000;
		let (mut prod, mut cons, pcm) = tone(rate, Duration::from_millis(10));
		let base = prod.base;
		let mut out = vec![0.0; 480 * 2];
		for _ in 0..100 {
			assert_eq!(prod.push(&pcm), Push::Ok);
		}
		// All due at 10 ms, inside a 1970 ms bound.
		prod.due(
			base + Duration::from_millis(10),
			Duration::from_millis(1970),
			Duration::ZERO,
		);
		let read = pull(&mut cons, &mut out, base);
		assert!(matches!(read, Read::Trimmed { .. }), "{read:?}");
		assert_eq!(cons.skips(), 1);
		assert!(cons.inputs(0).lag <= 0);
	}

	#[test]
	fn staged_audio_is_buffered_and_finish_drains_it() {
		let (mut prod, mut cons) = channel(
			2,
			48_000,
			48_000,
			Config {
				latency: Duration::from_millis(50),
				capacity: Duration::from_secs(1),
				pad: false,
				trim: false,
				comfort: true,
			},
		);
		cons.stretch(48_000);
		cons.start();
		assert_eq!(prod.push(&vec![0.25; 4800 * 2]), Push::Ok);
		let mut out = [0.0; 128 * 2];
		assert_eq!(cons.read(&mut out), Read::Ok);
		assert_eq!(
			prod.buffered(),
			Duration::from_secs_f64(cons.available() as f64 / 48_000.0)
		);
		assert!(prod.flush());
		let mut frames = 0;
		while cons.available() > 0 {
			let before = cons.available();
			cons.read(&mut out);
			frames += before - cons.available();
		}
		assert_eq!(frames, 7200 - 128, "finish never extends the tail");
		assert_eq!(cons.read(&mut out), Read::Underflow { frames: 0 });
	}

	#[test]
	fn a_declared_end_drains_its_tail_unstretched() {
		let (mut prod, mut cons) = channel(
			2,
			48_000,
			48_000,
			Config {
				latency: Duration::from_millis(50),
				capacity: Duration::from_secs(1),
				pad: false,
				trim: false,
				comfort: true,
			},
		);
		cons.stretch(48_000);
		cons.start();
		// Twice the target, so the tail drains through it: without the end it
		// would expand there, toward a target nothing will refill.
		assert_eq!(prod.push(&vec![0.25; 4800 * 2]), Push::Ok);
		prod.end();
		let mut out = [0.0; 128 * 2];
		let mut reads = 0;
		while cons.available() > 0 {
			cons.read(&mut out);
			reads += 1;
		}
		assert_eq!(reads, 7200usize.div_ceil(128), "a declared end never extends the tail");
	}

	#[test]
	fn recovery_waits_for_received_pcm_without_stretching_padding() {
		let (mut prod, mut cons) = channel(
			2,
			48_000,
			48_000,
			Config {
				latency: Duration::from_millis(50),
				capacity: Duration::from_secs(1),
				pad: true,
				trim: false,
				comfort: true,
			},
		);
		cons.stretch(48_000);
		cons.start();
		assert_eq!(prod.push(&[]), Push::Ok);
		let mut out = vec![1.0; 4800 * 2];
		assert_eq!(cons.read(&mut out), Read::Underflow { frames: 2400 });
		// One 30 ms window and the 128-frame read, not the 50 ms target: 1568 frames.
		assert_eq!(
			prod.push(&[0.25; 1567 * 2]),
			Push::Ok,
			"no padding inserted in the ring"
		);
		assert_eq!(cons.read(&mut out[..128 * 2]), Read::Underflow { frames: 0 });
		assert!(out[..128 * 2].iter().all(|&sample| sample == 0.0));
		assert_eq!(prod.push(&[0.25; 2]), Push::Ok);
		assert_eq!(cons.read(&mut out[..128 * 2]), Read::Ok);
		assert!(out[..128 * 2].iter().all(|&sample| sample == 0.25));
		// A final short input must not wait forever to reach the cushion.
		assert!(prod.flush());
		while cons.available() > 0 {
			cons.read(&mut out);
		}
		assert_eq!(cons.read(&mut out), Read::Underflow { frames: 0 });
	}

	#[test]
	fn a_target_near_capacity_still_has_emergency_headroom() {
		let (mut prod, mut cons) = channel(
			2,
			48_000,
			48_000,
			Config {
				latency: Duration::from_millis(50),
				capacity: Duration::from_millis(3050),
				pad: false,
				trim: true,
				comfort: true,
			},
		);
		cons.stretch(48_000);
		cons.start();
		assert!(prod.set_latency(Duration::from_secs(3)));
		// A 200 Hz tone, so the seam has a pitch period to remove.
		let mut pcm = vec![0.0; 146_600 * 2];
		for (i, frame) in pcm.as_chunks_mut::<2>().0.iter_mut().enumerate() {
			frame.fill((i as f32 * std::f32::consts::TAU * 200.0 / 48_000.0).sin() * 0.25);
		}
		let (first, rest) = pcm.split_at(143_000 * 2);
		let (cushion, second) = rest.split_at(1_000 * 2);
		// Play out the 50 ms prime below the trim watermark, then top the queue
		// up to the target so its first window, inside the 60 ms startup
		// cooldown, holds rather than expands. End on a window boundary.
		assert_eq!(prod.push(first), Push::Ok);
		let mut out = [0.0; 480 * 2];
		for _ in 0..5 {
			assert_eq!(cons.read(&mut out), Read::Ok);
		}
		assert_eq!(prod.push(cushion), Push::Ok);
		for _ in 0..3 {
			assert_eq!(cons.read(&mut out), Read::Ok);
		}
		// Above NetEq's high limit (target + 20 ms) but under the trim
		// watermark (three quarters of the 50 ms headroom).
		assert_eq!(prod.push(second), Push::Ok);
		let target = 144_000;
		let queued = cons.available();
		assert!((target + 960..target + 1800).contains(&queued), "{queued}");
		assert_eq!(
			cons.read(&mut out),
			Read::Ok,
			"ordinary target overshoot must stretch, not trim"
		);
		let removed = queued - cons.available() - 480;
		assert!(
			(120..=720).contains(&removed),
			"accelerate removes one pitch period, removed {removed}"
		);
		assert_eq!(cons.accelerated(), removed as u64, "the shed is counted");
		assert_eq!(cons.trimmed(), 0, "and is no trim");
		assert_eq!(
			prod.buffered(),
			Duration::from_secs_f64(cons.available() as f64 / 48_000.0)
		);
	}

	/// The plain queue's 50 ms, in frames.
	const LATENCY: usize = 2400;

	/// A queue that has played its primed silence and all but `tail` frames of a
	/// quiet background, then declared an end.
	fn ended(tail: usize) -> (Producer, Consumer) {
		let (mut prod, mut cons) = queue(48_000, 48_000, Duration::from_secs(1));
		assert_eq!(prod.push(&[0.0002; 4800 * STEREO]), Push::Ok);
		let mut out = vec![0.0; (LATENCY + 4800 - tail) * STEREO];
		assert_eq!(cons.read(&mut out), Read::Ok);
		prod.end();
		(prod, cons)
	}

	fn noise(samples: &[f32]) -> bool {
		samples
			.iter()
			.all(|&sample| sample != 0.0 && sample.abs() <= 0.0002 * 3f32.sqrt() * 1.0001)
	}

	/// A declared end plays comfort noise once the queue runs dry, and the next
	/// utterance plays one latency after its first push.
	#[test]
	fn a_declared_silence_is_comfort_noise_until_the_next_utterance() {
		let (mut prod, mut cons) = ended(0);
		let mut out = vec![0.0; 1000 * STEREO];
		assert_eq!(cons.read(&mut out), Read::Comfort { frames: 0 });
		assert!(noise(&out));

		prod.next();
		assert_eq!(prod.push(&[0.5; 960 * STEREO]), Push::Ok);
		let mut out = vec![0.0; (LATENCY + 960) * STEREO];
		assert_eq!(cons.read(&mut out), Read::Ok);
		assert!(noise(&out[..LATENCY * STEREO]));
		assert!(out[LATENCY * STEREO..].iter().all(|&sample| sample == 0.5));
		// Running dry after the next utterance is an underflow again.
		assert_eq!(cons.read(&mut out), Read::Underflow { frames: 0 });
	}

	/// A queue that measures nothing fills a declared silence with plain silence.
	#[test]
	fn an_unmeasured_declared_silence_is_silent() {
		let (mut prod, mut cons) = channel(
			STEREO,
			48_000,
			48_000,
			Config {
				latency: Duration::from_millis(50),
				capacity: Duration::from_secs(1),
				pad: false,
				trim: false,
				comfort: false,
			},
		);
		cons.start();
		assert_eq!(prod.push(&[0.0002; 4800 * STEREO]), Push::Ok);
		let mut out = vec![0.0; (LATENCY + 4800) * STEREO];
		assert_eq!(cons.read(&mut out), Read::Ok);
		prod.end();
		let mut out = vec![1.0; 1000 * STEREO];
		assert_eq!(cons.read(&mut out), Read::Comfort { frames: 0 });
		assert!(out.iter().all(|&sample| sample == 0.0));
	}

	/// A next utterance pushed while the end's tail still plays waits behind it,
	/// with comfort noise topping the wait up to one latency.
	#[test]
	fn a_resume_behind_a_queued_tail_starts_one_latency_out() {
		let (mut prod, mut cons) = ended(1200);
		prod.next();
		assert_eq!(prod.push(&[0.5; 960 * STEREO]), Push::Ok);
		assert_eq!(prod.buffered(), Duration::from_millis(70));
		let mut out = vec![0.0; (LATENCY + 960) * STEREO];
		assert_eq!(cons.read(&mut out), Read::Ok);
		assert!(out[..1200 * STEREO].iter().all(|&sample| sample == 0.0002));
		assert!(noise(&out[1200 * STEREO..LATENCY * STEREO]));
		assert!(out[LATENCY * STEREO..].iter().all(|&sample| sample == 0.5));
	}

	/// A second silence declared before the reader reaches the first resume is
	/// cleared by it: the first already topped the queue up to the latency.
	#[test]
	fn a_second_silence_within_one_latency_clears_with_the_first() {
		let (mut prod, mut cons) = ended(600);
		prod.next();
		assert_eq!(prod.push(&[0.5; 240 * STEREO]), Push::Ok);
		prod.end();
		prod.next();
		assert_eq!(prod.push(&[0.25; 240 * STEREO]), Push::Ok);
		let mut out = vec![0.0; (LATENCY + 480) * STEREO];
		assert_eq!(cons.read(&mut out), Read::Ok);
		assert!(out[(LATENCY + 240) * STEREO..].iter().all(|&sample| sample == 0.25));
		assert_eq!(cons.read(&mut out), Read::Underflow { frames: 0 });
	}

	/// An end before any audio, such as a listener joining a muted talker, is a
	/// declared silence already, with nothing measured to fill it with.
	#[test]
	fn an_end_before_any_audio_is_a_silence() {
		let (mut prod, mut cons) = queue(48_000, 48_000, Duration::from_secs(1));
		prod.end();
		let mut out = vec![1.0; 128 * STEREO];
		assert_eq!(cons.read(&mut out), Read::Comfort { frames: 0 });
		assert!(out.iter().all(|&sample| sample == 0.0));
		prod.next();
		assert_eq!(prod.push(&[0.5; 960 * STEREO]), Push::Ok);
		let mut out = vec![0.0; (LATENCY + 960) * STEREO];
		assert_eq!(cons.read(&mut out), Read::Ok);
		assert!(out[LATENCY * STEREO..].iter().all(|&sample| sample == 0.5));
		assert_eq!(cons.read(&mut out), Read::Underflow { frames: 0 });
	}

	/// A trim across a resume skips its comfort noise like any other queued audio.
	#[test]
	fn a_discard_crosses_a_resume() {
		let (mut prod, mut cons) = ended(1200);
		prod.next();
		assert_eq!(prod.push(&[0.5; 960 * STEREO]), Push::Ok);
		cons.discard(1800);
		assert_eq!(cons.available(), 600 + 960);
		let mut out = vec![0.0; (600 + 960) * STEREO];
		assert_eq!(cons.read(&mut out), Read::Ok);
		assert!(noise(&out[..600 * STEREO]));
		assert!(out[600 * STEREO..].iter().all(|&sample| sample == 0.5));
		assert_eq!(cons.read(&mut out), Read::Underflow { frames: 0 });
	}

	/// Each resume flushes the resampler, and a later flush still owes the next
	/// utterance's whole tail rather than counting the earlier padding against it.
	#[test]
	fn every_utterance_through_the_resampler_is_heard() {
		let (mut prod, mut cons) = queue(44_100, 48_000, Duration::from_secs(1));
		let mut out = vec![0.0; 48_000 * STEREO];
		// The first flush pads nearly a whole chunk, more than the second leaves pending.
		for (sample, frames) in [(0.25f32, CHUNK + 1), (0.5, CHUNK + 100)] {
			prod.end();
			prod.next();
			assert_eq!(prod.push(&vec![sample; frames * STEREO]), Push::Ok);
		}
		assert!(prod.flush());
		let _ = cons.read(&mut out);
		// Past half amplitude, where the resampled edges cross.
		let heard = out.iter().step_by(STEREO).filter(|&&sample| sample > 0.375).count();
		let want = (CHUNK + 100) * 48_000 / 44_100;
		assert!(heard.abs_diff(want) <= 1, "heard {heard}, want {want}");
	}

	/// A tail stuck short of an emergency refill when its end is declared plays
	/// out, rather than waiting through the silence for audio that is not coming.
	#[test]
	fn a_declared_end_releases_an_emergency_refill() {
		let (mut prod, mut cons) = channel(
			STEREO,
			48_000,
			48_000,
			Config {
				latency: Duration::from_millis(50),
				capacity: Duration::from_secs(1),
				pad: true,
				trim: false,
				comfort: true,
			},
		);
		cons.stretch(48_000);
		cons.start();
		assert_eq!(prod.push(&[]), Push::Ok);
		let mut out = vec![0.0; LATENCY * STEREO];
		assert_eq!(cons.read(&mut out), Read::Ok);
		assert_eq!(cons.read(&mut out[..128 * STEREO]), Read::Underflow { frames: 0 });
		assert_eq!(prod.push(&[0.25; 960 * STEREO]), Push::Ok);
		assert_eq!(cons.read(&mut out[..128 * STEREO]), Read::Underflow { frames: 0 });
		prod.end();
		let mut out = vec![0.0; 1000 * STEREO];
		assert_eq!(cons.read(&mut out), Read::Comfort { frames: 960 });
		assert!(out[..960 * STEREO].iter().all(|&sample| sample == 0.25));
	}
}
