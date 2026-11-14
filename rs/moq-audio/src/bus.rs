//! Route PCM sources into several mixes, advanced on the caller's clock.
//!
//! A [`Bus`] holds sources, each a [`Live`] or [`Positioned`] channel fed with
//! PCM and controlled by a [`Strip`] of gain, mute, pan, sync delay, and level
//! meter, and [`Mix`]es, each the sum of the sources routed into it. A
//! [`Minus`] is a mix-minus: one mix with some of its sources taken out, for a
//! guest's return feed. Every mix
//! publishes interleaved `f32` at the bus's [`Config`] to any number of
//! independent [`Reader`]s.
//!
//! The bus owns no clock. Whoever owns the audio clock holds the one [`Driver`]
//! and calls [`Driver::advance`] with as many frames as that clock has moved:
//! a program clock paced by `kio::clock`, an audio device's callback, or a test
//! stepping virtual time. Every source and mix advances together on that
//! count, so the mixes stay sample-aligned with each other and with the clock.
//! Each block a reader gets carries its [`position`](Block::position) on that
//! clock, so a reader that falls behind and skips audio still knows where the
//! next block belongs, and a slow reader never stalls the mix or another reader.
//!
//! Each channel is resampled from its own rate to the bus's, and a source that
//! runs slightly fast or slow against the clock is corrected by the same ring
//! that absorbs its jitter, so it never drifts out of the mix. A source that
//! already runs on the bus's clock, such as decoded media scheduled against
//! video, opens a [`Positioned`] channel instead, and says where on the clock
//! each write belongs. Given a stretch bound, it glides onto those positions
//! with the playback sink's time stretch rather than placing each write exactly.
//!
//! ```
//! # async fn example() -> Result<(), moq_audio::Error> {
//! use moq_audio::{bus, encode};
//!
//! let (bus, mut driver) = bus::Bus::new(bus::Config::default())?;
//! let strip = bus::Strip::default();
//! let mut mic = bus.live(encode::Input::new(44_100, moq_audio::Layout::Mono), strip.clone())?;
//! strip.set_gain(0.5);
//!
//! let program = bus.mix();
//! program.route(&strip);
//! let mut reader = program.reader();
//!
//! // On the audio clock: 10 ms of stereo at 48 kHz.
//! driver.advance(480);
//! let block = reader.read().await.expect("the driver is alive");
//! assert_eq!(block.position, 0);
//! assert_eq!(block.frames(), 480);
//! # Ok(())
//! # }
//! ```
//!
//! Enabled by the default-on `bus` feature; `Bus::capture` also needs `capture`.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet, VecDeque};
use std::f32::consts::FRAC_PI_4;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::task::{Poll, ready};
use std::time::Duration;

use crate::encode::Input;
use crate::resample::Remix;
use crate::ring;
use crate::{Error, Layout, Level};

/// Audio held between a [`Live::write`] and the [`Driver::advance`] that mixes it.
///
/// The jitter a late write absorbs without a dropout, and the delay every
/// source carries into the mix. Matches the playback sink's default, which
/// rides out a stalled network read without being audible as delay.
const LATENCY: Duration = Duration::from_millis(50);

/// Room above [`LATENCY`] before writes are dropped. A writer that runs ahead
/// (a decoder catching up after a stall) parks its samples here rather than
/// losing them, and the overflow correction then trims back to [`LATENCY`].
const HEADROOM: Duration = Duration::from_millis(150);

/// Mixed audio a [`Reader`] can fall behind by before it skips ahead.
///
/// Rides out an encoder stall of a few blocks without holding the mix up;
/// past it the reader loses its own oldest audio, never another reader's.
const RETENTION: Duration = Duration::from_millis(200);

/// The longest [`Strip::set_delay`] accepts, which bounds a source's delay line.
pub const MAX_DELAY: Duration = Duration::from_secs(2);

/// How far past the next frame the bus mixes a [`Positioned::write`] may end:
/// the audio a positioned channel holds, as much as a live channel can.
pub const REACH: Duration = LATENCY.saturating_add(HEADROOM);

/// The longest stretch bound [`Bus::positioned`] accepts, which bounds how
/// much audio a positioned channel holds.
pub const MAX_STRETCH: Duration = Duration::from_secs(2);

/// The time stretch's window, which a ring may hold past its bound.
const WINDOW: Duration = Duration::from_millis(30);

/// What every mix is mixed to.
///
/// `#[non_exhaustive]`: construct via [`Config::default`] and set fields, so new
/// options can be added without breaking callers.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Config {
	/// Samples per second per channel of every mix.
	pub sample_rate: u32,
	/// Speaker layout of every mix. Must name speaker positions, since every
	/// channel is remixed to it.
	pub layout: Layout,
}

impl Default for Config {
	fn default() -> Self {
		Self {
			sample_rate: 48_000,
			layout: Layout::Stereo,
		}
	}
}

/// The sources and mixes of one bus, and the routing between them.
///
/// Cheap to clone; every clone adds sources and mixes to the same bus. Only its
/// [`Driver`] advances it.
#[derive(Clone)]
pub struct Bus {
	inner: Arc<Inner>,
}

struct Inner {
	config: Config,
	mixer: Mutex<Mixer>,
	next: AtomicU64,
}

/// Everything [`Driver::advance`] touches, under one lock it holds for one block.
struct Mixer {
	/// Sources by their strip, so a reopened channel keeps its routing.
	sources: HashMap<StripKey, Source>,
	/// Routed mixes in stable slots, which [`Minus`]es name their parent by.
	mixes: Vec<Option<Routed>>,
	free: Vec<usize>,
	minuses: Vec<MinusState>,
	/// One channel's share of a block, reused across blocks.
	scratch: Vec<f32>,
	/// Frames advanced so far: the position of the next block.
	position: u64,
	/// The driver is gone, so every mix has ended.
	closed: bool,
}

/// A strip's identity: the address of its shared state, which every clone shares.
type StripKey = usize;

/// One open channel of a source, as the mixer reads it.
struct Feed {
	id: u64,
	queue: Queue,
	/// Panned with constant power rather than balanced.
	mono: bool,
}

/// Where a channel's audio waits to be mixed.
enum Queue {
	/// A [`Live`]'s jitter buffer, filled without the mixer's lock. Boxed,
	/// since it dwarfs a positioned queue.
	Live(Box<ring::Consumer>),
	/// A [`Positioned`] channel's interleaved samples, the first of which is
	/// mixed at the bus's next position. Filled under the mixer's lock, so a
	/// write sees exactly where the mix is.
	Positioned(VecDeque<f32>),
	/// A [`Positioned`] channel with a stretch bound, which plays onto the
	/// positions its writes are due at. Filled under the mixer's lock, as a
	/// positioned queue is.
	Stretched(Box<Stretching>),
}

/// A stretched channel's ring, whose frame clock runs `origin` behind the
/// bus's positions.
struct Stretching {
	ring: ring::Consumer,
	/// Moved whenever the channel runs dry, so the ring's clock stays within
	/// its queued audio of the bus's however long the bus has run: the ring
	/// holds that distance in 32 bits.
	origin: u64,
}

impl Stretching {
	/// Bus `position` on the ring's frame clock, which wraps.
	fn clock(&self, position: u64) -> usize {
		position.wrapping_sub(self.origin) as usize
	}
}

impl Feed {
	/// Fill `output` with this channel's next samples, the first at bus
	/// `position`, and silence past them.
	fn read(&mut self, output: &mut [f32], position: u64, channels: usize) {
		match &mut self.queue {
			Queue::Live(ring) => {
				ring.read(output);
			}
			Queue::Stretched(stretching) => {
				let now = stretching.clock(position);
				stretching.ring.read_at_frame(output, now, output.len() / channels);
			}
			Queue::Positioned(queue) => {
				let queued = queue.len().min(output.len());
				for (out, sample) in output.iter_mut().zip(queue.drain(..queued)) {
					*out = sample;
				}
				output[queued..].fill(0.0);
			}
		}
	}
}

/// The channels sharing one strip, as the mixer sees them.
struct Source {
	strip: Strip,
	channels: Vec<Feed>,
	/// Gain actually applied, ramping toward the strip's target across a block.
	applied: f32,
	/// Pan actually applied, ramping toward the strip's pan across a block.
	panned: f32,
	/// Balanced channels, then mono ones a stereo bus pans with constant power.
	/// Each class is delayed apart, so a pan change reaches audio already held
	/// back by the sync delay.
	lanes: [Lane; 2],
	/// This source's share of the current block, after its delay, pan, and gain.
	block: Vec<f32>,
}

/// A source's channels that share a pan law, summed and then delayed.
#[derive(Default)]
struct Lane {
	block: Vec<f32>,
	/// Samples held back for the strip's sync delay.
	delay: VecDeque<f32>,
	/// A channel was read into `block` this advance.
	fed: bool,
	/// Samples advanced since a channel last fed this lane: once they cover
	/// the delay line, it holds only silence.
	idle: usize,
}

impl Lane {
	/// Hold `block` back by exactly `held` samples. A change inserts silence or
	/// skips ahead once, which a calibration rarely makes.
	fn delay(&mut self, held: usize, samples: usize) {
		if held == 0 && self.delay.is_empty() {
			return;
		}
		while self.delay.len() < held {
			self.delay.push_front(0.0);
		}
		if self.delay.len() > held {
			self.delay.drain(..self.delay.len() - held);
		}
		self.delay.extend(self.block.iter().copied());
		for (out, sample) in self.block.iter_mut().zip(self.delay.drain(..samples)) {
			*out = sample;
		}
	}
}

struct Routed {
	id: u64,
	/// In routing order, so the sum is the same every block.
	routes: Vec<Strip>,
	routed: HashSet<StripKey>,
	output: Output,
}

struct MinusState {
	id: u64,
	parent: usize,
	/// Deduplicated, so each is subtracted at most once.
	excluded: Vec<Strip>,
	output: Output,
}

/// One mix's block buffer, meter, and published history.
struct Output {
	buffer: Vec<f32>,
	level: Arc<Meter>,
	history: kio::Producer<History>,
}

impl Output {
	fn new(closed: bool) -> (Self, kio::Consumer<History>, Arc<Meter>) {
		let history = kio::Producer::new(History::default());
		if closed {
			let _ = history.close();
		}
		let consumer = history.consume();
		let level = Arc::new(Meter::default());
		let output = Self {
			buffer: Vec::new(),
			level: level.clone(),
			history,
		};
		(output, consumer, level)
	}

	/// Meter the block in `buffer` and hand it to every reader.
	fn publish(&mut self, position: u64, channels: usize, retention: usize) {
		let frames = self.buffer.len() / channels;
		self.level.record(Level::measure(&self.buffer));
		let Ok(mut history) = self.history.write() else { return };
		history.frames += frames;
		history.blocks.push_back(Block {
			position,
			data: Arc::from(self.buffer.as_slice()),
			channels,
		});
		history.next += 1;
		// Keep the newest block even when it alone exceeds the retention.
		while history.frames > retention && history.blocks.len() > 1 {
			let dropped = history.blocks.pop_front().unwrap();
			history.frames -= dropped.data.len() / channels;
			history.first += 1;
		}
	}
}

#[derive(Debug, Default)]
struct History {
	blocks: VecDeque<Block>,
	/// Index of `blocks[0]` among every block published.
	first: u64,
	/// Index of the next block to be published.
	next: u64,
	/// Frames across `blocks`.
	frames: usize,
}

