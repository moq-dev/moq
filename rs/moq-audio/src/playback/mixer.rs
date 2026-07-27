//! The output callback: sum every registered sink into one device buffer.
//!
//! Everything here runs on the OS audio thread, so it must not allocate, free,
//! lock, or log. That shapes the whole module:
//!
//! - Buffers and the entry list are sized once, at construction. [`MAX_SINKS`]
//!   is what makes that possible: the driver refuses to register more, so the
//!   entry list never has to grow.
//! - Commands arrive over a bounded channel, whose slots are also allocated up
//!   front, so draining it never touches the allocator.
//! - A removed sink owns a heap-backed `ResamplingCons`, so it is handed back to
//!   the driver to drop rather than dropped here.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError, TrySendError};

use fixed_resample::ResamplingCons;
#[cfg(feature = "aec")]
use fixed_resample::ResamplingProd;

/// Frames mixed per pass. The callback buffer is chunked to this so the scratch
/// buffers stay a fixed size no matter what period the device asks for.
const CHUNK: usize = 1024;

/// How long a gain change takes to apply. Long enough that a step change is
/// inaudible as a click, short enough to feel instant.
const RAMP: f32 = 0.003;

/// The mix bus is stereo: sinks resample into it and it fans out to however many
/// channels the device wants.
pub(crate) const BUS_CHANNELS: usize = 2;

/// Sinks one device will mix. The entry list is allocated to this up front and
/// never grows, which is what keeps registration off the allocator.
///
/// Far more than a call mixes client-side in practice, and a caller that hits it
/// gets an error from [`Engine::sink`](super::Engine::sink) rather than silence.
pub(super) const MAX_SINKS: usize = 64;

/// The volume and level of one sink, shared between the caller and the audio
/// thread.
///
/// Both fields hold an `f32` via [`f32::to_bits`], since there is no atomic
/// float. `Relaxed` throughout: these are independent scalars, not a
/// publication of other memory.
#[derive(Debug)]
pub(super) struct Gain {
	/// Volume the audio thread ramps toward.
	target: AtomicU32,
	/// Loudest sample this sink contributed since the last [`Gain::peak`].
	peak: AtomicU32,
}

impl Gain {
	pub(super) fn new() -> Self {
		Self {
			target: AtomicU32::new(1.0f32.to_bits()),
			peak: AtomicU32::new(0),
		}
	}

