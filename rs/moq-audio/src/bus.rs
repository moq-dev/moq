//! Route PCM sources into several mixes, advanced on the caller's clock.
//!
//! A [`Bus`] holds sources, each a [`Channel`] fed with PCM and controlled by a
//! [`Strip`] of gain, mute, sync delay, and level meter, and [`Mix`]es, each
//! the sum of the sources routed into it. A [`Minus`] is a mix-minus: one mix
//! with one of its sources taken out, for a guest's return feed. Every mix
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
//! that absorbs its jitter, so it never drifts out of the mix.
//!
//! ```
//! # async fn example() -> Result<(), moq_audio::Error> {
//! use moq_audio::{bus, encode};
//!
//! let (bus, mut driver) = bus::Bus::new(bus::Config::default())?;
//! let strip = bus::Strip::default();
//! let mut mic = bus.channel(encode::Input::new(44_100, moq_audio::Layout::Mono), strip.clone())?;
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

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::task::{Poll, ready};
use std::time::Duration;

use crate::encode::Input;
use crate::resample::Remix;
use crate::ring;
use crate::{Error, Layout, Level};

/// Audio held between a [`Channel::write`] and the [`Driver::advance`] that mixes it.
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

/// The channels sharing one strip, as the mixer sees them.
struct Source {
	strip: Strip,
	channels: Vec<(u64, ring::Consumer)>,
	/// Gain actually applied, ramping toward the strip's target across a block.
	applied: f32,
	/// Samples held back for the strip's sync delay.
	delay: VecDeque<f32>,
	/// This source's share of the current block, after its delay and gain.
	block: Vec<f32>,
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
	excluded: Strip,
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