fn key(strip: &Strip) -> StripKey {
	Arc::as_ptr(&strip.shared) as StripKey
}

/// `duration` in frames at `sample_rate`, rounded down.
fn frames(sample_rate: u32, duration: Duration) -> usize {
	(u128::from(sample_rate) * duration.as_nanos() / 1_000_000_000) as usize
}

impl Bus {
	/// An empty bus producing PCM in `config`'s format, and the one driver
	/// that advances it.
	pub fn new(config: Config) -> Result<(Self, Driver), Error> {
		if config.sample_rate == 0 {
			return Err(Error::Unsupported("bus sample rate must be > 0".into()));
		}
		if config.layout.speakers().is_none() {
			return Err(Error::Unsupported(format!(
				"a bus needs speaker positions to remix into (got {:?})",
				config.layout
			)));
		}

		let retention = frames(config.sample_rate, RETENTION);
		let inner = Arc::new(Inner {
			config,
			mixer: Mutex::new(Mixer {
				sources: HashMap::new(),
				mixes: Vec::new(),
				free: Vec::new(),
				minuses: Vec::new(),
				scratch: Vec::new(),
				position: 0,
				closed: false,
			}),
			next: AtomicU64::new(0),
		});
		let driver = Driver {
			inner: inner.clone(),
			retention,
		};
		Ok((Self { inner }, driver))
	}

	/// The format every mix produces.
	pub fn config(&self) -> &Config {
		&self.inner.config
	}

	/// Add a source, taking PCM in the layout `input` describes and controlled
	/// by `strip`.
	///
	/// The strip is the source's identity: it outlives the channel, so a source
	/// that reopens with another format (a new decoder, a replugged device)
	/// keeps its gain, mute, pan, delay, meter, and routing by passing the same
	/// strip again. Channels sharing a strip are summed. The channel leaves the
	/// bus when dropped.
	///
	/// Refuses a strip panned off center unless the bus is stereo, and holds
	/// the strip at center while this channel is open on such a bus.
	pub fn live(&self, input: Input, strip: Strip) -> Result<Live, Error> {
		if input.sample_rate == 0 {
			return Err(Error::Unsupported("channel sample rate must be > 0".into()));
		}
		let (prod, cons) = ring::channel(
			self.inner.config.layout.channels() as usize,
			input.sample_rate,
			self.inner.config.sample_rate,
			ring::Config {
				latency: LATENCY,
				capacity: LATENCY + HEADROOM,
				// Correct drift against the clock by trimming or padding back to
				// the target depth, not by letting the ring run dry or full.
				pad: true,
				trim: true,
				// No writer here declares a silence, so measuring for comfort
				// noise would be wasted on every block.
				comfort: false,
			},
		);
		let port = self.open(input, strip, Queue::Live(Box::new(cons)))?;
		Ok(Live { prod, port })
	}

	/// Add a source on the bus's own clock, whose every write says the bus
	/// position its first frame is due at, as [`live`](Self::live) adds one
	/// running on a clock of its own.
	///
	/// For audio scheduled against something else on the bus's clock, such as
	/// the video of the same media: there is no jitter buffer between a write
	/// and its position, and no drift correction to move it.
	///
	/// `stretch` bounds how far the channel's audio may trail its positions,
	/// as the playback sink's `due` bounds its own. Zero places every write
	/// exactly; otherwise a write that moves earlier or later than the audio
	/// queued before it plays right after that audio, and the channel
	/// time-stretches onto its positions. See [`Positioned::write`].
	///
	/// Refuses `input` at another rate than the bus's, which positions count
	/// frames of, a `stretch` past [`MAX_STRETCH`], and a strip as
	/// [`live`](Self::live) does.
	pub fn positioned(&self, input: Input, strip: Strip, stretch: Duration) -> Result<Positioned, Error> {
		let sample_rate = self.inner.config.sample_rate;
		if input.sample_rate != sample_rate {
			return Err(Error::Unsupported(format!(
				"a positioned channel takes the bus rate, {sample_rate} Hz (got {} Hz)",
				input.sample_rate
			)));
		}
		if stretch > MAX_STRETCH {
			return Err(Error::Unsupported(format!(
				"a positioned channel stretches at most {MAX_STRETCH:?} (got {stretch:?})"
			)));
		}
		let channels = self.inner.config.layout.channels() as usize;
		if stretch.is_zero() {
			let samples = frames(sample_rate, REACH) * channels;
			let port = self.open(input, strip, Queue::Positioned(VecDeque::with_capacity(samples)))?;
			return Ok(Positioned { port, stretch: None });
		}

		let (prod, mut cons) = ring::channel(
			channels,
			sample_rate,
			sample_rate,
			ring::Config {
				// The positions carry the writer's own playout delay, so the
				// ring adds none and keeps no depth of its own.
				latency: Duration::ZERO,
				// Audio trailing its positions by the bound and one stretch
				// window, then as much as a write may reach past the clock.
				capacity: stretch + WINDOW + REACH,
				pad: false,
				trim: false,
				comfort: false,
			},
		);
		cons.stretch(sample_rate);
		// Writes land before the first advance, as an exact channel's do.
		cons.start();
		let stretching = Stretching { ring: cons, origin: 0 };
		let port = self.open(input, strip, Queue::Stretched(Box::new(stretching)))?;
		Ok(Positioned {
			port,
			stretch: Some(Stretched {
				prod,
				bound: frames(sample_rate, stretch),
			}),
		})
	}

	/// Add a channel reading `queue` to `strip`'s source.
	fn open(&self, input: Input, strip: Strip, queue: Queue) -> Result<Port, Error> {
		let output = self.inner.config.layout;
		let remix = if input.layout == output {
			None
		} else {
			Some(Remix::new(input.layout, output)?)
		};
		// Only a stereo bus pans, so any other holds the strip at center for
		// as long as this channel is open. Nothing below fails, so the hold
		// is always released by the port's drop.
		let centered = output != Layout::Stereo;
		if centered {
			strip.hold_center(output)?;
		}

		let id = self.inner.next.fetch_add(1, Ordering::Relaxed);
		let mut mixer = self.inner.mixer.lock().unwrap();
		mixer
			.sources
			.entry(key(&strip))
			.or_insert_with(|| Source {
				strip: strip.clone(),
				channels: Vec::new(),
				applied: strip.target(),
				panned: strip.pan(),
				lanes: Default::default(),
				block: Vec::new(),
			})
			.channels
			.push(Feed {
				id,
				queue,
				mono: input.layout == Layout::Mono,
			});
		drop(mixer);

		Ok(Port {
			id,
			remix,
			input,
			strip,
			centered,
			bus: Arc::downgrade(&self.inner),
		})
	}

	/// A new mix with nothing routed into it yet.
	///
	/// It is mixed from the next [`Driver::advance`] until its last handle drops.
	pub fn mix(&self) -> Mix {
		let id = self.inner.next.fetch_add(1, Ordering::Relaxed);
		let mut mixer = self.inner.mixer.lock().unwrap();
		let (output, history, level) = Output::new(mixer.closed);
		let routed = Routed {
			id,
			routes: Vec::new(),
			routed: HashSet::new(),
			output,
		};
		let slot = match mixer.free.pop() {
			Some(slot) => {
				mixer.mixes[slot] = Some(routed);
				slot
			}
			None => {
				mixer.mixes.push(Some(routed));
				mixer.mixes.len() - 1
			}
		};
		drop(mixer);

		Mix {
			shared: Arc::new(MixShared {
				id,
				slot,
				history,
				level,
				bus: Arc::downgrade(&self.inner),
			}),
		}
	}

	/// Feed an independent capture consumer into the bus, controlled by `strip`.
	///
	/// Drive [`Capture::run`] alongside the capture producer; the channel adapts
	/// to the producer's fixed input format.
	///
	/// Requires the `capture` feature.
	#[cfg(feature = "capture")]
	pub fn capture(&self, capture: crate::capture::Consumer, strip: Strip) -> Result<Capture, Error> {
		let channel = self.live(capture.input().clone(), strip)?;
		Ok(Capture { capture, channel })
	}
}

/// The one handle that advances a [`Bus`], held by whoever owns its clock.
///
/// Dropping it ends every mix: readers drain what was published, then end.
pub struct Driver {
	inner: Arc<Inner>,
	/// [`RETENTION`] in frames at the bus rate.
	retention: usize,
}

impl Driver {
	/// Mix the next `frames` frames of every mix, as the clock moves by that much.
	///
	/// Call it as the clock advances, with exactly that much time's worth of
	/// frames: the bus has no clock of its own, so this is what paces every
	/// source. A source with nothing buffered contributes silence. Each mix
	/// costs only the sources routed into it, and a mix-minus one more per
	/// source it removes.
	pub fn advance(&mut self, frames: usize) {
		if frames == 0 {
			return;
		}
		let channels = self.inner.config.layout.channels() as usize;
		let stereo = self.inner.config.layout == Layout::Stereo;
		let sample_rate = u64::from(self.inner.config.sample_rate);
		let samples = frames * channels;

		let mut mixer = self.inner.mixer.lock().unwrap();
		let Mixer {
			sources,
			mixes,
			minuses,
			scratch,
			position,
			..
		} = &mut *mixer;

		for source in sources.values_mut() {
			source.advance(*position, samples, channels, stereo, sample_rate, scratch);
		}

		for routed in mixes.iter_mut().flatten() {
			let buffer = &mut routed.output.buffer;
			buffer.clear();
			buffer.resize(samples, 0.0);
			for strip in &routed.routes {
				if let Some(source) = sources.get(&key(strip)) {
					for (out, sample) in buffer.iter_mut().zip(&source.block) {
						*out += *sample;
					}
				}
			}
			routed.output.publish(*position, channels, self.retention);
		}

		for minus in minuses.iter_mut() {
			let parent = mixes[minus.parent].as_ref().expect("a mix-minus keeps its parent");
			let buffer = &mut minus.output.buffer;
			buffer.clear();
			buffer.extend_from_slice(&parent.output.buffer);
			for strip in &minus.excluded {
				let excluded = key(strip);
				if parent.routed.contains(&excluded)
					&& let Some(source) = sources.get(&excluded)
				{
					for (out, sample) in buffer.iter_mut().zip(&source.block) {
						*out -= *sample;
					}
				}
			}
			minus.output.publish(*position, channels, self.retention);
		}

		*position += frames as u64;
	}

	/// Frames advanced so far, which is the position of the next block.
	pub fn position(&self) -> u64 {
		self.inner.mixer.lock().unwrap().position
	}
}

impl Drop for Driver {
	fn drop(&mut self) {
		let mut mixer = self.inner.mixer.lock().unwrap();
		mixer.closed = true;
		for routed in mixer.mixes.iter().flatten() {
			let _ = routed.output.history.close();
		}
		for minus in &mixer.minuses {
			let _ = minus.output.history.close();
		}
	}
}