	pub(super) fn set_volume(&self, volume: f32) {
		// `clamp` propagates NaN rather than clamping it, and the ramp below
		// would then carry it into `applied`, which never recovers: every sample
		// this sink contributes is NaN for the life of the stream. A non-finite
		// request leaves the volume where it was.
		if !volume.is_finite() {
			return;
		}

		self.target.store(volume.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
	}

	pub(super) fn volume(&self) -> f32 {
		f32::from_bits(self.target.load(Ordering::Relaxed))
	}

	/// The peak since the previous call, resetting it.
	pub(super) fn peak(&self) -> f32 {
		f32::from_bits(self.peak.swap(0, Ordering::Relaxed))
	}

	/// Raise the peak from the audio thread. A plain load/store rather than a
	/// compare-exchange loop: a reader swapping concurrently can lose one update,
	/// which costs a meter one frame of accuracy and is not worth spinning the
	/// realtime thread for.
	fn record(&self, peak: f32) {
		let current = f32::from_bits(self.peak.load(Ordering::Relaxed));
		if peak > current {
			self.peak.store(peak.to_bits(), Ordering::Relaxed);
		}
	}
}

/// Sink registration, sent from the driver to the audio thread.
pub(super) enum Command {
	/// Start mixing a sink.
	Add {
		id: u64,
		cons: ResamplingCons<f32>,
		gain: Arc<Gain>,
	},
	/// Stop mixing a sink, because it was dropped or is being rebuilt for a new
	/// device.
	Remove { id: u64 },
	/// Copy the mix into an echo-cancellation reference, or stop with `None`.
	#[cfg(feature = "aec")]
	Reference(Option<ResamplingProd<f32>>),
}

/// One sink being mixed, owned by the audio thread until it is retired.
pub(super) struct Entry {
	id: u64,
	cons: ResamplingCons<f32>,
	gain: Arc<Gain>,
	/// Gain actually applied, chasing [`Gain::target`] a step at a time.
	applied: f32,
}

/// Something the audio thread is done with and must not free itself.
///
/// Nothing reads these payloads, which is the point: they exist so the driver
/// thread is the one that drops them.
#[allow(dead_code)]
pub(super) enum Retired {
	/// A sink that was removed or rebuilt.
	Sink(Entry),
	/// The echo reference a canceller replaced or gave up.
	#[cfg(feature = "aec")]
	Reference(ResamplingProd<f32>),
}

/// The state behind the output callback.
pub(super) struct Mixer {
	entries: Vec<Entry>,
	commands: Receiver<Command>,
	/// Where anything the mixer is done with goes to be dropped, since dropping
	/// it here would free on the audio thread.
	retired: SyncSender<Retired>,
	/// Channels the device takes.
	channels: usize,
	/// Per-frame gain step, so any change spans [`RAMP`] regardless of rate.
	step: f32,
	/// Stereo accumulator for one chunk.
	bus: Vec<f32>,
	/// Stereo scratch for the sink being read.
	scratch: Vec<f32>,
	/// Where echo cancellation reads what was played. Fed the mix after
	/// clipping but before it fans out, since that is the signal the speaker
	/// gets and therefore the one the microphone hears back.
	#[cfg(feature = "aec")]
	reference: Option<ResamplingProd<f32>>,
}

impl Mixer {
	/// `rate` and `channels` describe the device, not the sinks: each sink
	/// resamples into the bus on its way here.
	pub(super) fn new(commands: Receiver<Command>, retired: SyncSender<Retired>, rate: u32, channels: usize) -> Self {
		Self {
			entries: Vec::with_capacity(MAX_SINKS),
			commands,
			retired,
			channels,
			step: 1.0 / (rate as f32 * RAMP),
			bus: vec![0.0; CHUNK * BUS_CHANNELS],
			scratch: vec![0.0; CHUNK * BUS_CHANNELS],
			#[cfg(feature = "aec")]
			reference: None,
		}
	}

	/// Hand something the mixer is done with back to the driver to drop.
	///
	/// The channel holds [`MAX_SINKS`] entries and the driver drains it every
	/// time a sink is dropped, so filling it takes a driver that has stopped
	/// running entirely. Dropping in place then costs one free on the audio
	/// thread, which beats leaking the entry for the life of the stream.
	fn retire(&mut self, item: Retired) {
		if let Err(err) = self.retired.try_send(item) {
			let (TrySendError::Full(item) | TrySendError::Disconnected(item)) = err;
			drop(item);
		}
	}

