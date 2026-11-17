//! Receive encoded video independently of the paced decoder and window.

use std::collections::{BTreeSet, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use kio::clock::Instant;
use moq_mux::container::{Frame, Read};

use super::buffer::Buffer;
use super::output::Output;
use super::window::Event;
use moq_mux::sync::Producer as Presentation;

/// A few surfaces, independent of the requested playout delay. A codec batch
/// that overflows it waits beside the queue rather than pushing out pictures
/// the window has yet to show.
pub(super) const MAX_FRAMES: usize = 3;
/// How long before the earliest owed picture is due to feed the codec.
const DECODE_AHEAD: Duration = Duration::from_millis(100);

/// What the decode loop does next.
enum Step {
	/// Sleep until the instant, or until the buffer or the window changes.
	Wait(Option<Instant>),
	Decode,
	/// The track ended: drain what the codec still holds.
	Flush,
	/// The front segment drained: start the next one.
	Next,
}

/// When the window's queue has room for another picture, or `None` if it does
/// now.
///
/// The window presents the newest due picture, so a full queue's oldest one
/// is skipped anyway once the next falls due. Making room then keeps a
/// stalled window from stalling decode without dropping a picture a live one
/// would show.
fn vacancy(queue: &VecDeque<moq_video::Frame>, presentation: &Presentation) -> Option<Instant> {
	if queue.len() < MAX_FRAMES {
		return None;
	}
	queue.get(1).and_then(|frame| presentation.due(frame.timestamp))
}

type Track = moq_mux::container::Consumer<moq_mux::catalog::hang::Container>;

/// The window's shared state and the subscription's encoded retention budget.
pub(super) struct Video<O> {
	pub presentation: Presentation,
	pub frames: Arc<Mutex<VecDeque<moq_video::Frame>>>,
	pub changed: Arc<tokio::sync::Notify>,
	pub output: O,
	pub max_delay: Duration,
	/// The playout delay the subscription was opened for; a shared delay past it grows every budget by the excess.
	pub min_delay: Duration,
	/// The rendition's catalog `delay`, held as this track's playout target.
	pub delay: tokio::sync::watch::Receiver<Option<Duration>>,
}

/// The production sink and the deterministic buffered decoder used by tests.
pub(super) trait Decoder {
	/// Submit one frame, whose pictures `receive` returns.
	async fn decode(&mut self, frame: Frame) -> anyhow::Result<()>;
	/// Mark the end of input, so `receive` returns every picture the codec holds.
	async fn flush(&mut self) -> anyhow::Result<()>;
	/// Pop the next ready picture, in display order.
	fn receive(&mut self) -> anyhow::Result<Option<moq_video::Frame>>;
}

impl Decoder for moq_video::decode::Sink {
	async fn decode(&mut self, frame: Frame) -> anyhow::Result<()> {
		Ok(self.decode(frame.payload, frame.timestamp, frame.keyframe).await?)
	}

	async fn flush(&mut self) -> anyhow::Result<()> {
		Ok(self.flush().await?)
	}

	fn receive(&mut self) -> anyhow::Result<Option<moq_video::Frame>> {
		Ok(self.receive()?)
	}
}

/// One encoder's media between declared ends. The codec drains each one before
/// decoding the next, which a fresh encoder opens with a keyframe.
#[derive(Default)]
struct Segment {
	media: Buffer,
	/// When its first picture arrived.
	arrived: Option<Instant>,
}

impl std::ops::Deref for Segment {
	type Target = Buffer;

	fn deref(&self) -> &Buffer {
		&self.media
	}
}

impl std::ops::DerefMut for Segment {
	fn deref_mut(&mut self) -> &mut Buffer {
		&mut self.media
	}
}

/// The earlier of two timestamps, `a` when they don't compare: [`moq_net::Timestamp`] has no `Ord`.
fn earlier(a: moq_net::Timestamp, b: moq_net::Timestamp) -> moq_net::Timestamp {
	if b < a { b } else { a }
}

/// A presentation timestamp, ordered by time.
///
/// [`moq_net::Timestamp`] has no `Ord`, since an unscaled one orders against no scaled one.
/// Video timestamps are scaled, and scaled ones order totally.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Ordered(moq_net::Timestamp);

impl Ord for Ordered {
	fn cmp(&self, other: &Self) -> std::cmp::Ordering {
		self.0.partial_cmp(&other.0).unwrap_or(std::cmp::Ordering::Equal)
	}
}

impl PartialOrd for Ordered {
	fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
		Some(self.cmp(other))
	}
}

impl<O: Output> Video<O> {
	/// How much longer than its own budgets video retains media once playback
	/// trails the reference by `shared`, whichever track's target set it.
	fn held(&self, shared: Duration) -> Duration {
		shared.saturating_sub(self.min_delay)
	}

	/// A subscription's budget with `held` on top of its own.
	fn budget(&self, held: Duration) -> moq_net::track::Delay {
		moq_net::track::Delay {
			min: Some(self.min_delay + held),
			max: Some(self.max_delay + held),
		}
	}