impl Source {
	/// Compute this source's share of the next `samples`, interleaved.
	///
	/// Only a `stereo` bus pans; any other holds every strip on it at center.
	fn advance(
		&mut self,
		position: u64,
		samples: usize,
		channels: usize,
		stereo: bool,
		sample_rate: u64,
		scratch: &mut Vec<f32>,
	) {
		for lane in &mut self.lanes {
			lane.fed = false;
		}
		for feed in &mut self.channels {
			let lane = &mut self.lanes[usize::from(stereo && feed.mono)];
			if lane.fed {
				scratch.resize(samples, 0.0);
				feed.read(scratch, position, channels);
				for (out, sample) in lane.block.iter_mut().zip(scratch.iter()) {
					*out += *sample;
				}
			} else {
				lane.block.clear();
				lane.block.resize(samples, 0.0);
				feed.read(&mut lane.block, position, channels);
				lane.fed = true;
			}
		}

		// Delay, then pan, so a pan change applies from this block on.
		let delay = self.strip.delay();
		let held = (u128::from(sample_rate) * delay.as_nanos() / 1_000_000_000) as usize * channels;
		let pan = self.strip.pan();
		let mut first = true;
		for (mono, lane) in [false, true].into_iter().zip(&mut self.lanes) {
			if lane.fed {
				lane.idle = 0;
			} else {
				// Nothing new, but audio held back from a closed channel plays out.
				if lane.delay.is_empty() {
					continue;
				}
				lane.block.clear();
				lane.block.resize(samples, 0.0);
				lane.idle += samples;
			}
			lane.delay(held, samples);
			if stereo {
				apply_pan(&mut lane.block, self.panned, pan, mono);
			}
			if first {
				std::mem::swap(&mut self.block, &mut lane.block);
				first = false;
			} else {
				for (out, sample) in self.block.iter_mut().zip(&lane.block) {
					*out += *sample;
				}
			}
			// The last held audio just played, so free the delay line.
			if !lane.fed && lane.idle >= lane.delay.len() {
				*lane = Lane::default();
			}
		}
		if first {
			self.block.clear();
			self.block.resize(samples, 0.0);
		}
		self.panned = pan;

		// Ramp across the block rather than stepping, so a fader move or a mute
		// never clicks.
		let frames = samples / channels;
		let target = self.strip.target();
		let start = self.applied;
		let step = (target - start) / frames as f32;
		for (index, frame) in self.block.chunks_exact_mut(channels).enumerate() {
			let gain = start + step * (index + 1) as f32;
			for sample in frame {
				*sample *= gain;
			}
		}
		self.applied = target;

		self.strip.shared.level.record(Level::measure(&self.block));
	}
}

/// Pan interleaved stereo `block` from `start` toward `target` across it,
/// moving the pan rather than each side's gain so a mono source keeps its
/// loudness mid-move.
fn apply_pan(block: &mut [f32], start: f32, target: f32, mono: bool) {
	if start == target {
		let [left, right] = pan_gains(target, mono);
		// A centered stereo source passes through bit-exact.
		if left == 1.0 && right == 1.0 {
			return;
		}
		for frame in block.as_chunks_mut::<2>().0 {
			frame[0] *= left;
			frame[1] *= right;
		}
		return;
	}
	let frames = block.len() / 2;
	let step = (target - start) / frames as f32;
	for (index, frame) in block.as_chunks_mut::<2>().0.iter_mut().enumerate() {
		let [left, right] = pan_gains(start + step * (index + 1) as f32, mono);
		frame[0] *= left;
		frame[1] *= right;
	}
}

/// The left and right gains for `pan`.
///
/// A mono source follows a sine/cosine law, -3 dB per side at center, so its
/// loudness holds wherever it sits. Any other source is balanced: the same
/// curve, scaled to unity at center, turns the far side down and leaves the
/// near side alone.
fn pan_gains(pan: f32, mono: bool) -> [f32; 2] {
	if mono {
		let (right, left) = ((pan + 1.0) * FRAC_PI_4).sin_cos();
		// `cos` of a rounded π/2 is a hair below zero.
		return [left.max(0.0), right.max(0.0)];
	}
	// The mono law's far side over its center gain, `√2·cos(π/4 + x)`,
	// written as `cos x - sin x` so center is exactly unity.
	let (sin, cos) = (pan.abs() * FRAC_PI_4).sin_cos();
	let far = (cos - sin).max(0.0);
	if pan < 0.0 { [1.0, far] } else { [far, 1.0] }
}

/// One mix: the sum of the sources routed into it.
///
/// Cheap to clone; clones control the same mix. The mix ends when its last
/// handle drops, and its readers end after draining what was published.
#[derive(Clone)]
pub struct Mix {
	shared: Arc<MixShared>,
}

struct MixShared {
	id: u64,
	slot: usize,
	history: kio::Consumer<History>,
	level: Arc<Meter>,
	bus: Weak<Inner>,
}

impl Drop for MixShared {
	fn drop(&mut self) {
		let Some(bus) = self.bus.upgrade() else { return };
		let mut mixer = bus.mixer.lock().unwrap();
		if mixer.mixes[self.slot]
			.as_ref()
			.is_some_and(|routed| routed.id == self.id)
		{
			mixer.mixes[self.slot] = None;
			mixer.free.push(self.slot);
		}
	}
}

impl Mix {
	/// Route `strip`'s source into this mix, from the next block on.
	///
	/// Routing follows the strip, so it holds before the source opens a channel
	/// and after it reopens. Routing a strip twice is a no-op.
	pub fn route(&self, strip: &Strip) {
		self.with(|routed| {
			if routed.routed.insert(key(strip)) {
				routed.routes.push(strip.clone());
			}
		});
	}

	/// Take `strip`'s source out of this mix, from the next block on.
	pub fn unroute(&self, strip: &Strip) {
		self.with(|routed| {
			if routed.routed.remove(&key(strip)) {
				routed.routes.retain(|routed| key(routed) != key(strip));
			}
		});
	}

	/// Whether `strip`'s source is routed into this mix.
	pub fn routes(&self, strip: &Strip) -> bool {
		let mut routes = false;
		self.with(|routed| routes = routed.routed.contains(&key(strip)));
		routes
	}

	/// A mix-minus: this mix without the sources of `strips`, such as every
	/// source carrying one guest's sound, for that guest's return feed.
	///
	/// It follows this mix's routing, and costs one subtraction per removed
	/// source per block rather than a second sum. A strip listed twice is
	/// removed once. Keeps this mix alive while it lives.
	pub fn minus<'a>(&self, strips: impl IntoIterator<Item = &'a Strip>) -> Minus {
		// Collected before locking the mixer, since the caller's iterator may lock it.
		let mut seen = HashSet::new();
		let excluded: Vec<Strip> = strips
			.into_iter()
			.filter(|strip| seen.insert(key(strip)))
			.cloned()
			.collect();
		let (id, history, level) = match self.shared.bus.upgrade() {
			Some(bus) => {
				let id = bus.next.fetch_add(1, Ordering::Relaxed);
				let mut mixer = bus.mixer.lock().unwrap();
				let (output, history, level) = Output::new(mixer.closed);
				mixer.minuses.push(MinusState {
					id,
					parent: self.shared.slot,
					excluded,
					output,
				});
				(id, history, level)
			}
			// Dropping the output closes its history: nothing will advance it.
			None => {
				let (_, history, level) = Output::new(true);
				(0, history, level)
			}
		};

		Minus {
			shared: Arc::new(MinusShared {
				id,
				history,
				level,
				parent: self.clone(),
			}),
		}
	}

	/// An independent reader of this mix, starting at the next block.
	pub fn reader(&self) -> Reader {
		Reader::new(&self.shared.history)
	}

	/// The loudest this mix has been since the previous call.
	pub fn level(&self) -> Level {
		self.shared.level.take()
	}

	/// Run `f` on this mix's state, or return `None` once the bus is gone.
	fn with<R>(&self, f: impl FnOnce(&mut Routed) -> R) -> Option<R> {
		let bus = self.shared.bus.upgrade()?;
		let mut mixer = bus.mixer.lock().unwrap();
		let routed = mixer.mixes[self.shared.slot].as_mut()?;
		Some(f(routed))
	}
}

/// A mix-minus, built by [`Mix::minus`]: a mix without some of its sources.
///
/// Cheap to clone. It ends when its last handle drops.
#[derive(Clone)]
pub struct Minus {
	shared: Arc<MinusShared>,
}

struct MinusShared {
	id: u64,
	history: kio::Consumer<History>,
	level: Arc<Meter>,
	/// Keeps the parent mixed, and names the bus to leave on drop.
	parent: Mix,
}

impl Drop for MinusShared {
	fn drop(&mut self) {
		let Some(bus) = self.parent.shared.bus.upgrade() else {
			return;
		};
		bus.mixer.lock().unwrap().minuses.retain(|minus| minus.id != self.id);
	}
}

impl Minus {
	/// An independent reader of this mix-minus, starting at the next block.
	pub fn reader(&self) -> Reader {
		Reader::new(&self.shared.history)
	}

	/// The loudest this mix-minus has been since the previous call.
	pub fn level(&self) -> Level {
		self.shared.level.take()
	}
}

/// One block of a mix, as a [`Reader`] gets it.
#[derive(Clone, Debug)]
pub struct Block {
	/// Frames the driver had advanced before this block: where its first frame
	/// sits on the clock. A reader that skipped audio sees this jump.
	pub position: u64,
	/// Interleaved samples in the bus layout, shared by every reader.
	pub data: Arc<[f32]>,
	channels: usize,
}

impl Block {
	/// Frames in this block.
	pub fn frames(&self) -> usize {
		self.data.len() / self.channels
	}
}

/// An independent reader of one mix.
///
/// Clones start at the same block and then advance independently. A reader
/// that falls more than 200 ms behind skips to the oldest block still kept,
/// whose [`position`](Block::position) shows how much it lost. It never holds
/// the mix or another reader up.
#[derive(Clone)]
pub struct Reader {
	history: kio::Consumer<History>,
	next: u64,
}

impl Reader {
	fn new(history: &kio::Consumer<History>) -> Self {
		let next = history.read().next;
		Self {
			history: history.clone(),
			next,
		}
	}

	/// The next block, or `None` once the mix has ended and every block kept
	/// for this reader has been read.
	#[kio::wrapper]
	pub fn poll_read(&mut self, waiter: &kio::Waiter) -> Poll<Option<Block>> {
		let next = self.next;
		let result = ready!(self.history.poll(waiter, |history| {
			let index = next.max(history.first);
			match history.blocks.get((index - history.first) as usize) {
				Some(block) => Poll::Ready((block.clone(), index + 1)),
				None => Poll::Pending,
			}
		}));
		match result {
			Ok((block, next)) => {
				self.next = next;
				Poll::Ready(Some(block))
			}
			Err(_) => Poll::Ready(None),
		}
	}
}

/// One source's PCM on its way into a [`Bus`], on a clock of its own.
///
/// Dropping it takes this channel out of the bus.
pub struct Live {
	prod: ring::Producer,
	port: Port,
}

impl Live {
	/// Push PCM in the format, rate, and layout this channel was created with.
	///
	/// Never blocks. Audio that arrives before the bus first advances, or that
	/// overruns the channel's buffer, is dropped to keep the mixes live.
	///
	/// Refuses a NaN or infinite sample, which would reach every mix and
	/// survive any gain, mute included.
	pub fn write(&mut self, data: &[u8]) -> Result<(), Error> {
		let pcm = self.port.pcm(data)?;
		self.push(&pcm);
		Ok(())
	}

	fn push(&mut self, pcm: &[f32]) {
		match &self.port.remix {
			Some(remix) => {
				let _ = self.prod.push(&remix.process(pcm));
			}
			None => {
				let _ = self.prod.push(pcm);
			}
		}
	}

	/// This source's gain, mute, pan, delay, and meter.
	pub fn strip(&self) -> &Strip {
		&self.port.strip
	}
}