	/// Fill one device buffer, interleaved at the device's channel count.
	pub(super) fn fill(&mut self, out: &mut [f32]) {
		loop {
			match self.commands.try_recv() {
				Ok(Command::Add { id, cons, gain }) => {
					// Start silent and ramp up, so a sink joining mid-playback
					// doesn't click.
					let entry = Entry {
						id,
						cons,
						gain,
						applied: 0.0,
					};

					// The driver caps registrations at MAX_SINKS, so this is
					// unreachable. Retire rather than push anyway: growing the
					// list would allocate right here on the audio thread.
					debug_assert!(self.entries.len() < MAX_SINKS, "more sinks than the driver allows");
					if self.entries.len() < MAX_SINKS {
						self.entries.push(entry);
					} else {
						self.retire(Retired::Sink(entry));
					}
				}
				Ok(Command::Remove { id }) => {
					if let Some(index) = self.entries.iter().position(|e| e.id == id) {
						let entry = self.entries.swap_remove(index);
						self.retire(Retired::Sink(entry));
					}
				}
				#[cfg(feature = "aec")]
				Ok(Command::Reference(reference)) => {
					// The old tap owns a heap ring, so it leaves the same way a
					// removed sink does rather than being freed here.
					if let Some(previous) = std::mem::replace(&mut self.reference, reference) {
						self.retire(Retired::Reference(previous));
					}
				}
				Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
			}
		}

		// Split the borrow so the per-sink loop can hold the scratch buffers.
		let Self {
			entries,
			channels,
			step,
			bus,
			scratch,
			#[cfg(feature = "aec")]
			reference,
			..
		} = self;
		let channels = *channels;

		let total = out.len() / channels;
		let mut done = 0;
		while done < total {
			let frames = (total - done).min(CHUNK);
			let samples = frames * BUS_CHANNELS;

			bus[..samples].fill(0.0);

			for entry in entries.iter_mut() {
				// Read even at zero volume: this is live audio, so a muted sink
				// stays on the timeline instead of queueing up and jumping when
				// it unmutes. The status only reports under/overflow, which the
				// channel has already corrected for.
				let _ = entry.cons.read_interleaved(&mut scratch[..samples], false);

				let target = entry.gain.volume();
				let mut applied = entry.applied;
				let mut peak = 0.0f32;

				for frame in 0..frames {
					applied += (target - applied).clamp(-*step, *step);
					let left = scratch[frame * 2] * applied;
					let right = scratch[frame * 2 + 1] * applied;
					peak = peak.max(left.abs()).max(right.abs());
					bus[frame * 2] += left;
					bus[frame * 2 + 1] += right;
				}

				entry.applied = applied;
				entry.gain.record(peak);
			}

			// Summing sinks can exceed full scale; clip rather than wrap.
			for sample in &mut bus[..samples] {
				*sample = sample.clamp(-1.0, 1.0);
			}

			// Tap the mix on its way out. Overflow means the microphone stopped
			// draining, which the canceller notices on its own; there is nothing
			// useful to do about it from here and nothing may be logged.
			#[cfg(feature = "aec")]
			if let Some(reference) = reference.as_mut() {
				reference.push_interleaved(&bus[..samples]);
			}

			let out = &mut out[done * channels..(done + frames) * channels];
			match channels {
				1 => {
					for (frame, out) in out.iter_mut().enumerate() {
						*out = (bus[frame * 2] + bus[frame * 2 + 1]) * 0.5;
					}
				}
				_ => {
					for (frame, out) in out.chunks_exact_mut(channels).enumerate() {
						out[0] = bus[frame * 2];
						out[1] = bus[frame * 2 + 1];
						// Surround devices get silence past the front pair, which
						// is better than duplicating stereo into the rears.
						out[2..].fill(0.0);
					}
				}
			}

			done += frames;
		}
	}
}

#[cfg(test)]
mod tests {
	use std::sync::mpsc::{Receiver, SyncSender, sync_channel};

	use fixed_resample::{PushStatus, ResamplingChannelConfig, ResamplingProd, resampling_channel};

	use super::*;

	const RATE: u32 = 48_000;

	/// Frames pushed per test: a tenth of a second, far more than any `fill`
	/// below drains, so a short read never starves by accident.
	const FRAMES: usize = RATE as usize / 10;

	struct Harness {
		mixer: Mixer,
		commands: SyncSender<Command>,
		/// Sinks the mixer has finished with. A test asserts on this rather than
		/// on the absence of a `free`, since the point of retirement is that the
		/// audio thread never drops one.
		retired: Receiver<Retired>,
		next: u64,
	}

	impl Harness {
		fn new(channels: usize) -> Self {
			Self::with_depth(channels, 8)
		}

		fn with_depth(channels: usize, depth: usize) -> Self {
			let (commands, rx) = sync_channel(depth);
			let (retired_tx, retired) = sync_channel(MAX_SINKS);
			Self {
				mixer: Mixer::new(rx, retired_tx, RATE, channels),
				commands,
				retired,
				next: 0,
			}
		}

		/// Register a sink and return its id plus the producer feeding it.
		///
		/// The channel runs at the device rate so no resampling sits between the
		/// pushed samples and the assertions.
		fn add(&mut self, gain: Arc<Gain>) -> (u64, ResamplingProd<f32>) {
			let (prod, cons) = resampling_channel::<f32>(
				BUS_CHANNELS,
				RATE,
				RATE,
				true,
				ResamplingChannelConfig {
					latency_seconds: 0.01,
					capacity_seconds: 1.0,
					..Default::default()
				},
			);

			let id = self.next;
			self.next += 1;
			self.commands.send(Command::Add { id, cons, gain }).unwrap();
			(id, prod)
		}