	/// Play `track` through `decoder`.
	pub async fn run<D: Decoder>(self, mut track: Track, mut decoder: D) -> anyhow::Result<()> {
		let closed = std::sync::atomic::AtomicBool::new(false);
		// One segment per encoder between declared ends, oldest first.
		let buffers = Mutex::new(VecDeque::from([Segment::default()]));
		// Signalled when a segment behind the front becomes the front.
		let advanced = tokio::sync::Notify::new();
		let role = self.presentation.track(moq_mux::container::Kind::Video);
		// A missing group stops being waited for once the clock reaches the group after it,
		// whose pictures would replace any late one it held.
		track.set_playhead(self.presentation.consume());
		let receive = async {
			// Video has no jitter estimate, so its target is the encoder delay alone,
			// which holds audio back with it.
			let mut encoder = self.delay.clone();
			let target = |delay: Option<Duration>| {
				if let Some(delay) = delay
					&& role.set_target(delay)
				{
					self.changed.notify_one();
					self.output.send(Event::Wake);
				}
			};
			target(*encoder.borrow_and_update());
			// Every budget follows the shared delay, whichever track set it, or media
			// the later schedule still waits for would be evicted first.
			let schedule = self.presentation.consume();
			let mut shared = schedule.delay();
			let mut held = Duration::ZERO;
			let follow = |track: &mut Track, held: &mut Duration, shared: Duration| {
				let next = self.held(shared);
				if next != *held {
					*held = next;
					track.set_delay(self.budget(next));
				}
			};
			follow(&mut track, &mut held, shared);
			// The group of a declared end no later group has followed yet. The media
			// buffered before the end still plays out.
			let mut end = None;

			loop {
				// Stop reading while media queued behind the front segment arrived more
				// than the age budget ago, as behind a decoder slower than real time, so
				// the subscription evicts the excess. Each segment ages out only its own
				// media, so without this the queue would grow a segment per restart.
				while buffers
					.lock()
					.unwrap()
					.iter()
					.skip(1)
					.filter(|segment| segment.oldest().is_some())
					.filter_map(|segment| segment.arrived)
					.min()
					.is_some_and(|arrived| arrived.elapsed() > self.max_delay + held)
				{
					tokio::select! {
						() = advanced.notified() => {}
						Ok(()) = encoder.changed() => target(*encoder.borrow_and_update()),
						delay = role.delay_changed(shared) => {
							shared = delay;
							follow(&mut track, &mut held, shared);
						}
					}
				}
				let read = tokio::select! {
					read = track.read() => read?,
					Ok(()) = encoder.changed() => {
						target(*encoder.borrow_and_update());
						continue;
					}
					delay = role.delay_changed(shared) => {
						shared = delay;
						follow(&mut track, &mut held, shared);
						continue;
					}
				};
				let mut buffers = buffers.lock().unwrap();
				let before = buffers.front().unwrap().generation;
				let ended = match read {
					Some(Read::Frame { group, frame }) => {
						// Only a group later than a declared end's starts the next encoder, as
						// in `@moq/watch`: the stopped encoder's own group may still follow it.
						if end.take_if(|end| group > *end).is_some() {
							// A superseded segment never receives another picture, so one that
							// eviction emptied has nothing left to decode.
							let mut index = 1;
							while index < buffers.len() {
								if buffers[index].oldest().is_some() {
									index += 1;
								} else {
									buffers.remove(index);
								}
							}
							buffers.push_back(Segment::default());
							// Another timeline: its arrivals stop holding the clock until this one.
							role.reset();
						}
						if role.received(frame.timestamp, Instant::now()) {
							self.output.send(Event::Wake);
						}
						let back = buffers.back_mut().unwrap();
						back.push(frame, self.max_delay + held);
						back.arrived.get_or_insert_with(Instant::now);
						false
					}
					// Lost media discards every picture still buffered. An end declared
					// before the loss still holds, as on the audio side: a later group comes
					// from the replacement encoder.
					Some(Read::Gap { .. }) => {
						buffers.clear();
						buffers.push_back(Segment::default());
						let front = buffers.front_mut().unwrap();
						front.generation = before + 1;
						front.ended = end.is_some();
						role.reset();
						false
					}
					Some(Read::End { group, .. }) => {
						end = Some(group);
						buffers.back_mut().unwrap().ended = true;
						false
					}
					None => {
						closed.store(true, std::sync::atomic::Ordering::Relaxed);
						buffers.back_mut().unwrap().ended = true;
						true
					}
				};
				let buffer = buffers.front().unwrap();
				if buffer.generation != before {
					self.frames.lock().unwrap().clear();
					self.output.send(Event::Wake);
				}
				tracing::trace!(
					bytes = buffer.bytes,
					frames = buffer.frames.len(),
					"buffered encoded video"
				);
				self.changed.notify_one();
				if ended {
					break;
				}
			}
			Ok::<_, anyhow::Error>(())
		};
		let decode = async {
			// The generation the decoder's output belongs to, while it holds any.
			let mut generation = None;
			// Presentation times fed to the decoder that it has not returned yet.
			let mut held = BTreeSet::new();
			// Decoded pictures waiting for room in the window's queue.
			let mut pending = VecDeque::new();
			loop {
				let (current, oldest, ended) = {
					let buffers = buffers.lock().unwrap();
					let buffer = buffers.front().unwrap();
					(buffer.generation, buffer.oldest(), buffer.ended)
				};
				if generation.is_some_and(|generation| generation != current) {
					// Whatever the codec still holds sits below the new floor, so it is
					// filtered on the way out and owes the window nothing.
					generation = None;
					held.clear();
					pending.clear();
				}
				let step = {
					let mut queue = self.frames.lock().unwrap();
					let presentation = &self.presentation;
					let now = Instant::now();
					let mut placed = false;
					while !pending.is_empty() && vacancy(&queue, presentation).is_none_or(|at| at <= now) {
						if queue.len() >= MAX_FRAMES {
							queue.pop_front();
						}
						let frame: moq_video::Frame = pending.pop_front().expect("checked by the loop");
						let index = queue.partition_point(|queued| queued.timestamp <= frame.timestamp);
						queue.insert(index, frame);
						placed = true;
					}
					if placed {
						self.output.send(Event::Wake);
					}
					if !pending.is_empty() {
						Step::Wait(vacancy(&queue, presentation))
					} else if let Some(oldest) = oldest {
						// Every access unit up to the earliest picture still owed must be
						// decoded before that picture is due, however deep the stream
						// reorders or the codec holds pictures back.
						let owed = held.first().map_or(oldest, |held: &Ordered| earlier(oldest, held.0));
						let at = presentation
							.due(owed)
							.and_then(|at| at.checked_sub(DECODE_AHEAD))
							.max(vacancy(&queue, presentation));
						match at.filter(|at| *at > now) {
							Some(at) => Step::Wait(Some(at)),
							None => Step::Decode,
						}
					} else if !ended {
						Step::Wait(None)
					} else if generation.is_some() {
						Step::Flush
					} else if closed.load(std::sync::atomic::Ordering::Relaxed) && buffers.lock().unwrap().len() == 1 {
						break;
					} else {
						Step::Next
					}
				};
				match step {
					Step::Wait(Some(at)) => {
						tokio::select! {
							_ = kio::clock::sleep_until(at) => {},
							_ = self.changed.notified() => {},
						}
						continue;
					}
					Step::Wait(None) => {
						self.changed.notified().await;
						continue;
					}
					Step::Decode => {
						let frame = buffers
							.lock()
							.unwrap()
							.front_mut()
							.unwrap()
							.pop()
							.expect("no await since inspecting the buffer");
						generation = Some(current);
						held.insert(Ordered(frame.timestamp));
						// Never race a codec call: cancelling Sink::decode poisons it. The
						// joined receiver continues observing arrivals during this await.
						decoder.decode(frame).await?;
					}
					Step::Next => {
						let next = {
							let mut buffers = buffers.lock().unwrap();
							// Eviction can empty a replacement until its next keyframe; one a
							// later replacement already follows never gets another.
							while buffers.len() > 2 && buffers[1].oldest().is_none() {
								buffers.remove(1);
							}
							let next = buffers.get(1).is_some_and(|next| next.oldest().is_some());
							if next {
								buffers.pop_front();
								advanced.notify_one();
							}
							next
						};
						if !next {
							if closed.load(std::sync::atomic::Ordering::Relaxed) {
								break;
							}
							self.changed.notified().await;
						}
						continue;
					}
					Step::Flush => {
						let flushed = generation.take();
						held.clear();
						decoder.flush().await?;
						// A gap during the await already discarded these pictures' generation.
						if flushed != Some(buffers.lock().unwrap().front().unwrap().generation) {
							while decoder.receive()?.is_some() {}
							continue;
						}
					}
				}
				// A codec may return pictures it held across the keyframe that resumed a
				// skipped timeline, which no longer have a slot.
				let floor = buffers.lock().unwrap().front().unwrap().floor;
				// A picture that doesn't compare with the floor isn't known to precede it.
				let below = |at: moq_net::Timestamp| {
					floor.is_some_and(
						|floor| matches!((at.as_micros(), floor.as_micros()), (Ok(at), Ok(floor)) if at < floor),
					)
				};
				while let Some(frame) = decoder.receive()? {
					// A codec returns display order, so a picture held before one it
					// returned was dropped rather than delayed.
					held.retain(|held| held.0 > frame.timestamp);
					if !below(frame.timestamp) {
						pending.push_back(frame);
					}
				}
			}
			Ok::<_, anyhow::Error>(())
		};
		tokio::try_join!(receive, decode)?;
		Ok(())
	}
}