/// One source's PCM on its way into a [`Bus`], placed on the bus's clock.
///
/// Built by [`Bus::positioned`]. Dropping it takes this channel out of the bus.
pub struct Positioned {
	port: Port,
	/// The writing end of a [`Queue::Stretched`], with its bound in frames;
	/// `None` places each write exactly.
	stretch: Option<Stretched>,
}

struct Stretched {
	prod: ring::Producer,
	bound: usize,
}

impl Positioned {
	/// Mix PCM in this channel's format and layout so its first frame is due
	/// at bus `position`, on the clock [`Driver::position`] counts, then
	/// delayed by the strip's sync delay.
	///
	/// Without a stretch bound, each write plays exactly there. Writes only
	/// move forward: whatever lies between the end of the previous write and
	/// `position` is silence. A write fails, and nothing of it is mixed, if any
	/// of it would land where the bus has already mixed or where this channel
	/// already holds audio. The bus never shifts a write to fit.
	///
	/// With one, a write plays right after the audio the channel holds, and the
	/// channel speeds up or slows down onto its position, as the playback sink
	/// does onto its clock. Audio trailing its positions by more than the bound
	/// skips onto them. Only a write to a
	/// channel that has run dry is placed exactly, behind silence, since
	/// nothing audible plays before it. A write fails whole if the channel has
	/// no room for it.
	///
	/// Either way, a write fails if it would end more than [`REACH`] past the
	/// next frame the bus mixes.
	///
	/// Waits only for a [`Driver::advance`] in progress. Once the driver is
	/// gone nothing mixes, so writes are dropped.
	///
	/// Refuses a NaN or infinite sample, as [`Live::write`] does.
	pub fn write(&mut self, position: u64, data: &[u8]) -> Result<(), Error> {
		let pcm = self.port.pcm(data)?;
		let pcm = match &self.port.remix {
			Some(remix) => remix.process(&pcm).into(),
			None => pcm,
		};
		if pcm.is_empty() {
			return Ok(());
		}
		let Some(bus) = self.port.bus.upgrade() else {
			return Ok(());
		};
		let channels = bus.config.layout.channels() as usize;
		let reach = frames(bus.config.sample_rate, REACH) as u64;
		let frames = (pcm.len() / channels) as u64;

		let mut mixer = bus.mixer.lock().unwrap();
		if mixer.closed {
			return Ok(());
		}
		let next = mixer.position;
		if position.saturating_add(frames) > next + reach {
			return Err(Error::Unsupported(format!(
				"a write of {frames} frames at bus position {position} ends past {}, the reach from {next}",
				next + reach
			)));
		}
		let feed = mixer
			.sources
			.get_mut(&key(&self.port.strip))
			.and_then(|source| source.channels.iter_mut().find(|feed| feed.id == self.port.id))
			.expect("an open channel keeps its feed");

		if let Some(stretched) = &mut self.stretch {
			let Queue::Stretched(stretching) = &mut feed.queue else {
				unreachable!("a stretched channel feeds a stretched queue");
			};
			// A dry channel plays silence until the write anyway, so it waits
			// there exactly rather than playing early and slowing down. With
			// nothing queued, the ring's clock restarts on the bus's here.
			let dry = stretching.ring.available() == 0;
			let gap = if dry {
				stretching.origin = next.wrapping_sub(stretching.ring.consumed() as u64);
				position.saturating_sub(next) as usize
			} else {
				0
			};
			let vacant = stretched.prod.vacant();
			if gap + frames as usize > vacant {
				return Err(Error::Unsupported(format!(
					"a write of {frames} frames at bus position {position} overflows this channel, with room for {vacant}"
				)));
			}
			const SILENCE: [f32; 1024] = [0.0; 1024];
			let mut silence = gap * channels;
			while silence > 0 {
				let samples = silence.min(SILENCE.len() / channels * channels);
				let _ = stretched.prod.push(&SILENCE[..samples]);
				silence -= samples;
			}
			let _ = stretched.prod.push(&pcm);
			let due = stretching.clock(position.wrapping_add(frames));
			stretched.prod.due_frame(due, stretched.bound, 0);
			return Ok(());
		}

		let Queue::Positioned(queue) = &mut feed.queue else {
			unreachable!("a positioned channel feeds a positioned queue");
		};
		let end = next + (queue.len() / channels) as u64;
		if position < next {
			return Err(Error::Unsupported(format!(
				"bus position {position} is already mixed, up to {next}"
			)));
		}
		if position < end {
			return Err(Error::Unsupported(format!(
				"bus position {position} overlaps this channel's audio, which runs to {end}"
			)));
		}
		queue.extend(std::iter::repeat_n(0.0, (position - end) as usize * channels));
		queue.extend(pcm.iter().copied());
		Ok(())
	}

	/// This source's gain, mute, pan, delay, and meter.
	pub fn strip(&self) -> &Strip {
		&self.port.strip
	}
}

/// What every channel shares: its place in the mixer, and the conversion of
/// its input to the bus layout.
struct Port {
	id: u64,
	remix: Option<Remix>,
	input: Input,
	strip: Strip,
	/// Holds the strip at center, since the bus cannot pan.
	centered: bool,
	bus: Weak<Inner>,
}

impl Port {
	/// `data` as interleaved `f32` in the input's layout, refusing a NaN or
	/// infinite sample, which would reach every mix and survive any gain.
	fn pcm<'a>(&self, data: &'a [u8]) -> Result<Cow<'a, [f32]>, Error> {
		let pcm = self
			.input
			.format
			.as_interleaved_f32(data, self.input.layout.channels())?;
		if pcm.iter().any(|sample| !sample.is_finite()) {
			return Err(Error::Unsupported("a non-finite PCM sample".into()));
		}
		Ok(pcm)
	}
}

impl Drop for Port {
	fn drop(&mut self) {
		if self.centered {
			self.strip.release_center();
		}
		let Some(bus) = self.bus.upgrade() else { return };
		let mut mixer = bus.mixer.lock().unwrap();
		let key = key(&self.strip);
		if let Some(source) = mixer.sources.get_mut(&key) {
			source.channels.retain(|feed| feed.id != self.id);
			if source.channels.is_empty() {
				mixer.sources.remove(&key);
			}
		}
	}
}

/// The gain, mute, pan, sync delay, and meter of one source in a [`Bus`].
///
/// Cheap to clone and `Send + Sync`: clones control the same source, so a UI
/// can hold one while a task owns the [`Live`]. Its settings apply to every
/// mix the source is routed into, from the next [`Driver::advance`], with gain
/// and pan ramped across it.
#[derive(Clone, Debug, Default)]
pub struct Strip {
	shared: Arc<StripState>,
}

#[derive(Debug)]
struct StripState {
	/// Linear gain, as [`f32::to_bits`] since there is no atomic float.
	gain: AtomicU32,
	muted: AtomicBool,
	/// The pan as [`f32::to_bits`] in the low half, and in the high half how
	/// many channels are open on buses that cannot pan. One atomic, so a pan
	/// and a channel opening at once can't both pass the other's check.
	pan: AtomicU64,
	/// Sync delay in nanoseconds.
	delay: AtomicU64,
	level: Meter,
}

/// One channel open on a bus that cannot pan, in [`StripState::pan`].
const CENTERED: u64 = 1 << 32;

impl Default for StripState {
	fn default() -> Self {
		Self {
			gain: AtomicU32::new(1.0f32.to_bits()),
			muted: AtomicBool::new(false),
			pan: AtomicU64::new(0),
			delay: AtomicU64::new(0),
			level: Meter::default(),
		}
	}
}

impl Strip {
	/// Set the linear gain: `1.0` is unity, `0.0` silent, above `1.0` boosts.
	///
	/// # Panics
	///
	/// If `gain` is negative or not finite.
	pub fn set_gain(&self, gain: f32) {
		assert!(gain.is_finite() && gain >= 0.0, "invalid gain {gain}");
		self.shared.gain.store(gain.to_bits(), Ordering::Relaxed);
	}

	/// The linear gain, independent of [`muted`](Self::muted).
	pub fn gain(&self) -> f32 {
		f32::from_bits(self.shared.gain.load(Ordering::Relaxed))
	}

	/// Silence this source without forgetting its gain.
	pub fn set_muted(&self, muted: bool) {
		self.shared.muted.store(muted, Ordering::Relaxed);
	}

	/// Whether this source is muted.
	pub fn muted(&self) -> bool {
		self.shared.muted.load(Ordering::Relaxed)
	}

	/// Place this source between left and right: `-1.0` is hard left, `0.0`
	/// center, and `1.0` hard right.
	///
	/// A mono source keeps its loudness as it moves, at -3 dB per side when
	/// centered. Any other source is balanced: its far side is turned down and
	/// each channel stays on its side, so sound on one side only goes silent
	/// when balanced hard to the other.
	///
	/// # Errors
	///
	/// If `pan` is off center while this source has a channel open on a bus
	/// that is not stereo, which cannot pan.
	///
	/// # Panics
	///
	/// If `pan` is outside `-1.0..=1.0`.
	pub fn set_pan(&self, pan: f32) -> Result<(), Error> {
		assert!((-1.0..=1.0).contains(&pan), "invalid pan {pan}");
		self.shared
			.pan
			.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |packed| {
				let held = packed & !(CENTERED - 1);
				(held == 0 || pan == 0.0).then_some(held | u64::from(pan.to_bits()))
			})
			.map_err(|_| {
				Error::Unsupported(format!(
					"cannot pan to {pan} with a channel open on a bus that is not stereo"
				))
			})?;
		Ok(())
	}

	/// This source's pan, from `-1.0` hard left to `1.0` hard right.
	pub fn pan(&self) -> f32 {
		f32::from_bits(self.shared.pan.load(Ordering::Relaxed) as u32)
	}

	/// Delay this source in every mix it feeds, so a source that leads
	/// another (a microphone ahead of its camera) lines up with it.
	///
	/// A change inserts silence or skips audio once, so set it as calibration
	/// rather than ramp it.
	///
	/// # Panics
	///
	/// If `delay` exceeds [`MAX_DELAY`].
	pub fn set_delay(&self, delay: Duration) {
		assert!(delay <= MAX_DELAY, "delay {delay:?} exceeds {MAX_DELAY:?}");
		self.shared.delay.store(delay.as_nanos() as u64, Ordering::Relaxed);
	}

	/// This source's sync delay.
	pub fn delay(&self) -> Duration {
		Duration::from_nanos(self.shared.delay.load(Ordering::Relaxed))
	}

	/// The loudest this source contributed since the previous call, after its
	/// gain, mute, pan, and sync delay.
	pub fn level(&self) -> Level {
		self.shared.level.take()
	}

	fn target(&self) -> f32 {
		if self.muted() { 0.0 } else { self.gain() }
	}

	/// Hold this strip at center while a channel is open on a `layout` bus,
	/// which cannot pan, or refuse if it is already panned.
	fn hold_center(&self, layout: Layout) -> Result<(), Error> {
		self.shared
			.pan
			.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |packed| {
				(f32::from_bits(packed as u32) == 0.0).then_some(packed + CENTERED)
			})
			.map_err(|packed| {
				let pan = f32::from_bits(packed as u32);
				Error::Unsupported(format!("a {layout:?} bus cannot pan, and the strip is at {pan}"))
			})?;
		Ok(())
	}

	/// Release a [`hold_center`](Self::hold_center) as its channel closes.
	fn release_center(&self) {
		self.shared.pan.fetch_sub(CENTERED, Ordering::Relaxed);
	}
}