		let retention = (u128::from(config.sample_rate) * RETENTION.as_nanos() / 1_000_000_000) as usize;
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
	/// keeps its gain, mute, delay, meter, and routing by passing the same strip
	/// again. Channels sharing a strip are summed. The channel leaves the bus
	/// when dropped.
	pub fn channel(&self, input: Input, strip: Strip) -> Result<Channel, Error> {
		if input.sample_rate == 0 {
			return Err(Error::Unsupported("channel sample rate must be > 0".into()));
		}
		let output = self.inner.config.layout;
		let remix = if input.layout == output {
			None
		} else {
			Some(Remix::new(input.layout, output)?)
		};

		let (prod, cons) = ring::channel(
			output.channels() as usize,
			input.sample_rate,
			self.inner.config.sample_rate,
			ring::Config {
				latency: LATENCY,
				capacity: LATENCY + HEADROOM,
				// Correct drift against the clock by trimming or padding back to
				// the target depth, not by letting the ring run dry or full.
				pad: true,
				trim: true,
			},
		);

		let id = self.inner.next.fetch_add(1, Ordering::Relaxed);
		let mut mixer = self.inner.mixer.lock().unwrap();
		mixer
			.sources
			.entry(key(&strip))
			.or_insert_with(|| Source {
				strip: strip.clone(),
				channels: Vec::new(),
				applied: strip.target(),
				delay: VecDeque::new(),
				block: Vec::new(),
			})
			.channels
			.push((id, cons));
		drop(mixer);

		Ok(Channel {
			id,
			prod,
			remix,
			input,
			strip,
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
		let channel = self.channel(capture.input().clone(), strip)?;
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
	/// costs only the sources routed into it, and a mix-minus one more.
	pub fn advance(&mut self, frames: usize) {
		if frames == 0 {
			return;
		}
		let channels = self.inner.config.layout.channels() as usize;
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
			source.advance(samples, channels, sample_rate, scratch);
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
			let excluded = key(&minus.excluded);
			if parent.routed.contains(&excluded)
				&& let Some(source) = sources.get(&excluded)
			{
				for (out, sample) in buffer.iter_mut().zip(&source.block) {
					*out -= *sample;
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
	fn advance(&mut self, samples: usize, channels: usize, sample_rate: u64, scratch: &mut Vec<f32>) {
		self.block.clear();
		self.block.resize(samples, 0.0);
		let mut rings = self.channels.iter_mut();
		if let Some((_, first)) = rings.next() {
			first.read(&mut self.block);
		}
		for (_, cons) in rings {
			scratch.resize(samples, 0.0);
			cons.read(scratch);
			for (out, sample) in self.block.iter_mut().zip(scratch.iter()) {
				*out += *sample;
			}
		}

		// The sync delay: hold back exactly this many samples. A change inserts
		// silence or skips ahead once, which a calibration rarely makes.
		let delay = self.strip.delay();
		let held = (u128::from(sample_rate) * delay.as_nanos() / 1_000_000_000) as usize * channels;
		if held > 0 || !self.delay.is_empty() {
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

	/// A mix-minus: this mix without `strip`'s source, for that source's own
	/// return feed.
	///
	/// It follows this mix's routing, and costs one subtraction per block
	/// rather than a second sum. Keeps this mix alive while it lives.
	pub fn minus(&self, strip: &Strip) -> Minus {
		let (id, history, level) = match self.shared.bus.upgrade() {
			Some(bus) => {
				let id = bus.next.fetch_add(1, Ordering::Relaxed);
				let mut mixer = bus.mixer.lock().unwrap();
				let (output, history, level) = Output::new(mixer.closed);
				mixer.minuses.push(MinusState {
					id,
					parent: self.shared.slot,
					excluded: strip.clone(),
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

/// A mix-minus, built by [`Mix::minus`]: a mix without one of its sources.
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

	/// The next block, or `None` once the mix has ended and every block kept
	/// for this reader has been read.
	pub async fn read(&mut self) -> Option<Block> {
		kio::wait(|waiter| self.poll_read(waiter)).await
	}
}

/// One source's PCM on its way into a [`Bus`].
///
/// Dropping it takes this channel out of the bus.
pub struct Channel {
	id: u64,
	prod: ring::Producer,
	remix: Option<Remix>,
	input: Input,
	strip: Strip,
	bus: Weak<Inner>,
}

impl Channel {
	/// Push PCM in the format, rate, and layout this channel was created with.
	///
	/// Never blocks. Audio that arrives before the bus first advances, or that
	/// overruns the channel's buffer, is dropped to keep the mixes live.
	///
	/// Refuses a NaN or infinite sample, which would reach every mix and
	/// survive any gain, mute included.
	pub fn write(&mut self, data: &[u8]) -> Result<(), Error> {
		let pcm = self
			.input
			.format
			.as_interleaved_f32(data, self.input.layout.channels())?;
		if pcm.iter().any(|sample| !sample.is_finite()) {
			return Err(Error::Unsupported("a non-finite PCM sample".into()));
		}
		self.push(&pcm);
		Ok(())
	}

	fn push(&mut self, pcm: &[f32]) {
		match &self.remix {
			Some(remix) => {
				let _ = self.prod.push(&remix.process(pcm));
			}
			None => {
				let _ = self.prod.push(pcm);
			}
		}
	}

	/// This source's gain, mute, delay, and meter.
	pub fn strip(&self) -> &Strip {
		&self.strip
	}
}

impl Drop for Channel {
	fn drop(&mut self) {
		let Some(bus) = self.bus.upgrade() else { return };
		let mut mixer = bus.mixer.lock().unwrap();
		let key = key(&self.strip);
		if let Some(source) = mixer.sources.get_mut(&key) {
			source.channels.retain(|(id, _)| *id != self.id);
			if source.channels.is_empty() {
				mixer.sources.remove(&key);
			}
		}
	}
}

/// The gain, mute, sync delay, and meter of one source in a [`Bus`].
///
/// Cheap to clone and `Send + Sync`: clones control the same source, so a UI
/// can hold one while a task owns the [`Channel`]. Its settings apply to every
/// mix the source is routed into, from the next [`Driver::advance`], with gain
/// ramped across it.
#[derive(Clone, Debug, Default)]
pub struct Strip {
	shared: Arc<StripState>,
}

#[derive(Debug)]
struct StripState {
	/// Linear gain, as [`f32::to_bits`] since there is no atomic float.
	gain: AtomicU32,
	muted: AtomicBool,
	/// Sync delay in nanoseconds.
	delay: AtomicU64,
	level: Meter,
}

impl Default for StripState {
	fn default() -> Self {
		Self {
			gain: AtomicU32::new(1.0f32.to_bits()),
			muted: AtomicBool::new(false),
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
	/// gain and mute.
	pub fn level(&self) -> Level {
		self.shared.level.take()
	}

	fn target(&self) -> f32 {
		if self.muted() { 0.0 } else { self.gain() }
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
	channel: Channel,
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
	fn source(bus: &Bus) -> (Channel, Strip) {
		let strip = Strip::default();
		let channel = bus.channel(input(48_000, Layout::Stereo), strip.clone()).unwrap();
		(channel, strip)
	}

	/// Writes that land before the first advance are dropped, so the first
	/// advance opens the channels and the test feeds them from there. Returns
	/// each reader's last block.
	fn feed(
		driver: &mut Driver,
		sources: &mut [(&mut Channel, f32)],
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
				let frames = BLOCK * channel.input.sample_rate as usize / 48_000;
				let channels = channel.input.layout.channels() as usize;
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
				bus.channel(input(sample_rate, Layout::SevenPointOne), Strip::default())
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

	/// A mix-minus is its parent without one source, and follows the parent's
	/// routing as it changes.
	#[test]
	fn a_mix_minus_drops_one_source() {
		let (bus, mut driver) = Bus::new(Config::default()).unwrap();
		let (mut host, host_strip) = source(&bus);
		let (mut guest, guest_strip) = source(&bus);
		let (mut music, music_strip) = source(&bus);
		let program = bus.mix();
		for strip in [&host_strip, &guest_strip, &music_strip] {
			program.route(strip);
		}
		let guest_return = program.minus(&guest_strip);
		let mut minus_reader = guest_return.reader();
		let mut program_reader = program.reader();

		let mut sources = [(&mut host, 0.25), (&mut guest, 0.125), (&mut music, 0.0625)];
		let last = feed(
			&mut driver,
			&mut sources,
			&mut [&mut program_reader, &mut minus_reader],
			20,
		);
		assert!(all_near(&last[0], 0.4375));
		assert!(all_near(&last[1], 0.3125));

		program.unroute(&music_strip);
		let last = feed(
			&mut driver,
			&mut sources,
			&mut [&mut program_reader, &mut minus_reader],
			5,
		);
		assert!(all_near(&last[1], 0.25));

		// Excluding a source the parent does not route leaves the parent as is.
		program.unroute(&guest_strip);
		let last = feed(
			&mut driver,
			&mut sources,
			&mut [&mut program_reader, &mut minus_reader],
			5,
		);
		assert!(all_near(&last[0], 0.25));
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

	/// A mono source plays from both speakers of a stereo bus, and a source at
	/// another rate is resampled to the bus's rather than played fast or slow.
	#[test]
	fn converts_layout_and_rate() {
		let (bus, mut driver) = Bus::new(Config::default()).unwrap();
		let strip = Strip::default();
		let mut channel = bus.channel(input(16_000, Layout::Mono), strip.clone()).unwrap();
		let program = bus.mix();
		program.route(&strip);
		let mut reader = program.reader();
		let last = feed(&mut driver, &mut [(&mut channel, 0.5)], &mut [&mut reader], 40);

		for frame in last[0].data.as_chunks::<2>().0 {
			assert!((frame[0] - 0.5).abs() < 1e-2, "{frame:?}");
			assert!((frame[1] - 0.5).abs() < 1e-2, "{frame:?}");
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
		drop(bus.channel(input(48_000, Layout::Stereo), strip.clone()).unwrap());

		let mut second = bus.channel(input(16_000, Layout::Mono), strip.clone()).unwrap();
		let last = feed(&mut driver, &mut [(&mut second, 0.5)], &mut [&mut reader], 40);
		let sample = *last[0].data.last().unwrap();
		assert!((sample - 0.25).abs() < 1e-2, "{sample}");
		assert!(strip.level().peak() > 0.2);
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
		assert!(ready(&mut kept.minus(&Strip::default()).reader()).is_none());
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
		let end = start + period * 333;
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

	#[test]
	fn refuses_a_layout_without_speakers() {
		let config = Config {
			layout: Layout::Discrete(2),
			..Default::default()
		};
		assert!(Bus::new(config).is_err());

		let (bus, _driver) = Bus::new(Config::default()).unwrap();
		assert!(
			bus.channel(input(48_000, Layout::Discrete(3)), Strip::default())
				.is_err()
		);
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

	#[test]
	#[should_panic(expected = "exceeds")]
	fn refuses_a_delay_past_the_limit() {
		Strip::default().set_delay(MAX_DELAY + Duration::from_millis(1));
	}
}