		/// Run one pass, which drains pending commands and reads every sink.
		///
		/// The first pass after [`add`](Self::add) is also what marks the channel's
		/// output side ready: `push_interleaved` discards samples until the
		/// consumer has read once, exactly as it does for the real device before
		/// its first callback.
		fn fill(&mut self, out: &mut [f32]) {
			self.mixer.fill(out);
		}

		/// Fill twice and hand back the buffer, so assertions look at gain that has
		/// finished ramping in from zero.
		fn settle(&mut self, out: &mut [f32]) {
			self.fill(out);
			self.fill(out);
		}
	}

	/// Push `frames` stereo frames of a constant sample.
	fn push(prod: &mut ResamplingProd<f32>, value: f32, frames: usize) {
		prod.push_interleaved(&vec![value; frames * BUS_CHANNELS]);
	}

	#[test]
	fn discards_writes_until_the_device_reads() {
		let mut harness = Harness::new(2);
		let (_, mut prod) = harness.add(Arc::new(Gain::new()));

		// Nothing has read yet, so these samples are dropped rather than queued
		// to play late.
		assert_eq!(prod.push_interleaved(&[1.0; 64]), PushStatus::OutputNotReady);

		let mut out = vec![0.0f32; 512];
		harness.fill(&mut out);
		assert!(out.iter().all(|s| *s == 0.0));

		// Now that the mixer has read once, writes land.
		assert_ne!(prod.push_interleaved(&[1.0; 64]), PushStatus::OutputNotReady);
	}

	#[test]
	fn sums_sinks_and_clips() {
		let mut harness = Harness::new(2);
		let mut out = vec![0.0f32; 2048];

		// Three sinks at 0.5 sum to 1.5, which must clip to 1.0.
		let mut prods: Vec<_> = (0..3).map(|_| harness.add(Arc::new(Gain::new())).1).collect();
		harness.fill(&mut out);
		for prod in &mut prods {
			push(prod, 0.5, FRAMES);
		}

		harness.settle(&mut out);

		let tail = &out[out.len() - 64..];
		assert!(
			tail.iter().all(|s| (*s - 1.0).abs() < 1e-5),
			"expected clipped 1.0, got {:?}",
			&tail[..4]
		);
	}

	#[test]
	fn volume_ramps_instead_of_stepping() {
		let mut harness = Harness::new(2);
		let mut out = vec![0.0f32; 2048];

		let gain = Arc::new(Gain::new());
		let (_, mut prod) = harness.add(gain.clone());
		harness.fill(&mut out);
		push(&mut prod, 1.0, FRAMES);

		harness.settle(&mut out);
		assert!((out[out.len() - 1] - 1.0).abs() < 1e-5, "gain never reached unity");

		// A step to silence must not land on one sample.
		gain.set_volume(0.0);
		harness.fill(&mut out);
		assert!(out[0] > 0.9, "gain jumped: {}", out[0]);
		assert!(out[out.len() - 1].abs() < 1e-5, "gain never reached zero");

		// The whole ramp is monotonic, not just its endpoints.
		let ramp = (RATE as f32 * RAMP) as usize;
		assert!(ramp < out.len() / 2, "test buffer is shorter than the ramp");
		for frame in out[..ramp * BUS_CHANNELS]
			.chunks_exact(BUS_CHANNELS)
			.collect::<Vec<_>>()
			.windows(2)
		{
			assert!(frame[1][0] <= frame[0][0] + 1e-6, "ramp was not monotonic");
		}
	}

	/// `f32::clamp` propagates NaN, so an unguarded setter would put NaN into
	/// the ramp, where it never washes out.
	#[test]
	fn a_non_finite_volume_is_ignored() {
		let gain = Gain::new();

		for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
			gain.set_volume(bad);
			assert_eq!(gain.volume(), 1.0, "{bad} changed the volume");
		}

		gain.set_volume(0.5);
		gain.set_volume(f32::NAN);
		assert_eq!(gain.volume(), 0.5, "NaN clobbered a good volume");
	}

