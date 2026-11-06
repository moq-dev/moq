//! The queue between a PCM writer and a reader running on its own clock.
//!
//! The writer resamples to the reader's rate on the way in, so the queue holds
//! reader-rate frames and both its size and what it admits are counted in them.
//! It opens primed with silence to its latency, can pad back up to it when the
//! writer falls behind, and can trim back down to it when the writer runs ahead.
//! Native playback instead enables received-audio stretch; only an actual
//! underrun refills with emergency silence, which never enters the pitch search.
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

	let latency = ((config.latency.as_secs_f64() * output_rate as f64).round() as usize).max(1);
	// What the resampler holds back already delays the output, so the queue
	// primes only the remainder.
	let delay = convert.as_ref().map_or(0, |convert| convert.resampler.output_delay());
	let latency = latency.checked_sub(delay).filter(|&latency| latency > 0).unwrap_or(1);
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
		..Default::default()
	});
	let producer = Producer {
		ring: prod,
		channels,
		rate: output_rate,
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
		stretch: None,
		staged_prime: false,
		refill: false,
		prime: latency,
		consumed: 0,
		position: 0,
		cleared: 0,
		cushioned: 0,
		comfort: crate::comfort::Comfort::new(output_rate, channels),
		trim: config.trim,
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
	stretch: Option<Box<crate::stretch::Stretch>>,
	/// Whether the window staged in the stretcher is startup cushion.
	staged_prime: bool,
	/// An actual underrun waits for received PCM, emitting emergency silence
	/// directly rather than putting padding through the pitch search.
	refill: bool,
	/// The startup cushion is silence, not received audio to stretch.
	prime: usize,
	consumed: usize,
	/// Frames taken off the ring plus the comfort noise of each resume played
	/// out: the writer's `written`, as far as the reader has reached.
	position: usize,
	/// The count of declared ends the last resume reached clears.
	cleared: u64,
	/// Comfort noise already played at the pending resume.
	cushioned: usize,
	comfort: crate::comfort::Comfort,
	/// Trim only after filling three quarters of the current target's headroom.
	trim: bool,
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
		self.start();
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

		let read = self.fill(&mut output[..whole]);
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

		if self.trim {
			let queued = self.available();
			let latency = self.shared.target.load(Ordering::Relaxed);
			let capacity = self.ring.capacity().get() / self.channels;
			let trim = latency + (capacity - latency) * 3 / 4;
			if queued >= trim && queued > latency {
				let frames = queued - latency;
				self.discard(frames);
				return Read::Trimmed { frames };
			}
		}

		Read::Ok
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
			// Nothing more comes before a declared silence, so its tail plays now.
			if !self.declared() && self.available() < self.shared.target.load(Ordering::Relaxed) {
				return 0;
			}
			self.refill = false;
		}
		let channels = self.channels;
		let mut done = 0;
		while done < output.len() {
			if let Some(stretch) = self.stretch.as_mut().filter(|stretch| stretch.available() > 0) {
				let frames = stretch.read(&mut output[done..]);
				if !self.staged_prime {
					self.comfort.played(&output[done..done + frames * channels]);
				}
				done += frames * channels;
				self.consumed = self.consumed.wrapping_add(frames);
				continue;
			}

			let buffered = self.ring.occupied_len() / channels;
			let mut frames = buffered;
			if let Some((ahead, _)) = self.resume() {
				if ahead == 0 {
					let room = (output.len() - done) / channels;
					done += self.cushion(Some(&mut output[done..]), room) * channels;
					continue;
				}
				frames = frames.min(ahead);
			}
			let Some(stretch) = self.stretch.as_mut() else {
				let frames = frames.min((output.len() - done) / channels);
				if frames == 0 {
					break;
				}
				let read = self.ring.pop_slice(&mut output[done..done + frames * channels]);
				debug_assert_eq!(read, frames * channels);
				let prime = frames.min(self.prime);
				self.prime -= prime;
				self.comfort.played(&output[done + prime * channels..done + read]);
				self.position = self.position.wrapping_add(frames);
				self.consumed = self.consumed.wrapping_add(frames);
				done += read;
				continue;
			};
			let frames = frames.min(stretch.window);
			if frames == 0 {
				break;
			}
			// Never transform startup padding or a finite stream's tail.
			let enabled = self.prime == 0 && !self.shared.finished.load(Ordering::Relaxed);
			let frames = if self.prime > 0 { frames.min(self.prime) } else { frames };
			self.staged_prime = self.prime > 0;
			self.prime -= frames.min(self.prime);
			let read = self.ring.pop_slice(&mut stretch.input[..frames * channels]);
			debug_assert_eq!(read, frames * channels);
			self.position = self.position.wrapping_add(frames);
			stretch.process(frames, buffered, self.shared.target.load(Ordering::Relaxed), enabled);
			// The writer still counts the input while it is being analysed.
			// Publish only the resulting duration change, then the rendered
			// frames. Moving PCM into staging never makes it disappear.
			self.consumed = self.consumed.wrapping_add(frames).wrapping_sub(stretch.available());
		}
		self.shared.consumed.store(self.consumed, Ordering::Relaxed);
		done
	}

	/// Accept pushes without reading first, as a read would.
	pub(crate) fn start(&self) {
		self.shared.read.store(true, Ordering::Relaxed);
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

	/// Drop up to `frames` queued frames unread.
	pub(crate) fn discard(&mut self, frames: usize) {
		let staged = self.stretch.as_mut().map_or(0, |stretch| stretch.discard(frames));
		self.consumed = self.consumed.wrapping_add(staged);
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
			self.prime = self.prime.saturating_sub(skipped);
			self.position = self.position.wrapping_add(skipped);
			self.consumed = self.consumed.wrapping_add(skipped);
			left -= skipped;
		}
		self.shared.consumed.store(self.consumed, Ordering::Relaxed);
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	const STEREO: usize = 2;

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
		assert_eq!(cons.available(), 480);
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

	/// Clock-controlled packet/device drift, without padding to hide a dropout.
	#[test]
	fn stretch_tracks_both_clock_directions_and_target_changes() {
		for rate in [8_000, 44_100, 48_000, 96_000] {
			for interval in [19_960u64, 20_040] {
				let (mut prod, mut cons) = channel(
					2,
					rate,
					rate,
					Config {
						latency: Duration::from_millis(80),
						capacity: Duration::from_secs(3),
						pad: false,
						trim: false,
					},
				);
				cons.stretch(rate);
				cons.start();
				let mut pcm = vec![0.0; rate as usize / 50 * 2];
				for (i, frame) in pcm.as_chunks_mut::<2>().0.iter_mut().enumerate() {
					frame.fill((i as f32 * std::f32::consts::TAU * 200.0 / rate as f32).sin() * 0.25);
				}
				let mut out = vec![0.0; rate as usize / 100 * 2];
				let mut arrival = 0;
				for tick in 0..9000u64 {
					if tick == 3000 {
						assert!(prod.set_latency(Duration::from_millis(120)));
					}
					if tick == 6000 {
						assert!(prod.set_latency(Duration::from_millis(60)));
					}
					while arrival <= tick * 10_000 {
						assert_eq!(prod.push(&pcm), Push::Ok);
						arrival += interval;
					}
					let (status, allocations) = crate::allocation::measure(|| cons.read(&mut out));
					assert_eq!(allocations.operations, 0, "callback allocated");
					assert_eq!(status, Read::Ok, "{rate} Hz, {interval} us, tick {tick}");
					if tick % 3000 > 500 {
						let target = if tick < 3000 {
							80.0
						} else if tick < 6000 {
							120.0
						} else {
							60.0
						};
						let buffered = prod.buffered().as_secs_f64() * 1000.0;
						assert!(
							(target - 40.0..=target + 40.0).contains(&buffered),
							"{rate} Hz tick {tick}: {buffered} ms, target {target}"
						);
					}
				}
			}
		}
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
			},
		);
		cons.stretch(48_000);
		cons.start();
		assert_eq!(prod.push(&[]), Push::Ok);
		let mut out = vec![1.0; 4800 * 2];
		assert_eq!(cons.read(&mut out), Read::Underflow { frames: 2400 });
		for _ in 0..2 {
			assert_eq!(prod.push(&[0.25; 960 * 2]), Push::Ok, "no padding inserted in the ring");
			assert_eq!(cons.read(&mut out[..128 * 2]), Read::Underflow { frames: 0 });
			assert!(out[..128 * 2].iter().all(|&sample| sample == 0.0));
		}
		assert_eq!(prod.push(&[0.25; 960 * 2]), Push::Ok);
		assert_eq!(cons.read(&mut out[..128 * 2]), Read::Ok);
		assert!(out[..128 * 2].iter().all(|&sample| sample == 0.25));
		// A final short input must not wait forever to reach the refill target.
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
		let (first, second) = pcm.split_at(143_000 * 2);
		// Play out the 50 ms prime and the 60 ms startup cooldown below the
		// trim watermark, ending on a window boundary.
		assert_eq!(prod.push(first), Push::Ok);
		let mut out = [0.0; 480 * 2];
		for _ in 0..8 {
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