/// The loudest [`Level`] since it was last taken.
#[derive(Debug, Default)]
struct Meter(Mutex<Level>);

impl Meter {
	fn record(&self, level: Level) {
		let mut held = self.0.lock().unwrap();
		*held = held.max(level);
	}

	fn take(&self) -> Level {
		std::mem::take(&mut *self.0.lock().unwrap())
	}
}

/// A capture consumer feeding a [`Bus`], built by [`Bus::capture`].
///
/// Requires the `capture` feature.
#[cfg(feature = "capture")]
pub struct Capture {
	capture: crate::capture::Consumer,
	channel: Live,
}

#[cfg(feature = "capture")]
impl Capture {
	/// Feed the bus until shared capture stops or fails, returning its error.
	///
	/// Dropping this future removes only this bus channel.
	pub async fn run(mut self) -> Result<(), Error> {
		loop {
			let samples = self.capture.read().await?;
			self.channel.push(&samples.data);
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::Format;

	/// The default bus's 10 ms block.
	const BLOCK: usize = 480;

	/// Each side's gain for a centered mono source: -3 dB.
	const CENTER: f32 = std::f32::consts::FRAC_1_SQRT_2;

	/// Interleaved bytes of `frames` frames of `channels` channels, every sample `value`.
	fn constant(value: f32, frames: usize, channels: usize) -> Vec<u8> {
		std::iter::repeat_n(value, frames * channels)
			.flat_map(f32::to_le_bytes)
			.collect()
	}

	fn input(sample_rate: u32, layout: Layout) -> Input {
		let mut input = Input::new(sample_rate, layout);
		input.format = Format::F32;
		input
	}

	/// The block a reader already has waiting, without awaiting one.
	fn ready(reader: &mut Reader) -> Option<Block> {
		match reader.poll_read(&kio::Waiter::noop()) {
			Poll::Ready(block) => block,
			Poll::Pending => panic!("no block waiting"),
		}
	}

	/// Advance one block, then return `reader`'s copy of it.
	fn step(driver: &mut Driver, reader: &mut Reader) -> Block {
		driver.advance(BLOCK);
		ready(reader).expect("the mix is live")
	}

	/// A stereo 48 kHz source the test feeds one block per advance, the way a
	/// live source keeps pace with the clock.
	fn source(bus: &Bus) -> (Live, Strip) {
		let strip = Strip::default();
		let channel = bus.live(input(48_000, Layout::Stereo), strip.clone()).unwrap();
		(channel, strip)
	}

	/// Writes that land before the first advance are dropped, so the first
	/// advance opens the channels and the test feeds them from there. Returns
	/// each reader's last block.
	fn feed(
		driver: &mut Driver,
		sources: &mut [(&mut Live, f32)],
		readers: &mut [&mut Reader],
		blocks: usize,
	) -> Vec<Block> {
		driver.advance(BLOCK);
		let mut last = Vec::new();
		for reader in readers.iter_mut() {
			ready(reader);
		}
		for _ in 0..blocks {
			for (channel, value) in sources.iter_mut() {
				let frames = BLOCK * channel.port.input.sample_rate as usize / 48_000;
				let channels = channel.port.input.layout.channels() as usize;
				channel.write(&constant(*value, frames, channels)).unwrap();
			}
			driver.advance(BLOCK);
			last = readers.iter_mut().map(|reader| ready(reader).unwrap()).collect();
		}
		last
	}

	fn all_near(block: &Block, value: f32) -> bool {
		block.data.iter().all(|sample| (sample - value).abs() < 1e-4)
	}

	#[test]
	fn ring_allocation_follows_the_bus_rate() {
		let mut baseline = None;
		for sample_rate in [44_100, 96_000, 4_000_000_000] {
			let (bus, _driver) = Bus::new(Config::default()).unwrap();
			let (_, stats) = crate::allocation::measure(|| {
				bus.live(input(sample_rate, Layout::SevenPointOne), Strip::default())
					.unwrap()
			});
			let baseline = *baseline.get_or_insert(stats.bytes);
			assert!(
				stats.bytes <= baseline,
				"{sample_rate} Hz allocated {} bytes, baseline {baseline}",
				stats.bytes
			);
		}
	}

	#[test]
	fn sums_routed_sources_with_their_gain() {
		let (bus, mut driver) = Bus::new(Config::default()).unwrap();
		let (mut a, a_strip) = source(&bus);
		let (mut b, b_strip) = source(&bus);
		b_strip.set_gain(0.5);
		let program = bus.mix();
		program.route(&a_strip);
		program.route(&b_strip);
		let mut reader = program.reader();

		let last = feed(
			&mut driver,
			&mut [(&mut a, 0.25), (&mut b, 0.25)],
			&mut [&mut reader],
			20,
		);
		// Past the latency fill and the gain ramp, every sample is 0.25 + 0.125.
		assert!(all_near(&last[0], 0.375), "{:?}", &last[0].data[..4]);
	}

	/// Each mix sums only what is routed into it, and routing changes apply
	/// from the next block.
	#[test]
	fn each_mix_sums_only_its_routes() {
		let (bus, mut driver) = Bus::new(Config::default()).unwrap();
		let (mut voice, voice_strip) = source(&bus);
		let (mut music, music_strip) = source(&bus);
		let stream = bus.mix();
		let recording = bus.mix();
		let empty = bus.mix();
		stream.route(&voice_strip);
		recording.route(&voice_strip);
		recording.route(&music_strip);
		recording.route(&music_strip);
		assert!(recording.routes(&music_strip));
		assert!(!stream.routes(&music_strip));

		let (mut s, mut r, mut e) = (stream.reader(), recording.reader(), empty.reader());
		let mut sources = [(&mut voice, 0.25), (&mut music, 0.125)];
		let last = feed(&mut driver, &mut sources, &mut [&mut s, &mut r, &mut e], 20);
		assert!(all_near(&last[0], 0.25));
		assert!(all_near(&last[1], 0.375), "routing a strip twice counts it once");
		assert!(all_near(&last[2], 0.0));

		recording.unroute(&voice_strip);
		let last = feed(&mut driver, &mut sources, &mut [&mut s, &mut r, &mut e], 5);
		assert!(all_near(&last[0], 0.25));
		assert!(all_near(&last[1], 0.125));
	}

	/// A mix-minus is its parent without each of its sources, and follows the
	/// parent's routing as it changes.
	#[test]
	fn a_mix_minus_drops_its_sources() {
		let (bus, mut driver) = Bus::new(Config::default()).unwrap();
		let (mut host, host_strip) = source(&bus);
		let (mut camera, camera_strip) = source(&bus);
		let (mut screen, screen_strip) = source(&bus);
		let (mut music, music_strip) = source(&bus);
		let program = bus.mix();
		for strip in [&host_strip, &camera_strip, &screen_strip, &music_strip] {
			program.route(strip);
		}
		// Listing a strip twice removes it once, and the strips may come from an
		// iterator that reads the bus.
		let guest_return = program.minus(
			[&camera_strip, &screen_strip, &camera_strip]
				.into_iter()
				.filter(|strip| program.routes(strip)),
		);
		let mut minus_reader = guest_return.reader();
		let mut program_reader = program.reader();

		let mut sources = [
			(&mut host, 0.25),
			(&mut camera, 0.125),
			(&mut screen, 0.03125),
			(&mut music, 0.0625),
		];
		let mut readers = [&mut program_reader, &mut minus_reader];
		let last = feed(&mut driver, &mut sources, &mut readers, 20);
		assert!(all_near(&last[0], 0.46875));
		assert!(all_near(&last[1], 0.3125));

		program.unroute(&music_strip);
		let last = feed(&mut driver, &mut sources, &mut readers, 5);
		assert!(all_near(&last[1], 0.25));

		// Excluding a source the parent does not route leaves the rest removed.
		program.unroute(&camera_strip);
		let last = feed(&mut driver, &mut sources, &mut readers, 5);
		assert!(all_near(&last[0], 0.28125));
		assert!(all_near(&last[1], 0.25));

		// The mix-minus keeps its parent mixed after the parent's handle drops.
		drop(program);
		drop(program_reader);
		let last = feed(&mut driver, &mut sources, &mut [&mut minus_reader], 5);
		assert!(all_near(&last[0], 0.25));
	}

	/// A source's sync delay shifts it by exactly that many frames in every
	/// mix it feeds, and leaves the other sources where they were.
	#[test]
	fn sync_delay_shifts_a_source_in_every_mix() {
		let (bus, mut driver) = Bus::new(Config::default()).unwrap();
		let (mut camera, camera_strip) = source(&bus);
		let (mut mic, mic_strip) = source(&bus);
		mic_strip.set_delay(Duration::from_millis(10));
		assert_eq!(mic_strip.delay(), Duration::from_millis(10));
		let (camera_mix, stream, recording) = (bus.mix(), bus.mix(), bus.mix());
		camera_mix.route(&camera_strip);
		stream.route(&mic_strip);
		recording.route(&mic_strip);
		let mut readers = [camera_mix.reader(), stream.reader(), recording.reader()];
		let mut onsets = [None; 3];

		// Open the channels, then feed both sources from the same block on.
		driver.advance(BLOCK);
		for reader in &mut readers {
			ready(reader);
		}
		for _ in 0..20 {
			camera.write(&constant(0.25, BLOCK, 2)).unwrap();
			mic.write(&constant(0.25, BLOCK, 2)).unwrap();
			driver.advance(BLOCK);
			for (reader, onset) in readers.iter_mut().zip(&mut onsets) {
				let block = ready(reader).unwrap();
				if onset.is_none()
					&& let Some(index) = block.data.iter().position(|sample| *sample != 0.0)
				{
					*onset = Some(block.position + (index / 2) as u64);
				}
			}
		}
		let camera = onsets[0].expect("the camera is heard");
		assert_eq!(onsets[1], Some(camera + BLOCK as u64), "the mic lags by 10 ms");
		assert_eq!(onsets[2], onsets[1], "in every mix it feeds");
	}

	#[test]
	fn mute_silences_and_keeps_the_gain() {
		let (bus, mut driver) = Bus::new(Config::default()).unwrap();
		let (mut channel, strip) = source(&bus);
		let program = bus.mix();
		program.route(&strip);
		let mut reader = program.reader();
		strip.set_gain(0.8);
		strip.set_muted(true);

		let last = feed(&mut driver, &mut [(&mut channel, 0.5)], &mut [&mut reader], 20);
		assert!(all_near(&last[0], 0.0));
		assert_eq!(strip.gain(), 0.8);
		assert_eq!(strip.level(), Level::default());

		strip.set_muted(false);
		let last = feed(&mut driver, &mut [(&mut channel, 0.5)], &mut [&mut reader], 20);
		assert!(all_near(&last[0], 0.4));
	}

	/// A gain change ramps across one block instead of stepping, which would click.
	#[test]
	fn gain_changes_ramp() {
		let (bus, mut driver) = Bus::new(Config::default()).unwrap();
		let (mut channel, strip) = source(&bus);
		let program = bus.mix();
		program.route(&strip);
		let mut reader = program.reader();
		feed(&mut driver, &mut [(&mut channel, 0.5)], &mut [&mut reader], 20);

		strip.set_gain(0.0);
		channel.write(&constant(0.5, BLOCK, 2)).unwrap();
		let block = step(&mut driver, &mut reader);

		let first = block.data[0];
		let last = block.data[block.data.len() - 1];
		assert!(first > 0.49, "the ramp starts at the old gain: {first}");
		assert!(last.abs() < 1e-6, "the ramp ends at the new gain: {last}");
		assert!(block.data.windows(2).all(|pair| pair[1] <= pair[0] + 1e-6), "monotonic");
	}

	/// A mono source plays from both speakers of a stereo bus, -3 dB each at
	/// center, and a source at another rate is resampled to the bus's rather
	/// than played fast or slow.
	#[test]
	fn converts_layout_and_rate() {
		let (bus, mut driver) = Bus::new(Config::default()).unwrap();
		let strip = Strip::default();
		let mut channel = bus.live(input(16_000, Layout::Mono), strip.clone()).unwrap();
		let program = bus.mix();
		program.route(&strip);
		let mut reader = program.reader();
		let last = feed(&mut driver, &mut [(&mut channel, 0.5)], &mut [&mut reader], 40);

		for frame in last[0].data.as_chunks::<2>().0 {
			assert!((frame[0] - 0.5 * CENTER).abs() < 1e-2, "{frame:?}");
			assert!((frame[1] - 0.5 * CENTER).abs() < 1e-2, "{frame:?}");
		}
	}

	/// A source that stops writing leaves silence, not a repeat of its last block.
	#[test]
	fn a_stalled_source_is_silent() {
		let (bus, mut driver) = Bus::new(Config::default()).unwrap();
		let (mut channel, strip) = source(&bus);
		let program = bus.mix();
		program.route(&strip);
		let mut reader = program.reader();
		feed(&mut driver, &mut [(&mut channel, 0.5)], &mut [&mut reader], 20);

		let last = feed(&mut driver, &mut [], &mut [&mut reader], 20);
		assert!(all_near(&last[0], 0.0));
	}

	#[test]
	fn dropping_a_channel_leaves_the_mix() {
		let (bus, mut driver) = Bus::new(Config::default()).unwrap();
		let (mut kept, kept_strip) = source(&bus);
		let (mut dropped, dropped_strip) = source(&bus);
		let program = bus.mix();
		program.route(&kept_strip);
		program.route(&dropped_strip);
		let mut reader = program.reader();
		feed(&mut driver, &mut [(&mut dropped, 0.5)], &mut [&mut reader], 5);
		drop(dropped);

		let last = feed(&mut driver, &mut [(&mut kept, 0.25)], &mut [&mut reader], 20);
		assert!(all_near(&last[0], 0.25));
	}

	/// The meters hold the loudest reading until read, so a UI polling slower
	/// than the clock still sees a peak.
	#[test]
	fn levels_hold_until_read() {
		let (bus, mut driver) = Bus::new(Config::default()).unwrap();
		let (mut channel, strip) = source(&bus);
		let (program, quiet) = (bus.mix(), bus.mix());
		program.route(&strip);
		let mut reader = program.reader();
		feed(&mut driver, &mut [(&mut channel, 0.5)], &mut [&mut reader], 20);
		feed(&mut driver, &mut [], &mut [&mut reader], 5);

		let level = strip.level();
		assert!((level.peak() - 0.5).abs() < 1e-3, "{level:?}");
		assert_eq!(strip.level(), Level::default(), "taken");
		assert!((program.level().peak() - 0.5).abs() < 1e-3);
		assert_eq!(quiet.level(), Level::default(), "each mix meters itself");
	}

	/// A source that reopens in another format keeps its controls and routing
	/// by passing the same strip again, which is what a decoder restart or a
	/// replugged device does.
	#[test]
	fn a_strip_outlives_its_channel() {
		let (bus, mut driver) = Bus::new(Config::default()).unwrap();
		let strip = Strip::default();
		strip.set_gain(0.5);
		let program = bus.mix();
		program.route(&strip);
		let mut reader = program.reader();
		drop(bus.live(input(48_000, Layout::Stereo), strip.clone()).unwrap());

		let mut second = bus.live(input(16_000, Layout::Mono), strip.clone()).unwrap();
		let last = feed(&mut driver, &mut [(&mut second, 0.5)], &mut [&mut reader], 40);
		let sample = *last[0].data.last().unwrap();
		assert!((sample - 0.25 * CENTER).abs() < 1e-2, "{sample}");
		assert!(strip.level().peak() > 0.2 * CENTER);
	}

	/// Readers advance independently: one that stalls loses only its own
	/// oldest audio, and the position of its next block shows how much.
	#[test]
	fn a_slow_reader_skips_only_its_own_audio() {
		let (bus, mut driver) = Bus::new(Config::default()).unwrap();
		let program = bus.mix();
		let mut fast = program.reader();
		let mut slow = program.reader();
		let mut late = None;

		// A device clock's callback sizes need not be regular.
		let sizes = [441, 512, 256, 480, 1024];
		let mut position = 0u64;
		for index in 0..100 {
			let frames = sizes[index % sizes.len()];
			driver.advance(frames);
			let block = ready(&mut fast).unwrap();
			assert_eq!(block.position, position);
			assert_eq!(block.frames(), frames);
			position += frames as u64;
			if index == 90 {
				late = Some((program.reader(), position));
			}
		}
		assert_eq!(driver.position(), position);

		// The slow reader resumes at the oldest kept block, at most 200 ms back.
		let resumed = ready(&mut slow).unwrap();
		let retention = 48_000 / 5;
		assert!(resumed.position > 0, "skipped what expired");
		assert!(position - resumed.position <= retention + 1024, "{}", resumed.position);
		let mut next = resumed.position + resumed.frames() as u64;
		while let Poll::Ready(Some(block)) = slow.poll_read(&kio::Waiter::noop()) {
			assert_eq!(block.position, next, "contiguous after the skip");
			next += block.frames() as u64;
		}
		assert_eq!(next, position);

		// A reader made mid-stream starts at the next block, not the history.
		let (mut late, start) = late.unwrap();
		assert_eq!(ready(&mut late).unwrap().position, start);
		assert!(fast.poll_read(&kio::Waiter::noop()).is_pending());
	}

	/// Dropping the driver ends every mix once its readers drain, and dropping
	/// a mix's last handle ends its readers.
	#[test]
	fn readers_end_with_their_mix() {
		let (bus, mut driver) = Bus::new(Config::default()).unwrap();
		let (kept, dropped) = (bus.mix(), bus.mix());
		let mut kept_reader = kept.reader();
		let mut dropped_reader = dropped.reader();
		driver.advance(BLOCK);
		drop(dropped);
		assert_eq!(ready(&mut dropped_reader).unwrap().position, 0, "drains first");
		assert!(ready(&mut dropped_reader).is_none());

		driver.advance(BLOCK);
		drop(driver);
		assert_eq!(ready(&mut kept_reader).unwrap().position, 0);
		assert_eq!(ready(&mut kept_reader).unwrap().position, BLOCK as u64);
		assert!(ready(&mut kept_reader).is_none());
		assert!(
			ready(&mut bus.mix().reader()).is_none(),
			"a mix made after the driver ends at once"
		);
		assert!(ready(&mut kept.minus([&Strip::default()]).reader()).is_none());
	}

	/// The bus runs on whatever clock its caller owns. Paced by `kio::clock` on
	/// tokio's paused, virtual time, it stays sample-exact against that clock
	/// however the period divides the rate, and a reader stalled on the same
	/// clock loses only its own audio.
	#[tokio::test(start_paused = true)]
	async fn a_kio_clock_paces_the_bus_sample_exact() {
		let config = Config {
			sample_rate: 44_100,
			..Default::default()
		};
		let (bus, mut driver) = Bus::new(config).unwrap();
		let strip = Strip::default();
		let program = bus.mix();
		program.route(&strip);
		let mut fast = program.reader();
		let mut slow = program.reader();

		let start = kio::clock::now();
		// 132.3 frames per tick, so the blocks alternate in size.
		let period = Duration::from_millis(3);
		// 333 ticks: 999 ms, which is 44,055.9 frames.
		let end = start.saturating_add(period * 333);
		let clock = tokio::spawn(async move {
			let mut interval = kio::clock::interval_at(start, period);
			interval.set_missed_tick_behavior(kio::clock::MissedTickBehavior::Skip);
			loop {
				let tick = interval.tick().await;
				let due = (tick.saturating_duration_since(start).as_nanos() * 44_100 / 1_000_000_000) as u64;
				driver.advance((due - driver.position()) as usize);
				if tick >= end {
					return;
				}
			}
		});

		let mut stalled = tokio::spawn(async move {
			kio::clock::sleep(Duration::from_millis(900)).await;
			let first = slow.read().await.unwrap();
			let mut next = first.position + first.frames() as u64;
			while let Some(block) = slow.read().await {
				assert_eq!(block.position, next);
				next += block.frames() as u64;
			}
			(first.position, next)
		});

		let mut next = 0;
		let mut sizes = HashSet::new();
		while let Some(block) = fast.read().await {
			assert_eq!(block.position, next, "contiguous on the clock");
			sizes.insert(block.frames());
			next += block.frames() as u64;
		}
		assert_eq!(next, 44_055, "every frame due by the last tick");
		assert_eq!(sizes, HashSet::from([132, 133]));
		clock.await.unwrap();

		let (resumed, end) = (&mut stalled).await.unwrap();
		assert_eq!(end, 44_055);
		// The stalled reader woke at 900 ms and resumed within the 200 ms kept.
		assert!(
			(44_100 * 7 / 10 - 133..=44_100 * 9 / 10).contains(&resumed),
			"{resumed}"
		);
	}

	/// A mono source is panned with constant power: all on one side at either
	/// end, -3 dB on each side at center.
	#[test]
	fn pans_a_mono_source() {
		let (bus, mut driver) = Bus::new(Config::default()).unwrap();
		let strip = Strip::default();
		let mut channel = bus.live(input(48_000, Layout::Mono), strip.clone()).unwrap();
		let program = bus.mix();
		program.route(&strip);
		let mut reader = program.reader();

		for (pan, left, right) in [(-1.0, 0.5, 0.0), (0.0, 0.5 * CENTER, 0.5 * CENTER), (1.0, 0.0, 0.5)] {
			strip.set_pan(pan).unwrap();
			assert_eq!(strip.pan(), pan);
			let last = feed(&mut driver, &mut [(&mut channel, 0.5)], &mut [&mut reader], 20);
			for frame in last[0].data.as_chunks::<2>().0 {
				assert!((frame[0] - left).abs() < 1e-4, "{pan}: {frame:?}");
				assert!((frame[1] - right).abs() < 1e-4, "{pan}: {frame:?}");
			}
		}
	}

	/// A stereo source is balanced: the far side turns down, the near side and
	/// center are left alone, and neither channel moves to the other side.
	#[test]
	fn balances_a_stereo_source() {
		let (bus, mut driver) = Bus::new(Config::default()).unwrap();
		let (mut channel, strip) = source(&bus);
		let program = bus.mix();
		program.route(&strip);
		let mut reader = program.reader();
		let frame: Vec<u8> = [0.5f32, 0.25].iter().flat_map(|sample| sample.to_le_bytes()).collect();
		let pcm = frame.repeat(BLOCK);

		// Halfway right, the far side follows the mono law scaled to unity at center.
		let half = std::f32::consts::SQRT_2 * (3.0 * std::f32::consts::FRAC_PI_8).cos();
		driver.advance(BLOCK);
		ready(&mut reader);
		for (pan, left, right) in [
			(0.0, 0.5, 0.25),
			(0.5, 0.5 * half, 0.25),
			(1.0, 0.0, 0.25),
			(-1.0, 0.5, 0.0),
		] {
			strip.set_pan(pan).unwrap();
			let mut last = None;
			for _ in 0..20 {
				channel.write(&pcm).unwrap();
				last = Some(step(&mut driver, &mut reader));
			}
			for frame in last.unwrap().data.as_chunks::<2>().0 {
				assert!((frame[0] - left).abs() < 1e-4, "{pan}: {frame:?}");
				assert!((frame[1] - right).abs() < 1e-4, "{pan}: {frame:?}");
			}
		}
	}

	/// A pan move ramps the position across one block, so a mono source sweeps
	/// from one side to the other without a click or a dip in loudness.
	#[test]
	fn pan_changes_ramp_at_constant_power() {
		let (bus, mut driver) = Bus::new(Config::default()).unwrap();
		let strip = Strip::default();
		let mut channel = bus.live(input(48_000, Layout::Mono), strip.clone()).unwrap();
		strip.set_pan(-1.0).unwrap();
		let program = bus.mix();
		program.route(&strip);
		let mut reader = program.reader();
		feed(&mut driver, &mut [(&mut channel, 0.5)], &mut [&mut reader], 20);

		strip.set_pan(1.0).unwrap();
		channel.write(&constant(0.5, BLOCK, 1)).unwrap();
		let block = step(&mut driver, &mut reader);

		let frames = block.data.as_chunks::<2>().0;
		assert!(
			frames[0][0] > 0.49 && frames[0][1] < 0.01,
			"starts hard left: {:?}",
			frames[0]
		);
		let end = frames[frames.len() - 1];
		assert!(
			end[0].abs() < 1e-6 && (end[1] - 0.5).abs() < 1e-6,
			"ends hard right: {end:?}"
		);
		for pair in frames.windows(2) {
			assert!(
				pair[1][0] <= pair[0][0] + 1e-6 && pair[1][1] + 1e-6 >= pair[0][1],
				"monotonic: {pair:?}"
			);
		}
		for frame in frames {
			let power = frame[0] * frame[0] + frame[1] * frame[1];
			assert!((power - 0.25).abs() < 1e-4, "constant power: {frame:?}");
		}
	}

	/// Mono and stereo channels sharing a strip each follow their own pan law,
	/// and closing one plays out what the sync delay holds of it before its
	/// delay line is freed.
	#[test]
	fn mixed_layouts_on_one_strip_pan_apart() {
		use std::f32::consts::{FRAC_PI_8, SQRT_2};

		let (bus, mut driver) = Bus::new(Config::default()).unwrap();
		let strip = Strip::default();
		let mut mono = bus.live(input(48_000, Layout::Mono), strip.clone()).unwrap();
		let mut stereo = bus.live(input(48_000, Layout::Stereo), strip.clone()).unwrap();
		strip.set_pan(0.5).unwrap();
		strip.set_delay(Duration::from_millis(20));
		let program = bus.mix();
		program.route(&strip);
		let mut reader = program.reader();
		let pcm: Vec<u8> = [0.5f32, 0.25].iter().flat_map(|sample| sample.to_le_bytes()).collect();
		let pcm = pcm.repeat(BLOCK);

		// Halfway right: mono 0.5 by the constant-power law, and stereo
		// (0.5, 0.25) with only its far, left side turned down.
		let angle = 3.0 * FRAC_PI_8;
		let balanced = [0.5 * SQRT_2 * angle.cos(), 0.25];
		let mixed = [0.5 * angle.cos() + balanced[0], 0.5 * angle.sin() + balanced[1]];
		let assert_frames = |block: &Block, [left, right]: [f32; 2]| {
			for frame in block.data.as_chunks::<2>().0 {
				assert!(
					(frame[0] - left).abs() < 1e-4 && (frame[1] - right).abs() < 1e-4,
					"{frame:?}"
				);
			}
		};

		driver.advance(BLOCK);
		ready(&mut reader);
		let mut last = None;
		for _ in 0..20 {
			mono.write(&constant(0.5, BLOCK, 1)).unwrap();
			stereo.write(&pcm).unwrap();
			last = Some(step(&mut driver, &mut reader));
		}
		assert_frames(&last.unwrap(), mixed);

		// The 20 ms delay still holds two blocks of the mono channel.
		drop(mono);
		let held_mono = |bus: &Bus| {
			bus.inner.mixer.lock().unwrap().sources[&key(&strip)].lanes[1]
				.delay
				.capacity()
		};
		for _ in 0..2 {
			stereo.write(&pcm).unwrap();
			assert_frames(&step(&mut driver, &mut reader), mixed);
		}
		assert_eq!(held_mono(&bus), 0, "the drained delay line is freed");
		stereo.write(&pcm).unwrap();
		assert_frames(&step(&mut driver, &mut reader), balanced);
	}

	/// A pan change reaches audio the sync delay already holds back, as gain
	/// does, rather than waiting out the delay.
	#[test]
	fn pan_applies_after_the_sync_delay() {
		let (bus, mut driver) = Bus::new(Config::default()).unwrap();
		let strip = Strip::default();
		let mut channel = bus.live(input(48_000, Layout::Mono), strip.clone()).unwrap();
		strip.set_delay(Duration::from_millis(100));
		strip.set_pan(-1.0).unwrap();
		let program = bus.mix();
		program.route(&strip);
		let mut reader = program.reader();
		feed(&mut driver, &mut [(&mut channel, 0.5)], &mut [&mut reader], 30);

		// One block ramps; the next is already hard right, though the 100 ms
		// of audio coming out of the delay went in hard left.
		strip.set_pan(1.0).unwrap();
		for _ in 0..2 {
			channel.write(&constant(0.5, BLOCK, 1)).unwrap();
			step(&mut driver, &mut reader);
		}
		channel.write(&constant(0.5, BLOCK, 1)).unwrap();
		let block = step(&mut driver, &mut reader);
		for frame in block.data.as_chunks::<2>().0 {
			assert!(frame[0].abs() < 1e-4 && (frame[1] - 0.5).abs() < 1e-4, "{frame:?}");
		}
	}

	/// A bus that is not stereo cannot pan: it refuses a panned strip, and a
	/// strip open on it refuses a pan until its last such channel closes.
	#[test]
	fn only_a_stereo_bus_pans() {
		let config = Config {
			layout: Layout::Mono,
			..Default::default()
		};
		let (bus, mut driver) = Bus::new(config).unwrap();
		let strip = Strip::default();
		strip.set_pan(0.5).unwrap();
		assert!(bus.live(input(48_000, Layout::Mono), strip.clone()).is_err());

		strip.set_pan(0.0).unwrap();
		let mut first = bus.live(input(48_000, Layout::Mono), strip.clone()).unwrap();
		let second = bus.live(input(48_000, Layout::Stereo), strip.clone()).unwrap();
		assert!(strip.set_pan(-0.5).is_err());
		assert_eq!(strip.pan(), 0.0);
		strip.set_pan(0.0).unwrap();

		// A centered mono bus plays its sources as they are, with no pan law.
		let program = bus.mix();
		program.route(&strip);
		let mut reader = program.reader();
		let last = feed(&mut driver, &mut [(&mut first, 0.5)], &mut [&mut reader], 20);
		assert!(all_near(&last[0], 0.5));

		drop(first);
		assert!(strip.set_pan(-0.5).is_err(), "one channel is still open");
		drop(second);
		strip.set_pan(-0.5).unwrap();
	}

	#[test]
	fn refuses_a_layout_without_speakers() {
		let config = Config {
			layout: Layout::Discrete(2),
			..Default::default()
		};
		assert!(Bus::new(config).is_err());

		let (bus, _driver) = Bus::new(Config::default()).unwrap();
		assert!(bus.live(input(48_000, Layout::Discrete(3)), Strip::default()).is_err());
	}

	/// A NaN survives every gain, mute included, so it is refused before it can
	/// reach the mixes the other channels share.
	#[test]
	fn refuses_non_finite_samples() {
		let (bus, _driver) = Bus::new(Config::default()).unwrap();
		let (mut channel, _) = source(&bus);
		for bad in [f32::NAN, f32::INFINITY] {
			assert!(channel.write(&constant(bad, BLOCK, 2)).is_err(), "{bad}");
		}
	}

	/// A bus-rate source at `layout` placing its writes on the bus clock.
	fn positioned(bus: &Bus, layout: Layout) -> (Positioned, Strip) {
		let strip = Strip::default();
		let channel = bus
			.positioned(input(48_000, layout), strip.clone(), Duration::ZERO)
			.unwrap();
		(channel, strip)
	}

	/// Advance `frames` in blocks of `sizes`, returning the left channel of
	/// every frame mixed, indexed from the driver's position at the call.
	fn mixed(driver: &mut Driver, reader: &mut Reader, sizes: &[usize], frames: usize) -> Vec<f32> {
		let start = driver.position();
		let mut left = Vec::with_capacity(frames);
		for size in sizes.iter().cycle() {
			if left.len() >= frames {
				break;
			}
			driver.advance((*size).min(frames - left.len()));
			let block = ready(reader).unwrap();
			assert_eq!(block.position, start + left.len() as u64, "contiguous");
			left.extend(block.data.as_chunks::<2>().0.iter().map(|frame| frame[0]));
		}
		left
	}

	/// Each write plays from the sample it names, wherever that falls in a
	/// block, however the clock's blocks are sized, and whatever the channel
	/// held before: a first write after silence included.
	#[test]
	fn a_positioned_write_plays_at_its_position() {
		let (bus, mut driver) = Bus::new(Config::default()).unwrap();
		let (mut channel, strip) = positioned(&bus, Layout::Stereo);
		let program = bus.mix();
		program.route(&strip);
		let mut reader = program.reader();
		let sizes = [441, 512, 256, 480, 1024];

		// Before the first advance, then contiguous with it.
		channel.write(100, &constant(0.25, 300, 2)).unwrap();
		channel.write(400, &constant(0.5, 100, 2)).unwrap();
		// A gap of exactly 7 frames.
		channel.write(507, &constant(0.25, 93, 2)).unwrap();
		let left = mixed(&mut driver, &mut reader, &sizes, 3_000);
		let mut want = vec![0.0; 3_000];
		want[100..400].fill(0.25);
		want[400..500].fill(0.5);
		want[507..600].fill(0.25);
		assert_eq!(left, want);

		// The channel has long run dry: a write after the silence plays at the
		// sample it names, not where the bus happens to be.
		let next = driver.position();
		channel.write(next + 333, &constant(0.25, 10, 2)).unwrap();
		let left = mixed(&mut driver, &mut reader, &sizes, 1_000);
		let onset = left.iter().position(|sample| *sample != 0.0);
		assert_eq!(onset, Some(333));
		assert_eq!(left.iter().filter(|sample| **sample != 0.0).count(), 10);

		// The strip's sync delay moves it on from there, by exactly that much.
		strip.set_delay(Duration::from_millis(10));
		let next = driver.position();
		channel.write(next + 1_200, &constant(0.25, 10, 2)).unwrap();
		let left = mixed(&mut driver, &mut reader, &sizes, 3_000);
		let onset = left.iter().position(|sample| *sample != 0.0);
		assert_eq!(onset, Some(1_200 + BLOCK));
	}

	/// A write the bus cannot place where it names fails whole, rather than
	/// being shifted or cut to fit, and leaves the channel as it was.
	#[test]
	fn a_positioned_write_fails_rather_than_moves() {
		let (bus, mut driver) = Bus::new(Config::default()).unwrap();
		let (mut channel, strip) = positioned(&bus, Layout::Stereo);
		let program = bus.mix();
		program.route(&strip);
		let mut reader = program.reader();
		let block = constant(0.25, BLOCK, 2);
		let reach = 48_000 / 5;

		driver.advance(BLOCK);
		ready(&mut reader);
		let next = BLOCK as u64;
		assert!(channel.write(next - 1, &block).is_err(), "already mixed");
		assert!(channel.write(0, &block).is_err(), "already mixed");
		channel.write(next + 100, &block).unwrap();
		assert!(channel.write(next + 100, &block).is_err(), "overlaps the last write");
		assert!(channel.write(next + 579, &block).is_err(), "overlaps by one frame");
		assert!(
			channel.write(next + 99, &constant(0.25, 1, 2)).is_err(),
			"before the last write"
		);
		let at_reach = next + reach as u64 - BLOCK as u64;
		assert!(channel.write(at_reach + 1, &block).is_err(), "ends past the reach");
		assert!(channel.write(u64::MAX - 1, &block).is_err(), "ends past the reach");
		channel.write(at_reach, &block).unwrap();

		// Only the two writes that fit play, each where it said.
		let left = mixed(&mut driver, &mut reader, &[BLOCK], reach + BLOCK);
		let heard: Vec<_> = (0..left.len()).filter(|&index| left[index] != 0.0).collect();
		let mut want: Vec<_> = (100..580).collect();
		want.extend(reach - BLOCK..reach);
		assert_eq!(heard, want);

		// The position it fails on moves with the clock.
		let next = driver.position();
		assert!(channel.write(next - 1, &block).is_err());
		channel.write(next, &block).unwrap();

		// Once the driver is gone, nothing mixes, and writes are dropped.
		drop(driver);
		channel.write(0, &block).unwrap();
	}

	/// A positioned channel takes another layout, remixed and panned as any
	/// channel is, but only the bus's rate, which its positions count.
	#[test]
	fn a_positioned_channel_takes_the_bus_rate() {
		let (bus, mut driver) = Bus::new(Config::default()).unwrap();
		assert!(
			bus.positioned(input(44_100, Layout::Stereo), Strip::default(), Duration::ZERO)
				.is_err()
		);
		assert!(
			bus.positioned(input(48_000, Layout::Discrete(3)), Strip::default(), Duration::ZERO)
				.is_err()
		);

		let (mut channel, strip) = positioned(&bus, Layout::Mono);
		let program = bus.mix();
		program.route(&strip);
		let mut reader = program.reader();
		channel.write(0, &constant(0.5, BLOCK, 1)).unwrap();
		assert!(channel.write(BLOCK as u64, &constant(f32::NAN, 1, 1)).is_err());
		let block = step(&mut driver, &mut reader);
		assert!(all_near(&block, 0.5 * CENTER), "{:?}", &block.data[..4]);
		assert!(all_near(&step(&mut driver, &mut reader), 0.0));
	}

	/// 20 ms of stereo tone starting `start` frames into it, which never
	/// crosses zero, so any silence in a mix is silence the bus wrote.
	fn tone(start: usize, frames: usize) -> Vec<u8> {
		(start..start + frames)
			.flat_map(|frame| {
				let phase = 2.0 * std::f32::consts::PI * 200.0 * frame as f32 / 48_000.0;
				[0.5 + 0.25 * phase.sin(); 2]
			})
			.flat_map(f32::to_le_bytes)
			.collect()
	}

	/// The stretched queue behind `channel`.
	fn stretched<R>(bus: &Bus, channel: &Positioned, f: impl FnOnce(&ring::Consumer) -> R) -> R {
		let mixer = bus.inner.mixer.lock().unwrap();
		let feed = mixer.sources[&key(&channel.port.strip)]
			.channels
			.iter()
			.find(|feed| feed.id == channel.port.id)
			.unwrap();
		let Queue::Stretched(stretching) = &feed.queue else {
			panic!("not stretched")
		};
		f(&stretching.ring)
	}

	/// How far `channel`'s audio trails the positions it was written at, whose
	/// last write ends at `due`: negative when ahead.
	fn lag(bus: &Bus, channel: &Positioned, due: u64, next: u64) -> i64 {
		stretched(bus, channel, |cons| cons.available() as i64) - (due - next) as i64
	}

	/// A positioned channel with a stretch bound glides onto positions that
	/// step by a jitter target's 20 ms, later and earlier, as the playback
	/// sink glides onto its clock: no silence written into the audio, none of
	/// it skipped, and back on its positions within one pitch period before
	/// each next step.
	#[test]
	fn a_stretched_channel_glides_onto_stepping_positions() {
		const FRAME: usize = 960;
		const AHEAD: u64 = 2_880;
		const PERIOD: i64 = 240;
		let (bus, mut driver) = Bus::new(Config::default()).unwrap();
		let strip = Strip::default();
		let mut channel = bus
			.positioned(input(48_000, Layout::Stereo), strip.clone(), Duration::from_secs(2))
			.unwrap();
		let program = bus.mix();
		program.route(&strip);
		let mut reader = program.reader();

		// The playout target's step at each 50 frames (one second): up, back
		// down, down again, then up twice.
		let offsets = [0, 960, 0, -960, 0, 960];
		let offset = |k: usize| offsets[k / 50] as i64;
		let total = offsets.len() * 50;
		let start = 4_800;
		let at = |k: usize| (start + (k * FRAME) as i64 + offset(k)) as u64;

		let mut left = Vec::new();
		let mut k = 0;
		let mut due = 0;
		while left.len() < at(total - 1) as usize + 48_000 {
			let next = driver.position();
			while k < total && at(k) < next + AHEAD {
				if k % 50 == 0 && k > 0 {
					// Back on its positions before the step...
					let before = lag(&bus, &channel, due, next);
					assert!(before.abs() <= PERIOD, "frame {k}: {before} off before the step");
				}
				channel.write(at(k), &tone(k * FRAME, FRAME)).unwrap();
				due = at(k) + FRAME as u64;
				if k % 50 == 0 && k > 0 {
					// ...and off them by the step right after it, rather than
					// placed there.
					let after = lag(&bus, &channel, due, next);
					let step = offset(k - 1) - offset(k);
					assert!(
						(after - step).abs() <= PERIOD,
						"frame {k}: {after} off after a {step} step"
					);
				}
				k += 1;
			}
			driver.advance(BLOCK);
			let block = ready(&mut reader).unwrap();
			left.extend(block.data.as_chunks::<2>().0.iter().map(|frame| frame[0]));
		}

		// The first write, to a dry channel, plays exactly where it said.
		let onset = left.iter().position(|sample| *sample != 0.0).unwrap();
		assert_eq!(onset, start as usize);
		// Silent only once the audio has run out, and none of it skipped.
		let end = left.iter().rposition(|sample| *sample != 0.0).unwrap() + 1;
		assert!(left[onset..end].iter().all(|sample| *sample != 0.0), "silence written");
		assert_eq!(stretched(&bus, &channel, |cons| cons.trimmed()), 0);
		// Ending on its last position, which the stretch converged on.
		assert!((end as i64 - due as i64).abs() <= PERIOD, "ends at {end}, due at {due}");
	}

	/// A stretched channel that has run dry places a write exactly, since
	/// silence plays until it anyway; one trailing its positions plays late,
	/// up to the bound, past which it skips onto its positions.
	#[test]
	fn a_stretched_channel_places_onto_dry_and_skips_past_its_bound() {
		let (bus, mut driver) = Bus::new(Config::default()).unwrap();
		let strip = Strip::default();
		let bound = Duration::from_millis(100);
		let mut channel = bus
			.positioned(input(48_000, Layout::Stereo), strip.clone(), bound)
			.unwrap();
		let program = bus.mix();
		program.route(&strip);
		let mut reader = program.reader();

		driver.advance(BLOCK);
		ready(&mut reader);
		let next = driver.position();
		channel.write(next + 333, &tone(0, 960)).unwrap();
		let left = mixed(&mut driver, &mut reader, &[BLOCK], 4_800);
		assert_eq!(left.iter().position(|sample| *sample != 0.0), Some(333));
		assert_eq!(left.iter().filter(|sample| **sample != 0.0).count(), 960);

		// Already mixed by less than the bound: plays now, late, rather than
		// failing.
		let next = driver.position();
		channel.write(next - 960, &tone(0, 960)).unwrap();
		let left = mixed(&mut driver, &mut reader, &[BLOCK], 4_800);
		assert_eq!(left.iter().position(|sample| *sample != 0.0), Some(0));
		assert_eq!(stretched(&bus, &channel, |cons| cons.trimmed()), 0);

		// Trailing by 300 ms, past the bound: skips all 300 ms onto its
		// positions.
		mixed(&mut driver, &mut reader, &[BLOCK], 48_000);
		let next = driver.position();
		channel.write(next - 14_400, &tone(0, 15_360)).unwrap();

		// The reach holds as it does for an exact channel, and a write the
		// channel has no room for fails whole.
		assert!(channel.write(next + 9_601, &tone(0, 1)).is_err(), "past the reach");
		let held = stretched(&bus, &channel, |cons| cons.available());
		assert!(channel.write(next, &tone(0, 960)).is_err(), "overflows");
		assert_eq!(stretched(&bus, &channel, |cons| cons.available()), held);

		driver.advance(BLOCK);
		ready(&mut reader);
		assert_eq!(stretched(&bus, &channel, |cons| cons.trimmed()), 14_400);
		channel.write(next, &tone(0, 960)).unwrap();
	}

	/// A stretched channel plays its writes however long the bus ran before it
	/// opened or sat idle, past the 12 hours its ring's clock spans.
	#[test]
	fn a_stretched_channel_plays_after_hours() {
		const HOURS: u64 = 48_000 * 3_600 * 13;
		let (bus, mut driver) = Bus::new(Config::default()).unwrap();
		bus.inner.mixer.lock().unwrap().position = HOURS;
		let strip = Strip::default();
		let mut channel = bus
			.positioned(input(48_000, Layout::Stereo), strip.clone(), Duration::from_secs(2))
			.unwrap();
		let program = bus.mix();
		program.route(&strip);
		let mut reader = program.reader();

		for _ in 0..2 {
			let next = driver.position();
			channel.write(next + 100, &tone(0, 960)).unwrap();
			let left = mixed(&mut driver, &mut reader, &[BLOCK], 4_800);
			assert_eq!(left.iter().position(|sample| *sample != 0.0), Some(100));
			assert_eq!(left.iter().filter(|sample| **sample != 0.0).count(), 960);
			// Idle as long again.
			bus.inner.mixer.lock().unwrap().position += HOURS;
		}
		assert_eq!(stretched(&bus, &channel, |cons| cons.trimmed()), 0);
	}

	#[test]
	fn refuses_a_stretch_past_the_limit() {
		let (bus, _driver) = Bus::new(Config::default()).unwrap();
		let input = input(48_000, Layout::Stereo);
		assert!(
			bus.positioned(input.clone(), Strip::default(), MAX_STRETCH + Duration::from_millis(1))
				.is_err()
		);
		assert!(bus.positioned(input, Strip::default(), MAX_STRETCH).is_ok());
	}

	#[test]
	#[should_panic(expected = "exceeds")]
	fn refuses_a_delay_past_the_limit() {
		Strip::default().set_delay(MAX_DELAY + Duration::from_millis(1));
	}
}