	/// The whole point of the guard: a poisoned gain would silently turn the
	/// device buffer into NaN.
	#[test]
	fn output_stays_finite_after_a_non_finite_volume() {
		let mut harness = Harness::new(2);
		let mut out = vec![0.0f32; 2048];

		let gain = Arc::new(Gain::new());
		let (_, mut prod) = harness.add(gain.clone());
		harness.fill(&mut out);
		push(&mut prod, 0.5, FRAMES);

		gain.set_volume(f32::NAN);
		harness.settle(&mut out);

		assert!(out.iter().all(|s| s.is_finite()), "NaN reached the device buffer");
	}

	#[test]
	fn peak_reports_the_loudest_sample_then_resets() {
		let mut harness = Harness::new(2);
		let mut out = vec![0.0f32; 2048];

		let gain = Arc::new(Gain::new());
		let (_, mut prod) = harness.add(gain.clone());
		harness.fill(&mut out);
		push(&mut prod, 0.25, FRAMES);

		harness.settle(&mut out);

		let peak = gain.peak();
		assert!((peak - 0.25).abs() < 1e-3, "peak was {peak}");
		assert_eq!(gain.peak(), 0.0, "peak did not reset");
	}

	#[test]
	fn removed_sinks_stop_mixing() {
		let mut harness = Harness::new(2);
		let mut out = vec![0.0f32; 2048];

		let (id, mut prod) = harness.add(Arc::new(Gain::new()));
		harness.fill(&mut out);
		push(&mut prod, 1.0, FRAMES);

		harness.settle(&mut out);
		assert!(out[out.len() - 1] > 0.9);

		harness.commands.send(Command::Remove { id }).unwrap();
		harness.fill(&mut out);
		assert!(out.iter().all(|s| *s == 0.0), "removed sink still audible");
	}

	/// A removed sink owns a heap-backed ring buffer. Dropping it in the
	/// callback would free on the audio thread, so it goes back to the driver.
	#[test]
	fn removed_sinks_are_handed_back_rather_than_dropped() {
		let mut harness = Harness::new(2);
		let mut out = vec![0.0f32; 512];

		let (id, _prod) = harness.add(Arc::new(Gain::new()));
		harness.fill(&mut out);

		harness.commands.send(Command::Remove { id }).unwrap();
		harness.fill(&mut out);

		let retired = harness
			.retired
			.try_recv()
			.expect("the removed sink was dropped in the callback");
		match retired {
			Retired::Sink(entry) => assert_eq!(entry.id, id),
			#[cfg(feature = "aec")]
			Retired::Reference(_) => panic!("the mixer retired the echo reference instead"),
		}
	}

	/// Echo cancellation reads what the speaker was given, so the tap has to see
	/// the summed and clipped mix rather than any one sink.
	#[cfg(feature = "aec")]
	#[test]
	fn the_echo_reference_gets_the_mix() {
		let mut harness = Harness::new(2);
		let mut out = vec![0.0f32; 2048];

		let (reference, mut tap) = reference_channel();
		harness.commands.send(Command::Reference(Some(reference))).unwrap();

		let mut prods: Vec<_> = (0..3).map(|_| harness.add(Arc::new(Gain::new())).1).collect();
		harness.fill(&mut out);

		// The tap discards until the canceller has read once, the same way a sink
		// does, so that nothing queues up while nobody is listening. That first
		// read also primes the channel with its configured latency in silence,
		// which is why the buffer below is read past it rather than at it.
		let mut heard = vec![0.0f32; 2048 * BUS_CHANNELS];
		tap.read_interleaved(&mut heard, false);

		for prod in &mut prods {
			push(prod, 0.5, FRAMES);
		}

		harness.settle(&mut out);
		tap.read_interleaved(&mut heard, false);
		let tail = &heard[heard.len() - 64..];
		assert!(
			tail.iter().all(|s| (*s - 1.0).abs() < 1e-5),
			"the tap saw {:?}, not the clipped mix",
			&tail[..4]
		);
	}