#[cfg(test)]
mod tests {
	use super::super::{
		args::{Args, Delay},
		fake::Recorder,
		media::Media,
	};
	use super::*;
	use moq_mux::container::Container as _;

	/// A codec whose reorder buffer holds `depth` pictures and bumps the
	/// earliest one out, the way a real decoder returns display order.
	struct Buffered {
		depth: usize,
		/// How long each decode takes.
		cost: Duration,
		held: Vec<moq_video::Frame>,
		ready: VecDeque<moq_video::Frame>,
		decoded: Arc<Mutex<Vec<(moq_net::Timestamp, Instant)>>>,
		flushed: Arc<Mutex<usize>>,
	}

	impl Default for Buffered {
		fn default() -> Self {
			Self::new(1)
		}
	}

	impl Buffered {
		fn new(depth: usize) -> Self {
			Self {
				depth,
				cost: Duration::ZERO,
				held: Vec::new(),
				ready: VecDeque::new(),
				decoded: Default::default(),
				flushed: Default::default(),
			}
		}
	}

	impl Decoder for Buffered {
		async fn decode(&mut self, frame: Frame) -> anyhow::Result<()> {
			if !self.cost.is_zero() {
				kio::clock::sleep(self.cost).await;
			}
			self.decoded.lock().unwrap().push((frame.timestamp, Instant::now()));
			let surface = moq_video::Surface::rgba(&[128; 16 * 16 * 4], moq_video::Size::new(16, 16))?;
			self.held.push(moq_video::Frame::new(surface, frame.timestamp));
			self.held.sort_by(|a, b| a.timestamp.partial_cmp(&b.timestamp).unwrap());
			let bumped = self.held.len().saturating_sub(self.depth);
			self.ready.extend(self.held.drain(..bumped));
			Ok(())
		}

		async fn flush(&mut self) -> anyhow::Result<()> {
			*self.flushed.lock().unwrap() += 1;
			self.ready.extend(self.held.drain(..));
			Ok(())
		}

		fn receive(&mut self) -> anyhow::Result<Option<moq_video::Frame>> {
			Ok(self.ready.pop_front())
		}
	}

	fn media(delay: Duration) -> Media<Recorder> {
		Media {
			origin: moq_tokio::origin::spawn().consume(),
			broadcast: "room".into(),
			args: Args {
				delay: Delay::Fixed(delay),
				stretch: Duration::ZERO.into(),
				catalog_format: None,
				metrics: None,
				#[cfg(all(target_os = "linux", feature = "vaapi"))]
				vaapi_device: None,
				#[cfg(all(target_os = "linux", feature = "v4l2"))]
				v4l2_decoder: None,
				select: Default::default(),
			},
			video: Default::default(),
			presentation: Presentation::new(delay),
			playhead: Default::default(),
			drained: Default::default(),
			output: Recorder::default(),
		}
	}

	fn playback(media: &Media<Recorder>, max_delay: Duration) -> Video<Recorder> {
		Video {
			presentation: media.presentation.clone(),
			frames: media.video.clone(),
			changed: media.drained.clone(),
			output: media.output.clone(),
			max_delay,
			min_delay: max_delay,
			delay: tokio::sync::watch::channel(None).1,
		}
	}

	fn frame(ms: u64, keyframe: bool) -> Frame {
		Frame {
			timestamp: moq_net::Timestamp::from_millis(ms).unwrap(),
			duration: None,
			keyframe,
			payload: bytes::Bytes::from_static(b"encoded"),
		}
	}

	async fn track(frames: impl IntoIterator<Item = Frame>, delay: Duration) -> Track {
		let broadcast = moq_net::broadcast::Info::new().produce();
		let track = broadcast
			.create_track("video", hang::container::track_info(hang::catalog::PRIORITY.video))
			.unwrap();
		let subscriber = track
			.consume()
			.subscribe(
				moq_net::track::Subscription::default().with_delay(moq_net::track::Delay::default().with_max(delay)),
			)
			.await
			.unwrap();
		let format = moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Data);
		let mut producer = moq_mux::container::Producer::new(
			track,
			moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Data),
		);
		for frame in frames {
			producer.write(frame).unwrap();
		}
		let _finished = producer.finish().unwrap();
		moq_mux::container::Consumer::new(subscriber, format)
	}

	/// A video rendition's catalog `delay` holds audio back with it, and a rise moves both.
	#[tokio::test]
	async fn a_video_encoder_delay_delays_audio_to_match() {
		tokio::time::pause();
		let jitter = Duration::from_millis(50);
		let media = media(jitter);
		let start = Instant::now();
		let audio = media.presentation.track(moq_mux::container::Kind::Audio);
		audio.received(moq_net::Timestamp::ZERO, start);
		audio.set_target(jitter);
		assert_eq!(
			media.presentation.due(moq_net::Timestamp::ZERO),
			start.checked_add(jitter)
		);

		let (encoder, delay) = tokio::sync::watch::channel(Some(Duration::from_millis(200)));
		let budget = Duration::from_secs(1);
		let broadcast = moq_net::broadcast::Info::new().produce();
		let track = broadcast
			.create_track("video", hang::container::track_info(hang::catalog::PRIORITY.video))
			.unwrap();
		let subscriber = track
			.consume()
			.subscribe(
				moq_net::track::Subscription::default().with_delay(moq_net::track::Delay::default().with_max(budget)),
			)
			.await
			.unwrap();
		let format = || moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Data);
		let track_consumer = moq_mux::container::Consumer::new(subscriber, format());
		let mut producer = moq_mux::container::Producer::new(track, format());
		let settle = async || {
			for _ in 0..10 {
				tokio::task::yield_now().await;
			}
		};
		let video = Video {
			delay,
			..playback(&media, budget)
		};
		let task = tokio::spawn(video.run(track_consumer, Buffered::default()));
		producer.write(frame(0, true)).unwrap();
		settle().await;
		assert_eq!(
			media.presentation.due(moq_net::Timestamp::ZERO),
			start.checked_add(Duration::from_millis(200)),
			"audio plays in sync with the delayed video"
		);

		encoder.send_replace(Some(Duration::from_millis(300)));
		producer.write(frame(33, false)).unwrap();
		settle().await;
		let clock = media.presentation.consume();
		assert_eq!(
			clock.delay(),
			Duration::from_millis(300),
			"a rising catalog delay moves the schedule"
		);

		let _finished = producer.finish().unwrap();
		task.await.unwrap().unwrap();
		assert_eq!(clock.delay(), jitter, "released with its track");
	}

	/// Audio's 5 s catalog delay sets the shared schedule while video states none,
	/// so video keeps every picture that schedule still waits for, well past its own
	/// 2 s budget, rather than evicting them and freezing.
	#[tokio::test]
	async fn an_audio_encoder_delay_grows_video_retention() {
		tokio::time::pause();
		let (min, max) = (Duration::from_millis(100), Duration::from_secs(2));
		let shared = Duration::from_secs(5);
		let media = media(min);
		let audio = media.presentation.track(moq_mux::container::Kind::Audio);
		audio.set_target(shared);

		let broadcast = moq_net::broadcast::Info::new().produce();
		let source = broadcast
			.create_track("video", hang::container::track_info(hang::catalog::PRIORITY.video))
			.unwrap();
		let subscriber = source
			.consume()
			.subscribe(
				moq_net::track::Subscription::default().with_delay(moq_net::track::Delay {
					min: Some(min),
					max: Some(max),
				}),
			)
			.await
			.unwrap();
		let format = || moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Video);
		let mut producer = moq_mux::container::Producer::new(source.clone(), format());
		let track = moq_mux::container::Consumer::new(subscriber, format());
		let video = Video {
			min_delay: min,
			..playback(&media, max)
		};
		let task = tokio::spawn(video.run(track, Buffered::new(0)));

		let mut first = None;
		for at in 0..=5_500u64 {
			if at % 100 == 0 {
				producer.write(frame(at, true)).unwrap();
			}
			tokio::task::yield_now().await;
			if let Some(picture) = media.output.present(&media)
				&& first.is_none()
			{
				first = Some((picture.as_millis().unwrap(), at));
			}
			tokio::time::advance(Duration::from_millis(1)).await;
		}
		let (picture, at) = first.expect("video froze: its pictures were evicted before they were due");
		assert_eq!(picture, 0, "the first picture was kept until it was due");
		assert!((5_000..5_100).contains(&at), "played in sync with audio: {at}");
		let held = shared - min;
		assert_eq!(
			source.subscription().map(|subscription| subscription.delay),
			Some(moq_net::track::Delay {
				min: Some(min + held),
				max: Some(max + held),
			}),
			"the subscription's budgets grow with the shared delay"
		);
		let _finished = producer.finish().unwrap();
		task.await.unwrap().unwrap();
	}

	async fn burst(speaker: bool, paused_window: bool) {
		tokio::time::pause();
		let delay = Duration::from_secs(2);
		let media = media(delay);
		let start = Instant::now();
		let last = moq_net::Timestamp::from_millis(1_980).unwrap();
		// A speaker that anchored on the first frame's turn, and so trails the
		// burst's live edge by all of it.
		let audio = media.presentation.track(moq_mux::container::Kind::Audio);
		if speaker {
			audio.received(moq_net::Timestamp::ZERO, start);
		}
		let decoder = Buffered::default();
		let decoded = decoder.decoded.clone();
		let flushed = decoder.flushed.clone();
		let frames = (0..=60).map(|index| frame(index * 33, index == 0));
		let track = track(frames, delay).await;
		let task = tokio::spawn(playback(&media, delay).run(track, decoder));
		tokio::task::yield_now().await;
		// The burst is the live edge, which no speaker holds video back from.
		let last_due = start.saturating_add(delay);
		assert_eq!(media.presentation.due(last), Some(last_due));
		// Audio's own tune-in burst lands on the same edge, so the tail keeps its
		// schedule once video's track goes.
		audio.received(last, start);
		assert_eq!(media.presentation.due(last), Some(last_due));
		assert!(media.video.lock().unwrap().len() <= MAX_FRAMES);
		let mut shown = Vec::new();
		while Instant::now() <= last_due.saturating_add(Duration::from_millis(10)) {
			if (!paused_window || Instant::now() >= start.saturating_add(Duration::from_secs(1)))
				&& let Some(timestamp) = media.output.present(&media)
			{
				shown.push((timestamp, Instant::now()));
			}
			assert!(media.video.lock().unwrap().len() <= MAX_FRAMES, "unbounded raw queue");
			tokio::time::advance(Duration::from_millis(1)).await;
			tokio::task::yield_now().await;
		}
		task.await.unwrap().unwrap();
		assert_eq!(*flushed.lock().unwrap(), 1, "decoder tail was not flushed exactly once");
		let &(timestamp, at) = shown.last().expect("presented video");
		assert_eq!(
			timestamp.as_micros().unwrap(),
			last.as_micros().unwrap(),
			"decoder tail was lost"
		);
		assert!(
			at.max(last_due).duration_since(at.min(last_due)) <= Duration::from_millis(1),
			"live edge remained late: {at:?}, expected {last_due:?}"
		);
		assert!(shown.windows(2).all(|pair| pair[0].0 < pair[1].0));
		assert_eq!(decoded.lock().unwrap().len(), 61);
	}

	#[tokio::test]
	async fn video_only_burst_reaches_live_and_flushes_its_tail() {
		burst(false, false).await;
	}

	#[tokio::test]
	async fn a_stalled_presenter_does_not_stall_arrivals_or_decode() {
		burst(false, true).await;
	}

	#[tokio::test]
	async fn a_video_burst_reaches_live_past_a_trailing_speaker() {
		burst(true, false).await;
	}

	#[tokio::test]
	async fn reordered_decoder_output_is_presented_in_timestamp_order() {
		tokio::time::pause();
		let delay = Duration::from_millis(100);
		let media = media(delay);
		let track = track(
			[frame(0, true), frame(99, false), frame(33, false), frame(66, false)],
			delay,
		)
		.await;
		let task = tokio::spawn(playback(&media, delay).run(track, Buffered::default()));
		let mut shown = Vec::new();
		for _ in 0..110 {
			tokio::task::yield_now().await;
			if let Some(frame) = media.output.present(&media) {
				shown.push(frame.as_millis().unwrap());
			}
			tokio::time::advance(Duration::from_millis(1)).await;
		}
		task.await.unwrap().unwrap();
		assert_eq!(shown, [0, 33, 66, 99], "reordered output was not presented in order");
	}
	#[tokio::test]
	async fn a_gap_discards_old_pictures_and_restarts_the_delay() {
		tokio::time::pause();
		let delay = Duration::from_secs(2);
		let media = media(delay);
		let broadcast = moq_net::broadcast::Info::new().produce();
		let track = broadcast
			.create_track("video", hang::container::track_info(hang::catalog::PRIORITY.video))
			.unwrap();
		let subscriber = track
			.consume()
			.subscribe(
				moq_net::track::Subscription::default().with_delay(moq_net::track::Delay::default().with_max(delay)),
			)
			.await
			.unwrap();
		let mut producer = moq_mux::container::Producer::new(
			track,
			moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Video),
		);
		let track = moq_mux::container::Consumer::new(
			subscriber,
			moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Video),
		);
		producer.write(frame(0, true)).unwrap();
		let decoder = Buffered::default();
		let decoded = decoder.decoded.clone();
		let task = tokio::spawn(playback(&media, delay).run(track, decoder));
		tokio::task::yield_now().await;
		tokio::time::advance(delay).await;
		tokio::task::yield_now().await;
		assert_eq!(decoded.lock().unwrap().len(), 1, "old picture held inside decoder");
		producer.seek(2).unwrap();
		producer.write(frame(500, true)).unwrap();
		let _finished = producer.finish().unwrap();
		let restarted = Instant::now();
		tokio::task::yield_now().await;
		assert_eq!(
			media.presentation.due(moq_net::Timestamp::from_millis(500).unwrap()),
			Some(restarted.saturating_add(delay))
		);
		assert!(media.output.present(&media).is_none());
		tokio::time::advance(delay + Duration::from_millis(1)).await;
		task.await.unwrap().unwrap();
		let queued = media
			.video
			.lock()
			.unwrap()
			.iter()
			.map(|frame| frame.timestamp.as_millis().unwrap())
			.collect::<Vec<_>>();
		assert_eq!(queued, [500], "old decoder output crossed the discontinuity");
		assert_eq!(media.output.present(&media).unwrap().as_millis().unwrap(), 500);
	}

	#[tokio::test]
	async fn leading_pictures_are_trimmed_before_native_decode() {
		tokio::time::pause();
		let delay = Duration::from_secs(1);
		let media = media(delay);
		let broadcast = moq_net::broadcast::Info::new().produce();
		let published = broadcast
			.create_track("video", hang::container::track_info(hang::catalog::PRIORITY.video))
			.unwrap();
		let subscriber = published
			.consume()
			.subscribe(
				moq_net::track::Subscription::default().with_delay(moq_net::track::Delay::default().with_max(delay)),
			)
			.await
			.unwrap();
		let format = moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Video);
		let write = |sequence, stamps: Vec<u64>| {
			let mut group = published
				.create_group(moq_net::group::Info { start: sequence })
				.unwrap();
			let frames: Vec<_> = stamps
				.into_iter()
				.enumerate()
				.map(|(index, ms)| frame(ms, index == 0))
				.collect();
			format.write(&mut group, &frames).unwrap();
			group.finish().unwrap();
		};
		write(10, vec![100, 80, 90, 110]);
		write(11, vec![200, 180, 190, 210]);
		let track = moq_mux::container::Consumer::new(
			subscriber,
			moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Video),
		);
		let decoder = Buffered::new(0);
		let decoded = decoder.decoded.clone();
		let task = tokio::spawn(playback(&media, delay).run(track, decoder));
		for _ in 0..1300 {
			tokio::task::yield_now().await;
			media.output.present(&media);
			tokio::time::advance(Duration::from_millis(1)).await;
		}
		assert_eq!(
			decoded.lock().unwrap().len(),
			6,
			"continuous media reached the codec before the later delivery hole"
		);
		write(13, vec![400, 380, 390, 410]);
		let _published = published.finish().unwrap();
		for _ in 0..1300 {
			tokio::task::yield_now().await;
			media.output.present(&media);
			tokio::time::advance(Duration::from_millis(1)).await;
		}
		task.await.unwrap().unwrap();
		let got: Vec<_> = decoded
			.lock()
			.unwrap()
			.iter()
			.map(|(at, _)| at.as_millis().unwrap())
			.collect();
		assert_eq!(got, [100, 110, 200, 180, 190, 210, 400, 410]);
	}

	#[tokio::test]
	async fn a_declared_end_plays_out_the_pictures_before_it() {
		tokio::time::pause();
		let delay = Duration::from_secs(2);
		let media = media(delay);
		let broadcast = moq_net::broadcast::Info::new().produce();
		let track = broadcast
			.create_track("video", hang::container::track_info(hang::catalog::PRIORITY.video))
			.unwrap();
		let subscriber = track
			.consume()
			.subscribe(
				moq_net::track::Subscription::default().with_delay(moq_net::track::Delay::default().with_max(delay)),
			)
			.await
			.unwrap();
		let mut producer = moq_mux::container::Producer::new(
			track,
			moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Video),
		);
		let track = moq_mux::container::Consumer::new(
			subscriber,
			moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Video),
		);
		for ms in [0, 33, 66] {
			producer.write(frame(ms, ms == 0)).unwrap();
		}
		// The publisher pauses and sends nothing more for now.
		producer.discontinuity().unwrap();
		// A codec that holds nothing back, so every picture is owed before the track ends.
		let task = tokio::spawn(playback(&media, delay).run(track, Buffered::new(0)));
		let mut shown = Vec::new();
		for _ in 0..2_200 {
			tokio::task::yield_now().await;
			if let Some(timestamp) = media.output.present(&media) {
				shown.push(timestamp.as_millis().unwrap());
			}
			tokio::time::advance(Duration::from_millis(1)).await;
		}
		assert_eq!(shown, [0, 33, 66], "the end discarded the pictures before it");
		let _finished = producer.finish().unwrap();
		task.await.unwrap().unwrap();
	}

	#[tokio::test]
	async fn a_closed_track_queues_its_tail_without_waiting_for_it() {
		tokio::time::pause();
		let delay = Duration::from_millis(200);
		let media = media(delay);
		let broadcast = moq_net::broadcast::Info::new().produce();
		let source = broadcast
			.create_track("video", hang::container::track_info(hang::catalog::PRIORITY.video))
			.unwrap();
		let subscriber = source
			.consume()
			.subscribe(
				moq_net::track::Subscription::default().with_delay(moq_net::track::Delay::default().with_max(delay)),
			)
			.await
			.unwrap();
		let format = || moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Video);
		let mut producer = moq_mux::container::Producer::new(source, format());
		for ms in [0, 33, 66] {
			producer.write(frame(ms, ms == 0)).unwrap();
		}
		// The publisher declares an end, then the track closes.
		producer.discontinuity().unwrap();
		let _finished = producer.finish().unwrap();
		let track = moq_mux::container::Consumer::new(subscriber, format());
		// The window never presents, so the decoded pictures stay queued.
		let task = tokio::spawn(playback(&media, delay).run(track, Buffered::new(0)));
		while !task.is_finished() {
			tokio::time::advance(Duration::from_millis(1)).await;
			tokio::task::yield_now().await;
		}
		task.await.unwrap().unwrap();
		assert_eq!(media.video.lock().unwrap().len(), 3, "the pictures were presented");
		let last = media
			.presentation
			.due(moq_net::Timestamp::from_millis(66).unwrap())
			.unwrap();
		assert!(Instant::now() < last, "the track closed after its tail was due");
	}

	#[tokio::test]
	async fn an_emptied_superseded_segment_is_discarded() {
		tokio::time::pause();
		let delay = Duration::from_millis(200);
		let media = media(delay);
		let broadcast = moq_net::broadcast::Info::new().produce();
		let source = broadcast
			.create_track("video", hang::container::track_info(hang::catalog::PRIORITY.video))
			.unwrap();
		// The network keeps every group; only the player's age budget evicts.
		let subscriber = source
			.consume()
			.subscribe(
				moq_net::track::Subscription::default()
					.with_delay(moq_net::track::Delay::default().with_max(Duration::from_secs(10))),
			)
			.await
			.unwrap();
		let format = || moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Video);
		let mut producer = moq_mux::container::Producer::new(source, format());
		let track = moq_mux::container::Consumer::new(subscriber, format());
		let task = tokio::spawn(playback(&media, delay).run(track, Buffered::new(0)));
		producer.write(frame(0, true)).unwrap();
		// Each replacement is one GOP longer than the age budget, so eviction
		// takes its only keyframe and leaves it empty.
		const SEGMENTS: u64 = 10;
		for segment in 0..SEGMENTS {
			producer.discontinuity().unwrap();
			for index in 0..15 {
				producer
					.write(frame(20 + segment * 300 + index * 20, index == 0))
					.unwrap();
			}
		}
		// The emptied segments must give way to the one after them, rather than
		// leave the decoder waiting on media that never comes.
		producer.discontinuity().unwrap();
		producer.write(frame(3_040, true)).unwrap();
		let _finished = producer.finish().unwrap();
		let mut shown = None;
		for _ in 0..1_000 {
			tokio::task::yield_now().await;
			shown = media.output.present(&media).or(shown);
			tokio::time::advance(Duration::from_millis(1)).await;
		}
		task.await.unwrap().unwrap();
		assert_eq!(shown.map(|shown| shown.as_millis().unwrap()), Some(3_040));
	}

	#[tokio::test]
	async fn an_idle_decoder_wakes_for_the_next_picture() {
		tokio::time::pause();
		let delay = Duration::from_millis(200);
		let media = media(delay);
		let broadcast = moq_net::broadcast::Info::new().produce();
		let source = broadcast
			.create_track("video", hang::container::track_info(hang::catalog::PRIORITY.video))
			.unwrap();
		let subscriber = source
			.consume()
			.subscribe(
				moq_net::track::Subscription::default().with_delay(moq_net::track::Delay::default().with_max(delay)),
			)
			.await
			.unwrap();
		let format = || moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Video);
		let mut producer = moq_mux::container::Producer::new(source, format());
		let track = moq_mux::container::Consumer::new(subscriber, format());
		let decoder = Buffered::new(0);
		let decoded = decoder.decoded.clone();
		let task = tokio::spawn(playback(&media, delay).run(track, decoder));
		for ms in [0, 20, 40] {
			producer.write(frame(ms, ms == 0)).unwrap();
		}
		// Present until `count` pictures are decoded, or give up after 20 s.
		let step = async |count: usize| {
			for _ in 0..20_000 {
				if decoded.lock().unwrap().len() >= count {
					break;
				}
				tokio::task::yield_now().await;
				media.output.present(&media);
				tokio::time::advance(Duration::from_millis(1)).await;
			}
		};
		step(3).await;
		// The decoder idles.
		for _ in 0..200 {
			tokio::task::yield_now().await;
			media.output.present(&media);
			tokio::time::advance(Duration::from_millis(1)).await;
		}
		// The publisher sends one more picture after the pause.
		producer.write(frame(60, false)).unwrap();
		let sent = Instant::now();
		step(4).await;
		assert!(
			sent.elapsed() < Duration::from_millis(50),
			"the decoder missed the new picture for {:?}",
			sent.elapsed()
		);
		let _finished = producer.finish().unwrap();
		for _ in 0..500 {
			tokio::task::yield_now().await;
			media.output.present(&media);
			tokio::time::advance(Duration::from_millis(1)).await;
		}
		task.await.unwrap().unwrap();
	}

	#[tokio::test]
	async fn a_declared_end_flushes_and_plays_every_old_picture_first() {
		tokio::time::pause();
		let delay = Duration::from_millis(200);
		let media = media(delay);
		let broadcast = moq_net::broadcast::Info::new().produce();
		let track = broadcast
			.create_track("video", hang::container::track_info(hang::catalog::PRIORITY.video))
			.unwrap();
		let subscriber = track
			.consume()
			.subscribe(
				moq_net::track::Subscription::default().with_delay(moq_net::track::Delay::default().with_max(delay)),
			)
			.await
			.unwrap();
		let format = moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Video);
		let mut producer = moq_mux::container::Producer::new(
			track,
			moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Video),
		);
		// No durations: the declared end is the last picture's own timestamp.
		for ms in [0, 33, 66] {
			producer.write(frame(ms, ms == 0)).unwrap();
		}
		producer.discontinuity().unwrap();
		for ms in [100, 133, 166] {
			producer.write(frame(ms, ms == 100)).unwrap();
		}
		let _finished = producer.finish().unwrap();
		let decoder = Buffered::new(2);
		let flushed = decoder.flushed.clone();
		let task =
			tokio::spawn(playback(&media, delay).run(moq_mux::container::Consumer::new(subscriber, format), decoder));
		let mut shown = Vec::new();
		for _ in 0..1_000 {
			tokio::task::yield_now().await;
			if let Some(timestamp) = media.output.present(&media) {
				shown.push(timestamp.as_micros().unwrap());
			}
			tokio::time::advance(Duration::from_millis(1)).await;
		}
		task.await.unwrap().unwrap();
		assert_eq!(&shown[..3], [0, 33_000, 66_000]);
		assert_eq!(shown.len(), 6);
		assert!(shown[3] > shown[2], "the next segment follows every old picture");
		assert_eq!(shown[4] - shown[3], 33_000);
		assert_eq!(shown[5] - shown[4], 33_000);
		assert_eq!(
			*flushed.lock().unwrap(),
			2,
			"each encoder drains before its replacement"
		);
	}

	/// A decoder slower than real time leaves short segments queued behind the
	/// front. Each ages out only its own media, so receive stops reading once one
	/// has waited past the age budget, and the subscription evicts the excess.
	#[tokio::test]
	async fn segments_behind_a_slow_decoder_leave_the_excess_to_the_subscription() {
		tokio::time::pause();
		let delay = Duration::from_millis(200);
		let media = media(delay);
		let broadcast = moq_net::broadcast::Info::new().produce();
		let source = broadcast
			.create_track("video", hang::container::track_info(hang::catalog::PRIORITY.video))
			.unwrap();
		let subscriber = source
			.consume()
			.subscribe(
				moq_net::track::Subscription::default().with_delay(moq_net::track::Delay::default().with_max(delay)),
			)
			.await
			.unwrap();
		let format = || moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Video);
		let mut producer = Some(moq_mux::container::Producer::new(source, format()));
		let mut _finished = None;
		let track = moq_mux::container::Consumer::new(subscriber, format());
		let decoder = Buffered {
			cost: Duration::from_millis(100),
			..Buffered::new(0)
		};
		let decoded = decoder.decoded.clone();
		let task = tokio::spawn(playback(&media, delay).run(track, decoder));
		// Two-picture segments, one per restart, published live for two seconds.
		const SEGMENTS: u64 = 50;
		for at in 0..60_000u64 {
			if task.is_finished() {
				break;
			}
			if at < SEGMENTS * 40 && at % 20 == 0 {
				if at % 40 == 0 && at > 0 {
					producer.as_mut().unwrap().discontinuity().unwrap();
				}
				producer.as_mut().unwrap().write(frame(at, at % 40 == 0)).unwrap();
			} else if at == SEGMENTS * 40 {
				_finished = producer.take().map(|producer| producer.finish().unwrap());
			}
			tokio::task::yield_now().await;
			media.output.present(&media);
			tokio::time::advance(Duration::from_millis(1)).await;
		}
		task.await.unwrap().unwrap();
		let decoded = decoded.lock().unwrap().len() as u64;
		assert!(decoded > 0, "nothing played");
		assert!(
			decoded < SEGMENTS,
			"media queued behind the front past the age budget was kept, not evicted: {decoded}"
		);
	}

	/// Play one burst of 33 ms pictures, given by slot in decode order, through a
	/// codec holding `depth` pictures, and check the window shows every one on
	/// time.
	async fn schedule(order: &[u64], depth: usize) {
		tokio::time::pause();
		let delay = Duration::from_secs(2);
		let media = media(delay);
		let track = track(order.iter().map(|&slot| frame(slot * 33, slot == 0)), delay).await;
		let task = tokio::spawn(playback(&media, delay).run(track, Buffered::new(depth)));
		tokio::task::yield_now().await;
		let last = moq_net::Timestamp::from_millis(order.iter().max().unwrap() * 33).unwrap();
		let end = media.presentation.due(last).unwrap();
		let mut shown = Vec::new();
		while Instant::now() <= end.saturating_add(Duration::from_millis(10)) {
			if let Some(timestamp) = media.output.present(&media) {
				shown.push((timestamp, Instant::now()));
			}
			tokio::time::advance(Duration::from_millis(1)).await;
			tokio::task::yield_now().await;
		}
		task.await.unwrap().unwrap();
		let mut expected = order.iter().map(|slot| (slot * 33) as u128).collect::<Vec<_>>();
		expected.sort();
		assert_eq!(
			shown
				.iter()
				.map(|(timestamp, _)| timestamp.as_millis().unwrap())
				.collect::<Vec<_>>(),
			expected,
			"a picture was dropped"
		);
		let presentation = &media.presentation;
		for (timestamp, at) in shown {
			let due = presentation.due(timestamp).unwrap();
			assert!(
				at.saturating_duration_since(due) <= Duration::from_millis(1),
				"{timestamp:?} was shown {:?} late",
				at - due
			);
		}
	}

	/// A hierarchical GOP: the reference coded second is presented 264 ms after
	/// the first picture, so the B-pictures coded after it need it decoded far
	/// more than 100 ms before its own deadline.
	#[tokio::test]
	async fn reordering_deeper_than_the_decode_lead_stays_on_time() {
		schedule(&[0, 8, 4, 2, 1, 3, 6, 5, 7], 3).await;
	}

	/// A codec holding more pictures than the window's queue returns a tail
	/// larger than the queue when it is flushed.
	#[tokio::test]
	async fn a_flush_larger_than_the_queue_keeps_its_tail() {
		schedule(&(0..10).collect::<Vec<_>>(), 5).await;
	}

	/// A lost group behind media that arrives late: once the clock, which audio holds, reaches the
	/// group past the hole, video shows it rather than waiting a whole playout delay from its
	/// arrival, which would leave the picture frozen that much longer.
	#[tokio::test]
	async fn a_hole_ends_once_the_clock_reaches_the_group_past_it() {
		tokio::time::pause();
		let delay = Duration::from_millis(100);
		let media = media(delay);
		let start = Instant::now();
		// Audio arrives on time throughout, so its arrivals hold the clock.
		let audio = media.presentation.track(moq_mux::container::Kind::Audio);
		audio.received(moq_net::Timestamp::ZERO, start);
		let broadcast = moq_net::broadcast::Info::new().produce();
		let published = broadcast
			.create_track("video", hang::container::track_info(hang::catalog::PRIORITY.video))
			.unwrap();
		// The budgets `moq play` gives video: the playout delay, as the min and the max.
		let subscriber = published
			.consume()
			.subscribe(
				moq_net::track::Subscription::default()
					.with_delay(moq_net::track::Delay::default().with_min(delay).with_max(delay)),
			)
			.await
			.unwrap();
		let format = || moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Video);
		let track = moq_mux::container::Consumer::new(subscriber, format());
		let task = tokio::spawn(playback(&media, delay).run(track, Buffered::new(0)));
		// One keyframe group per 100 ms of media.
		let write = |sequence: u64| {
			let mut group = published
				.create_group(moq_net::group::Info { start: sequence })
				.unwrap();
			format().write(&mut group, &[frame(sequence * 100, true)]).unwrap();
			group.finish().unwrap();
		};
		let mut shown = Vec::new();
		// Groups 0 to 4 arrive live. Group 5 is lost, and from 6 on the path holds video 300 ms.
		for at in 0..1_200u64 {
			if at % 100 == 0 {
				let sequence = match at < 500 {
					true => Some(at / 100),
					false => (at >= 900).then(|| (at - 300) / 100),
				};
				if let Some(sequence) = sequence {
					write(sequence);
				}
			}
			tokio::task::yield_now().await;
			if let Some(timestamp) = media.output.present(&media) {
				shown.push((timestamp.as_millis().unwrap(), start.elapsed()));
			}
			tokio::time::advance(Duration::from_millis(1)).await;
		}
		task.abort();
		// Group 6 lands at 0.9 s, past its 0.7 s due time, so it shows then. Waiting the playout
		// delay from its arrival instead would skip at 1 s, when group 7 is due too and replaces it.
		let after = shown
			.iter()
			.filter(|(timestamp, _)| *timestamp >= 500)
			.copied()
			.collect::<Vec<_>>();
		assert_eq!(
			after,
			[
				(600, Duration::from_millis(900)),
				(700, Duration::from_millis(1_000)),
				(800, Duration::from_millis(1_100)),
			],
			"shown: {shown:?}"
		);
	}
}