	/// The tap owns a heap ring, so replacing or clearing it must leave by the
	/// same route a removed sink does.
	#[cfg(feature = "aec")]
	#[test]
	fn a_replaced_echo_reference_is_handed_back() {
		let mut harness = Harness::new(2);
		let mut out = vec![0.0f32; 512];

		harness
			.commands
			.send(Command::Reference(Some(reference_channel().0)))
			.unwrap();
		harness.fill(&mut out);
		assert!(harness.retired.try_recv().is_err(), "nothing to retire yet");

		harness
			.commands
			.send(Command::Reference(Some(reference_channel().0)))
			.unwrap();
		harness.fill(&mut out);
		assert!(
			matches!(harness.retired.try_recv(), Ok(Retired::Reference(_))),
			"the replaced tap was dropped in the callback"
		);

		harness.commands.send(Command::Reference(None)).unwrap();
		harness.fill(&mut out);
		assert!(
			matches!(harness.retired.try_recv(), Ok(Retired::Reference(_))),
			"the cleared tap was dropped in the callback"
		);
	}

	/// A tap running at the device rate, so the assertions see the mix untouched
	/// by resampling.
	#[cfg(feature = "aec")]
	fn reference_channel() -> (ResamplingProd<f32>, ResamplingCons<f32>) {
		resampling_channel::<f32>(
			BUS_CHANNELS,
			RATE,
			RATE,
			true,
			ResamplingChannelConfig {
				latency_seconds: 0.02,
				capacity_seconds: 0.2,
				..Default::default()
			},
		)
	}

	/// The entry list is allocated once. Growing it would allocate on the audio
	/// thread, which is why the driver caps registrations at MAX_SINKS.
	#[test]
	fn the_entry_list_never_grows() {
		let mut harness = Harness::with_depth(2, MAX_SINKS);
		let mut out = vec![0.0f32; 512];

		let capacity = harness.mixer.entries.capacity();
		assert_eq!(capacity, MAX_SINKS);

		let _prods: Vec<_> = (0..MAX_SINKS).map(|_| harness.add(Arc::new(Gain::new())).1).collect();
		harness.fill(&mut out);

		assert_eq!(harness.mixer.entries.len(), MAX_SINKS);
		assert_eq!(harness.mixer.entries.capacity(), capacity, "the entry list reallocated");
	}

	#[test]
	fn silence_when_no_sink_is_registered() {
		let mut harness = Harness::new(2);
		let mut out = vec![1.0f32; 512];
		harness.fill(&mut out);
		assert!(out.iter().all(|s| *s == 0.0), "callback buffer was not overwritten");
	}

	#[test]
	fn mono_device_gets_the_stereo_average() {
		let mut harness = Harness::new(1);
		let mut out = vec![0.0f32; 1024];

		let (_, mut prod) = harness.add(Arc::new(Gain::new()));
		harness.fill(&mut out);

		// Hard left, so a mono device should hear half of it.
		let mut samples = vec![0.0f32; FRAMES * BUS_CHANNELS];
		for frame in samples.chunks_exact_mut(BUS_CHANNELS) {
			frame[0] = 1.0;
		}
		prod.push_interleaved(&samples);

		harness.settle(&mut out);
		assert!((out[out.len() - 1] - 0.5).abs() < 1e-5, "got {}", out[out.len() - 1]);
	}

	#[test]
	fn surround_devices_get_silence_past_the_front_pair() {
		let mut harness = Harness::new(6);
		let mut out = vec![0.0f32; 6 * 512];

		let (_, mut prod) = harness.add(Arc::new(Gain::new()));
		harness.fill(&mut out);
		push(&mut prod, 1.0, FRAMES);

		harness.settle(&mut out);

		let last = &out[out.len() - 6..];
		assert!(last[0] > 0.9 && last[1] > 0.9, "front pair was silent");
		assert!(last[2..].iter().all(|s| *s == 0.0), "rear channels were not silent");
	}

	#[test]
	fn underflow_reads_as_silence_rather_than_stale_samples() {
		let mut harness = Harness::new(2);
		let mut out = vec![0.0f32; 8192];

		let (_, mut prod) = harness.add(Arc::new(Gain::new()));
		harness.fill(&mut out);
		// Far less than one pass drains, so the channel runs dry mid-buffer.
		push(&mut prod, 1.0, 64);

		harness.settle(&mut out);
		assert!(
			out[out.len() - 1].abs() < 1e-5,
			"expected silence after underflow, got {}",
			out[out.len() - 1]
		);
	}
}
