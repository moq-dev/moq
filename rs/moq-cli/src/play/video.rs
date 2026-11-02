//! Receive encoded video independently of the paced decoder and window.

use std::collections::{BTreeSet, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use kio::clock::Instant;
use moq_mux::container::{Frame, Read};
use moq_video::decode::Read as Decoded;
use moq_video::decode::{Lane, Next, Selector};

use super::buffer::Buffer;
use super::output::Output;
use super::timeline::{Presentation, Role, Transition};
use super::window::Event;

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
	Resume,
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
	pub presentation: Arc<Mutex<Presentation>>,
	pub frames: Arc<Mutex<VecDeque<moq_video::Frame>>>,
	pub changed: Arc<tokio::sync::Notify>,
	pub output: O,
	pub max_delay: Duration,
}

/// The max-delay decoder of a rendition with a catalog `warmup`, and the track it
/// reads: subscribed with one more sweep of budget, starting a group earlier.
///
/// Its pictures replace the min-delay decoder's once it catches up, through the
/// same [`Selector`] `moq_video::decode::Consumer` uses. It never moves the clock.
pub(super) struct Refresh<D> {
	pub track: Track,
	pub decoder: D,
	/// The catalog's frame interval, when it declares a rate.
	pub interval: Option<Duration>,
}

/// The production sink and the deterministic buffered decoder used by tests.
pub(super) trait Decoder {
	async fn decode(&mut self, frame: Frame) -> anyhow::Result<Vec<moq_video::Frame>>;
	async fn flush(&mut self) -> anyhow::Result<Vec<moq_video::Frame>>;
}

impl Decoder for moq_video::decode::Sink {
	async fn decode(&mut self, frame: Frame) -> anyhow::Result<Vec<moq_video::Frame>> {
		Ok(self.decode(frame.payload, frame.timestamp, frame.keyframe).await?)
	}

	async fn flush(&mut self) -> anyhow::Result<Vec<moq_video::Frame>> {
		Ok(self.flush().await?)
	}
}

/// One encoder segment's media, and the transition it resumes when it
/// replaces the segment before it.
#[derive(Default)]
struct Segment {
	media: Buffer,
	resumes: Option<Transition>,
	/// When its first picture arrived.
	arrived: Option<Instant>,
	/// The group of the end it replaced: its media is in later groups only.
	after: Option<u64>,
	/// The group of its own declared end, once read.
	until: Option<u64>,
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

/// The earlier deadline, where `None` is no deadline.
fn soonest(a: Option<Instant>, b: Option<Instant>) -> Option<Instant> {
	match (a, b) {
		(Some(a), Some(b)) => Some(a.min(b)),
		(a, b) => a.or(b),
	}
}

impl<O: Output> Video<O> {
	/// Play `track`, and with `refresh` also run its max-delay decoder.
	pub async fn run<D: Decoder>(
		self,
		mut track: Track,
		mut decoder: D,
		refresh: Option<Refresh<D>>,
	) -> anyhow::Result<()> {
		let interval = refresh.as_ref().and_then(|refresh| refresh.interval);
		let selector = Mutex::new(Selector::new(refresh.is_some(), interval));
		// The newest presentation time fed to the min-delay decoder, which the
		// max-delay decoder may always decode up to.
		let edge: Mutex<Option<hang::moq_net::Timestamp>> = Mutex::new(None);
		// Wakes the max-delay decoder when the clock, the segments, or the selector change.
		let wake = tokio::sync::Notify::new();
		let closed = std::sync::atomic::AtomicBool::new(false);
		let advancing = std::sync::atomic::AtomicBool::new(false);
		let revision = std::sync::atomic::AtomicU64::new(0);
		// The first group the clock's mapping covers: the first the min-delay decoder
		// read, then the one after each end it resumed past. A gap keeps the mapping,
		// so this outlives the buffers it replaces.
		let origin: Mutex<Option<u64>> = Mutex::new(None);
		let buffers = Mutex::new(VecDeque::from([Segment::default()]));
		// Signalled when a segment held behind a resume becomes the front.
		let advanced = tokio::sync::Notify::new();
		let role = Role::new(self.presentation.clone(), super::playback::Kind::Video);
		let receive = async {
			// The group of a declared end whose restart waits for a later group, so the
			// media buffered before the end still plays out, and the transition that
			// later group resumes.
			let mut end = None;

			loop {
				// Stop reading while media queued behind a resume arrived more than the age
				// budget ago, so the subscription evicts the excess instead of the queue
				// growing a segment per restart without bound.
				while buffers
					.lock()
					.unwrap()
					.iter()
					.skip(1)
					.filter(|segment| segment.oldest().is_some())
					.filter_map(|segment| segment.arrived)
					.min()
					.is_some_and(|arrived| arrived.elapsed() > self.max_delay)
				{
					advanced.notified().await;
				}
				let read = track.read().await?;
				let mut buffers = buffers.lock().unwrap();
				let active = buffers.len() == 1 && !advancing.load(std::sync::atomic::Ordering::Relaxed);
				let before = buffers.front().unwrap().generation;
				let ended = match read {
					Some(Read::Frame { group, frame }) => {
						if let Some((ended, transition)) = end.take_if(|(end, _)| *end != group) {
							// A superseded segment never receives another picture, so one that
							// eviction emptied can never resume.
							let mut index = 1;
							while index < buffers.len() {
								if buffers[index].oldest().is_some() {
									index += 1;
								} else if let Some(skipped) = buffers.remove(index).and_then(|skipped| skipped.resumes)
								{
									role.abandon(skipped);
								}
							}
							role.plan(transition, frame.timestamp, None);
							buffers.push_back(Segment {
								resumes: Some(transition),
								after: Some(ended),
								..Default::default()
							});
						}
						if active
							&& buffers.len() == 1 && self
							.presentation
							.lock()
							.unwrap()
							.video(role.map(frame.timestamp)?, Instant::now())
						{
							self.output.send(Event::Wake);
						}
						let back = buffers.back_mut().unwrap();
						// The first segment's mapping starts where this decoder joined, so an
						// end before the join needs no resume.
						if back.resumes.is_none() && back.after.is_none() {
							origin.lock().unwrap().get_or_insert(group);
						}
						back.push(frame, self.max_delay);
						back.arrived.get_or_insert_with(Instant::now);
						false
					}
					// Applied in delivery order, so the restart lands before the frame after it.
					// An end declared before the loss still holds, as on the audio side: what
					// follows comes from the replacement encoder and resumes the latest
					// transition, while the media lost with the gap is discarded.
					Some(Read::Gap { .. }) => {
						// The mapping is unchanged, so the front keeps its groups.
						let (after, until) = buffers.front().map(|front| (front.after, front.until)).unwrap();
						let mut queued: Vec<_> = buffers
							.iter()
							.filter_map(|buffer| Some((buffer.resumes?, buffer.after)))
							.collect();
						let resumes = queued.pop();
						for (transition, _) in queued {
							role.abandon(transition);
						}
						revision.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
						role.gap();
						buffers.clear();
						buffers.push_back(Segment {
							after,
							until,
							..Default::default()
						});
						let front = buffers.front_mut().unwrap();
						front.clear();
						front.generation = before + 1;
						front.ended = resumes.is_some() || end.is_some();
						if let Some((transition, after)) = resumes {
							buffers.push_back(Segment {
								resumes: Some(transition),
								after,
								..Default::default()
							});
						}
						self.presentation.lock().unwrap().video_restarted();
						false
					}
					Some(Read::End { group, end: endpoint }) => {
						if let Some((_, transition)) = end.take() {
							role.abandon(transition);
						}
						end = Some((group, role.declare()));
						buffers.back_mut().unwrap().until = Some(group);
						buffers.back_mut().unwrap().ended = true;
						buffers.back_mut().unwrap().end = Some(endpoint);
						if active {
							role.end(endpoint)?;
						}
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
				wake.notify_one();
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
			let drained = loop {
				wake.notify_one();
				let (current, oldest, ended, last, floor) = {
					let buffers = buffers.lock().unwrap();
					let buffer = buffers.front().unwrap();
					let last = buffers.len() == 1 && closed.load(std::sync::atomic::Ordering::Relaxed);
					(buffer.generation, buffer.oldest(), buffer.ended, last, buffer.floor)
				};
				// A codec may return pictures it held across the keyframe that
				// resumed a skipped timeline. Those pictures no longer have a slot.
				let floor = floor.map(|floor| role.map(floor)).transpose()?;
				if generation.is_some_and(|generation| generation != current) {
					// Whatever the codec still holds sits below the new floor, so it is
					// filtered on the way out and owes the window nothing.
					generation = None;
					held.clear();
					pending.clear();
					selector.lock().unwrap().clear();
				}
				let step = {
					let mut queue = self.frames.lock().unwrap();
					let presentation = self.presentation.lock().unwrap();
					let now = Instant::now();
					let mut selector = selector.lock().unwrap();
					// The min-delay decoder drained its tail, so the selector waits only on
					// the max-delay one, which ends with the same track.
					if last && oldest.is_none() && ended && generation.is_none() {
						selector.finish(Lane::Min);
					}
					// When the selector next has a picture, if it is holding one back. It
					// hands over only once the window took everything it handed over before.
					let mut select = None;
					let mut done = false;
					let mut placed = false;
					loop {
						if pending.is_empty() {
							loop {
								match selector.next(now) {
									Next::Read(Decoded::Frame(frame)) => {
										if floor.is_none_or(|floor| frame.timestamp.as_micros() >= floor.as_micros()) {
											pending.push_back(frame);
										}
									}
									// Only pictures are pushed.
									Next::Read(_) => {}
									Next::Wait(at) => {
										select = at;
										break;
									}
									Next::Done => {
										done = true;
										break;
									}
								}
							}
							if pending.is_empty() {
								break;
							}
						}
						while !pending.is_empty() && vacancy(&queue, &presentation).is_none_or(|at| at <= now) {
							if queue.len() >= MAX_FRAMES {
								queue.pop_front();
							}
							let frame: moq_video::Frame = pending.pop_front().expect("checked by the loop");
							let index = queue.partition_point(|queued| queued.timestamp <= frame.timestamp);
							queue.insert(index, frame);
							placed = true;
						}
						if !pending.is_empty() {
							break;
						}
					}
					if placed {
						self.output.send(Event::Wake);
					}
					if !pending.is_empty() {
						Step::Wait(vacancy(&queue, &presentation))
					} else if let Some(oldest) = oldest {
						// Every access unit up to the earliest picture still owed must be
						// decoded before that picture is due, however deep the stream
						// reorders or the codec holds pictures back.
						let oldest = role.map(oldest)?;
						let owed = held.first().map_or(oldest, |held| oldest.min(*held));
						let at = presentation
							.due(owed)
							.and_then(|at| at.checked_sub(DECODE_AHEAD))
							.max(vacancy(&queue, &presentation));
						match at.filter(|at| *at > now) {
							Some(at) => Step::Wait(soonest(Some(at), select)),
							// Pictures the selector holds back wait for the max-delay decoder.
							None if !selector.wants(Lane::Min) => Step::Wait(select),
							None => Step::Decode,
						}
					} else if !ended {
						Step::Wait(select)
					} else if generation.is_some() {
						Step::Flush
					} else if select.is_some() || (last && !done) {
						// The segment's last pictures are still owed to the window.
						Step::Wait(select)
					} else {
						// A new encoder waits for every old picture to be presented. A closed
						// track hands its last due time to whatever resumes after it.
						if closed.load(std::sync::atomic::Ordering::Relaxed) && buffers.lock().unwrap().len() == 1 {
							break queue.back().and_then(|frame| presentation.due(frame.timestamp));
						} else if !queue.is_empty() {
							Step::Wait(
								queue
									.back()
									.and_then(|frame| presentation.due(frame.timestamp))
									.filter(|at| *at > now),
							)
						} else {
							Step::Resume
						}
					}
				};
				let frames = match step {
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
						let mut frame = buffers
							.lock()
							.unwrap()
							.front_mut()
							.unwrap()
							.pop()
							.expect("no await since inspecting the buffer");
						frame.timestamp = role.map(frame.timestamp)?;
						generation = Some(current);
						held.insert(frame.timestamp);
						{
							let mut edge = edge.lock().unwrap();
							*edge = Some(edge.map_or(frame.timestamp, |edge| edge.max(frame.timestamp)));
						}
						wake.notify_one();
						// Never race a codec call: cancelling Sink::decode poisons it. The
						// joined receiver continues observing arrivals during this await.
						decoder.decode(frame).await?
					}
					Step::Resume => {
						// Read under the same lock as the pop: a gap replaces the buffers and
						// bumps the revision while holding it.
						let next = {
							let mut buffers = buffers.lock().unwrap();
							// Eviction can empty a replacement until its next keyframe; one a
							// later replacement already follows never gets another.
							while buffers.len() > 2 && buffers[1].oldest().is_none() {
								if let Some(transition) = buffers.remove(1).and_then(|skipped| skipped.resumes) {
									role.abandon(transition);
								}
							}
							match buffers.get(1).and_then(|next| Some((next.oldest()?, next.resumes?))) {
								Some((start, transition)) => {
									advancing.store(true, std::sync::atomic::Ordering::Relaxed);
									buffers.pop_front();
									advanced.notify_one();
									Some((start, transition, revision.load(std::sync::atomic::Ordering::Relaxed)))
								}
								None => None,
							}
						};
						let Some((start, transition, before)) = next else {
							if closed.load(std::sync::atomic::Ordering::Relaxed) {
								break None;
							}
							self.changed.notified().await;
							continue;
						};
						let mapping = role.resume(transition, start, None).await?;
						let buffers = buffers.lock().unwrap();
						if revision.load(std::sync::atomic::Ordering::Relaxed) != before {
							advancing.store(false, std::sync::atomic::Ordering::Relaxed);
							continue;
						}
						role.apply(transition, mapping);
						*origin.lock().unwrap() = buffers.front().unwrap().after.map(|after| after + 1);
						if let Some(end) = buffers.front().unwrap().end {
							role.end(end)?;
						}
						advancing.store(false, std::sync::atomic::Ordering::Relaxed);
						let mut presentation = self.presentation.lock().unwrap();
						for frame in &buffers.front().unwrap().frames {
							presentation.video(role.map(frame.timestamp)?, Instant::now());
						}
						self.output.send(Event::Wake);
						continue;
					}
					Step::Flush => {
						let flushed = generation.take();
						held.clear();
						let frames = decoder.flush().await?;
						// A gap during the await already discarded these pictures' generation.
						if flushed != Some(buffers.lock().unwrap().front().unwrap().generation) {
							continue;
						}
						frames
					}
				};
				// A codec returns display order, so a picture held before the newest
				// one it returned was dropped rather than delayed.
				if let Some(last) = frames.iter().map(|frame| frame.timestamp).max() {
					held.retain(|held| *held > last);
				}
				let now = Instant::now();
				let mut selector = selector.lock().unwrap();
				for frame in frames {
					selector.push(Lane::Min, Decoded::Frame(frame), now);
				}
			};
			Ok::<_, anyhow::Error>(drained)
		};
		// Pushes the max-delay decoder's pictures to the selector. It never moves the
		// clock, and it only decodes media the min-delay decoder's segment maps.
		let refresh = async {
			let Some(Refresh {
				track: mut refresh,
				decoder: mut codec,
				..
			}) = refresh
			else {
				return std::future::pending::<anyhow::Result<()>>().await;
			};
			let push = |frames: Vec<moq_video::Frame>| {
				let now = Instant::now();
				let mut selector = selector.lock().unwrap();
				for frame in frames {
					selector.push(Lane::Max, Decoded::Frame(frame), now);
				}
				drop(selector);
				self.changed.notify_one();
			};
			// After a join or a loss, pictures presenting before the first recovery
			// point reference media this decoder never had.
			let mut recover = true;
			let mut leading = None;
			// The group of the last end this decoder read. Its own subscription may
			// deliver the replacement before the min-delay decoder reads that end.
			let mut fence = None;
			loop {
				let (group, mut frame) = match refresh.read().await? {
					Some(Read::Frame { group, frame }) => (group, frame),
					Some(Read::Gap { .. }) => {
						push(codec.flush().await?);
						recover = true;
						continue;
					}
					// A fresh encoder has no preceding GOP to reference, so no recovery.
					Some(Read::End { group, .. }) => {
						push(codec.flush().await?);
						fence = Some(group);
						continue;
					}
					None => {
						push(codec.flush().await?);
						selector.lock().unwrap().finish(Lane::Max);
						self.changed.notify_one();
						return std::future::pending::<anyhow::Result<()>>().await;
					}
				};
				if frame.keyframe {
					leading = recover.then_some(frame.timestamp);
					recover = false;
				}
				if leading.is_some_and(|floor| frame.timestamp < floor) {
					continue;
				}
				// Wait until the frame is in the segment the clock maps and due to decode:
				// not ahead of the min-delay decoder, nor of the decode lead.
				let mapped = loop {
					let woken = wake.notified();
					let at = {
						let buffers = buffers.lock().unwrap();
						let front = buffers.front().unwrap();
						// A segment the clock already moved past.
						if front.after.is_some_and(|after| group <= after) {
							break None;
						}
						// A segment the clock has not mapped yet.
						let ahead = front.until.is_some_and(|until| group > until)
							|| fence.is_some_and(|fence| {
								group > fence && origin.lock().unwrap().is_none_or(|origin| origin <= fence)
							}) || advancing.load(std::sync::atomic::Ordering::Relaxed);
						if ahead {
							// Nor ever will: the track closed without resuming it.
							if buffers.len() == 1 && closed.load(std::sync::atomic::Ordering::Relaxed) {
								break None;
							}
							None
						} else {
							drop(buffers);
							// Before the segment's mapped start, so the clock moved past it too.
							let Ok(mapped) = role.map(frame.timestamp) else {
								break None;
							};
							if !selector.lock().unwrap().wants(Lane::Max) {
								None
							} else if edge.lock().unwrap().is_some_and(|edge| mapped <= edge) {
								break Some(mapped);
							} else {
								let due = self.presentation.lock().unwrap().due(mapped);
								match due
									.and_then(|at| at.checked_sub(DECODE_AHEAD))
									.filter(|at| *at > Instant::now())
								{
									None => break Some(mapped),
									at => at,
								}
							}
						}
					};
					match at {
						Some(at) => tokio::select! {
							_ = woken => {},
							_ = kio::clock::sleep_until(at) => {},
						},
						None => woken.await,
					}
				};
				let Some(mapped) = mapped else {
					continue;
				};
				frame.timestamp = mapped;
				// Never race a codec call: cancelling Sink::decode poisons it.
				push(codec.decode(frame).await?);
			}
		};
		let decode = async {
			tokio::select! {
				drained = decode => drained,
				Err(err) = refresh => Err(err),
			}
		};
		let (_, drained) = tokio::try_join!(receive, decode)?;
		role.retire(drained);
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
	use hang::moq_net;
	use moq_mux::container::Container as _;

	/// A codec whose reorder buffer holds `depth` pictures and bumps the
	/// earliest one out, the way a real decoder returns display order.
	struct Buffered {
		depth: usize,
		/// The edge of every picture, which tells the two decoders apart.
		size: u32,
		/// How long each decode takes.
		cost: Duration,
		held: Vec<moq_video::Frame>,
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
				size: MIN,
				cost: Duration::ZERO,
				held: Vec::new(),
				decoded: Default::default(),
				flushed: Default::default(),
			}
		}
	}

	/// The min-delay decoder's picture size in tests, and the max-delay one's.
	const MIN: u32 = 16;
	const MAX: u32 = 32;

	/// The max-delay decoder of a test: [`MAX`] pictures, each decode taking `cost`.
	fn refresh_decoder(cost: Duration) -> Buffered {
		Buffered {
			size: MAX,
			cost,
			..Buffered::new(0)
		}
	}

	impl Decoder for Buffered {
		async fn decode(&mut self, frame: Frame) -> anyhow::Result<Vec<moq_video::Frame>> {
			if !self.cost.is_zero() {
				kio::clock::sleep(self.cost).await;
			}
			self.decoded.lock().unwrap().push((frame.timestamp, Instant::now()));
			let size = self.size as usize;
			let surface =
				moq_video::Surface::rgba(&vec![128; size * size * 4], moq_video::Size::new(self.size, self.size))?;
			self.held.push(moq_video::Frame::new(surface, frame.timestamp));
			self.held.sort_by_key(|frame| frame.timestamp);
			let bumped = self.held.len().saturating_sub(self.depth);
			Ok(self.held.drain(..bumped).collect())
		}

		async fn flush(&mut self) -> anyhow::Result<Vec<moq_video::Frame>> {
			*self.flushed.lock().unwrap() += 1;
			Ok(std::mem::take(&mut self.held))
		}
	}

	fn media(delay: Duration) -> Media<Recorder> {
		Media {
			origin: moq_tokio::origin::spawn().consume(),
			broadcast: "room".into(),
			args: Args {
				delay: Delay::Fixed(delay),
				catalog_format: None,
				metrics: None,
				select: Default::default(),
			},
			video: Default::default(),
			presentation: Arc::new(Mutex::new(Presentation::new(delay))),
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
			.subscribe(moq_net::track::Subscription::default().with_max_delay(delay))
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
		producer.finish().unwrap();
		moq_mux::container::Consumer::new(subscriber, format)
	}

	/// Two subscriptions to one track: the min-delay decoder's, and the max-delay
	/// one's with `warmup` more budget.
	async fn tracks(frames: impl IntoIterator<Item = Frame>, delay: Duration, warmup: Duration) -> (Track, Track) {
		let broadcast = moq_net::broadcast::Info::new().produce();
		let track = broadcast
			.create_track("video", hang::container::track_info(hang::catalog::PRIORITY.video))
			.unwrap();
		let subscribe = async |budget| {
			let subscriber = track
				.consume()
				.subscribe(moq_net::track::Subscription::default().with_max_delay(budget))
				.await
				.unwrap();
			moq_mux::container::Consumer::new(
				subscriber,
				moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Data),
			)
		};
		let (min, max) = (subscribe(delay).await, subscribe(delay + warmup).await);
		let mut producer = moq_mux::container::Producer::new(
			track,
			moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Data),
		);
		for frame in frames {
			producer.write(frame).unwrap();
		}
		producer.finish().unwrap();
		(min, max)
	}

	async fn burst(speaker: bool, paused_window: bool) {
		burst_with(speaker, paused_window, false).await;
	}

	async fn burst_with(speaker: bool, paused_window: bool, dual: bool) {
		tokio::time::pause();
		let delay = Duration::from_secs(2);
		let media = media(delay);
		let start = Instant::now();
		let last = moq_net::Timestamp::from_millis(1_980).unwrap();
		if speaker {
			media.presentation.lock().unwrap().audio(Duration::ZERO, delay, start);
		}
		let decoder = Buffered::default();
		let decoded = decoder.decoded.clone();
		let flushed = decoder.flushed.clone();
		let frames = (0..=60).map(|index| frame(index * 33, index == 0));
		let task = if dual {
			let (track, max) = tracks(frames, delay, Duration::from_secs(1)).await;
			let refresh = Refresh {
				track: max,
				decoder: refresh_decoder(Duration::ZERO),
				interval: Some(Duration::from_millis(33)),
			};
			tokio::spawn(playback(&media, delay).run(track, decoder, Some(refresh)))
		} else {
			let track = track(frames, delay).await;
			tokio::spawn(playback(&media, delay).run(track, decoder, None))
		};
		tokio::task::yield_now().await;
		let last_due = start
			+ delay + if speaker {
			Duration::from_millis(1_980)
		} else {
			Duration::ZERO
		};
		assert_eq!(media.presentation.lock().unwrap().due(last), Some(last_due));
		assert!(media.video.lock().unwrap().len() <= MAX_FRAMES);
		if speaker {
			assert!(
				decoded.lock().unwrap().is_empty(),
				"the delay was buffered as raw pictures"
			);
		}
		let mut shown = Vec::new();
		let mut sizes = Vec::new();
		while Instant::now() <= last_due + Duration::from_millis(10) {
			if (!paused_window || Instant::now() >= start + Duration::from_secs(1))
				&& let Some(picture) = media.output.present_frame(&media)
			{
				shown.push((picture.timestamp, Instant::now()));
				sizes.push(picture.size().width);
			}
			assert!(media.video.lock().unwrap().len() <= MAX_FRAMES, "unbounded raw queue");
			tokio::time::advance(Duration::from_millis(1)).await;
			tokio::task::yield_now().await;
		}
		task.await.unwrap().unwrap();
		assert_eq!(*flushed.lock().unwrap(), 1, "decoder tail was not flushed exactly once");
		let &(timestamp, at) = shown.last().expect("presented video");
		assert_eq!(timestamp.as_micros(), last.as_micros(), "decoder tail was lost");
		assert!(
			at.max(last_due).duration_since(at.min(last_due)) <= Duration::from_millis(1),
			"live edge remained late: {at:?}, expected {last_due:?}"
		);
		assert!(shown.windows(2).all(|pair| pair[0].0 < pair[1].0));
		assert_eq!(decoded.lock().unwrap().len(), 61);
		if dual {
			// Both decoders start on the same keyframe, and the max-delay one takes over
			// for good once it has decoded what the min-delay one has.
			assert!(
				sizes.iter().skip_while(|size| **size == MIN).all(|size| *size == MAX),
				"{sizes:?}"
			);
			assert_eq!(sizes.last(), Some(&MAX));
		}
	}

	/// The max-delay decoder decodes past due pictures the min-delay one also
	/// paced, without moving the clock or the schedule.
	#[tokio::test]
	async fn a_dual_burst_keeps_the_single_decoder_schedule() {
		burst_with(false, false, true).await;
	}

	#[tokio::test]
	async fn a_dual_burst_follows_the_speakers_clock() {
		burst_with(true, false, true).await;
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
	async fn a_video_burst_follows_the_speakers_clock() {
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
		let task = tokio::spawn(playback(&media, delay).run(track, Buffered::default(), None));
		let mut shown = Vec::new();
		for _ in 0..110 {
			tokio::task::yield_now().await;
			if let Some(frame) = media.output.present(&media) {
				shown.push(frame.as_millis());
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
			.subscribe(moq_net::track::Subscription::default().with_max_delay(delay))
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
		let task = tokio::spawn(playback(&media, delay).run(track, decoder, None));
		tokio::task::yield_now().await;
		tokio::time::advance(delay).await;
		tokio::task::yield_now().await;
		assert_eq!(decoded.lock().unwrap().len(), 1, "old picture held inside decoder");
		producer.seek(2).unwrap();
		producer.write(frame(500, true)).unwrap();
		producer.finish().unwrap();
		let restarted = Instant::now();
		tokio::task::yield_now().await;
		assert_eq!(
			media
				.presentation
				.lock()
				.unwrap()
				.due(moq_net::Timestamp::from_millis(500).unwrap()),
			Some(restarted + delay)
		);
		assert!(media.output.present(&media).is_none());
		tokio::time::advance(delay + Duration::from_millis(1)).await;
		task.await.unwrap().unwrap();
		let queued = media
			.video
			.lock()
			.unwrap()
			.iter()
			.map(|frame| frame.timestamp.as_millis())
			.collect::<Vec<_>>();
		assert_eq!(queued, [500], "old decoder output crossed the discontinuity");
		assert_eq!(media.output.present(&media).unwrap().as_millis(), 500);
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
			.subscribe(moq_net::track::Subscription::default().with_max_delay(delay))
			.await
			.unwrap();
		let format = moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Video);
		let write = |sequence, stamps: Vec<u64>| {
			let mut group = published.create_group(moq_net::group::Info { sequence }).unwrap();
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
		let task = tokio::spawn(playback(&media, delay).run(track, decoder, None));
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
		published.finish().unwrap();
		for _ in 0..1300 {
			tokio::task::yield_now().await;
			media.output.present(&media);
			tokio::time::advance(Duration::from_millis(1)).await;
		}
		task.await.unwrap().unwrap();
		let got: Vec<_> = decoded.lock().unwrap().iter().map(|(at, _)| at.as_millis()).collect();
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
			.subscribe(moq_net::track::Subscription::default().with_max_delay(delay))
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
		let task = tokio::spawn(playback(&media, delay).run(track, Buffered::new(0), None));
		let mut shown = Vec::new();
		for _ in 0..2_200 {
			tokio::task::yield_now().await;
			if let Some(timestamp) = media.output.present(&media) {
				shown.push(timestamp.as_millis());
			}
			tokio::time::advance(Duration::from_millis(1)).await;
		}
		assert_eq!(shown, [0, 33, 66], "the end discarded the pictures before it");
		producer.finish().unwrap();
		task.await.unwrap().unwrap();
	}

	#[tokio::test]
	async fn a_closed_track_holds_its_end_until_its_last_picture_is_due() {
		tokio::time::pause();
		let delay = Duration::from_millis(200);
		let media = media(delay);
		let broadcast = moq_net::broadcast::Info::new().produce();
		let source = broadcast
			.create_track("video", hang::container::track_info(hang::catalog::PRIORITY.video))
			.unwrap();
		let subscriber = source
			.consume()
			.subscribe(moq_net::track::Subscription::default().with_max_delay(delay))
			.await
			.unwrap();
		let format = || moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Video);
		let mut producer = moq_mux::container::Producer::new(source, format());
		for ms in [0, 33, 66] {
			producer.write(frame(ms, ms == 0)).unwrap();
		}
		// The publisher declares an end, then the track closes.
		producer.discontinuity().unwrap();
		producer.finish().unwrap();
		let track = moq_mux::container::Consumer::new(subscriber, format());
		// The window never presents, so the decoded pictures stay queued.
		let task = tokio::spawn(playback(&media, delay).run(track, Buffered::new(0), None));
		while !task.is_finished() {
			tokio::time::advance(Duration::from_millis(1)).await;
			tokio::task::yield_now().await;
		}
		task.await.unwrap().unwrap();
		assert_eq!(media.video.lock().unwrap().len(), 3, "the pictures were presented");
		let last = media
			.presentation
			.lock()
			.unwrap()
			.due(moq_net::Timestamp::from_millis(66).unwrap())
			.unwrap();
		assert!(Instant::now() < last, "the track closed after its tail was due");
		let audio = Role::new(media.presentation.clone(), super::super::playback::Kind::Audio);
		let transition = audio.declare();
		audio.resume(transition, moq_net::Timestamp::ZERO, None).await.unwrap();
		assert!(Instant::now() >= last, "audio resumed over the video tail");
	}

	#[tokio::test]
	async fn a_held_video_resume_leaves_the_excess_to_the_subscription() {
		tokio::time::pause();
		let delay = Duration::from_millis(200);
		let media = media(delay);
		let broadcast = moq_net::broadcast::Info::new().produce();
		let source = broadcast
			.create_track("video", hang::container::track_info(hang::catalog::PRIORITY.video))
			.unwrap();
		let subscriber = source
			.consume()
			.subscribe(moq_net::track::Subscription::default().with_max_delay(delay))
			.await
			.unwrap();
		let format = || moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Video);
		let mut producer = moq_mux::container::Producer::new(source, format());
		let track = moq_mux::container::Consumer::new(subscriber, format());
		let task = tokio::spawn(playback(&media, delay).run(track, Buffered::new(0), None));
		let audio = Role::new(media.presentation.clone(), super::super::playback::Kind::Audio);
		// Two-picture segments, one per restart, published live for two seconds
		// while audio's end holds the first replacement back.
		const SEGMENTS: u64 = 50;
		let mut shown = 0;
		// Presents until the track drains, which replaying every queued segment would
		// take far longer than this bound.
		for at in 0..60_000u64 {
			if task.is_finished() {
				break;
			}
			if at < 60 && at % 20 == 0 {
				producer.write(frame(at, at == 0)).unwrap();
			} else if at == 60 {
				audio.declare();
				audio.end(moq_net::Timestamp::from_millis(2_000).unwrap()).unwrap();
			}
			if (60..60 + SEGMENTS * 40).contains(&at) && at % 20 == 0 {
				if at % 40 == 20 {
					producer.discontinuity().unwrap();
				}
				producer.write(frame(at, at % 40 == 20)).unwrap();
			} else if at == 60 + SEGMENTS * 40 {
				producer.finish().unwrap();
			}
			tokio::task::yield_now().await;
			if media
				.output
				.present(&media)
				.is_some_and(|picture| picture.as_millis() >= 60)
			{
				shown += 1;
			}
			tokio::time::advance(Duration::from_millis(1)).await;
		}
		task.await.unwrap().unwrap();
		assert!(shown > 0, "the replacements played");
		assert!(
			shown < SEGMENTS,
			"pictures held behind the resume past the age budget were queued, not evicted: {shown}"
		);
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
			.subscribe(moq_net::track::Subscription::default().with_max_delay(Duration::from_secs(10)))
			.await
			.unwrap();
		let format = || moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Video);
		let mut producer = moq_mux::container::Producer::new(source, format());
		let track = moq_mux::container::Consumer::new(subscriber, format());
		let task = tokio::spawn(playback(&media, delay).run(track, Buffered::new(0), None));
		// Audio's end two seconds out holds every replacement back.
		let audio = Role::new(media.presentation.clone(), super::super::playback::Kind::Audio);
		audio.declare();
		audio.end(moq_net::Timestamp::from_millis(2_000).unwrap()).unwrap();
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
		tokio::time::advance(Duration::from_millis(500)).await;
		tokio::task::yield_now().await;
		let pending = media.presentation.lock().unwrap().pending();
		assert!(pending <= 3, "emptied segments kept their transitions: {pending}");
		producer.finish().unwrap();
		drop(audio);
		for _ in 0..1_000 {
			tokio::task::yield_now().await;
			media.output.present(&media);
			tokio::time::advance(Duration::from_millis(1)).await;
		}
		task.await.unwrap().unwrap();
	}

	#[tokio::test]
	async fn a_waiting_resume_leaves_the_decoders_wakeup_alone() {
		tokio::time::pause();
		let delay = Duration::from_millis(200);
		let media = media(delay);
		let broadcast = moq_net::broadcast::Info::new().produce();
		let source = broadcast
			.create_track("video", hang::container::track_info(hang::catalog::PRIORITY.video))
			.unwrap();
		let subscriber = source
			.consume()
			.subscribe(moq_net::track::Subscription::default().with_max_delay(delay))
			.await
			.unwrap();
		let format = || moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Video);
		let mut producer = moq_mux::container::Producer::new(source, format());
		let track = moq_mux::container::Consumer::new(subscriber, format());
		let decoder = Buffered::new(0);
		let decoded = decoder.decoded.clone();
		let task = tokio::spawn(playback(&media, delay).run(track, decoder, None));
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
		// Audio waits on an end ten seconds out while the decoder idles.
		let audio = Role::new(media.presentation.clone(), super::super::playback::Kind::Audio);
		let transition = audio.declare();
		audio.end(moq_net::Timestamp::from_secs(10).unwrap()).unwrap();
		let resume = tokio::spawn(async move {
			audio
				.resume(transition, moq_net::Timestamp::ZERO, None)
				.await
				.map(|_| ())
		});
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
		producer.finish().unwrap();
		resume.abort();
		for _ in 0..500 {
			tokio::task::yield_now().await;
			media.output.present(&media);
			tokio::time::advance(Duration::from_millis(1)).await;
		}
		task.await.unwrap().unwrap();
	}

	#[tokio::test]
	async fn a_quick_resume_flushes_and_plays_every_old_picture_first() {
		tokio::time::pause();
		let delay = Duration::from_millis(200);
		let media = media(delay);
		let broadcast = moq_net::broadcast::Info::new().produce();
		let track = broadcast
			.create_track("video", hang::container::track_info(hang::catalog::PRIORITY.video))
			.unwrap();
		let subscriber = track
			.consume()
			.subscribe(moq_net::track::Subscription::default().with_max_delay(delay))
			.await
			.unwrap();
		let format = moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Video);
		let mut producer = moq_mux::container::Producer::new(
			track,
			moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Video),
		);
		for ms in [0, 33, 66] {
			producer.write(frame(ms, ms == 0)).unwrap();
		}
		producer.discontinuity().unwrap();
		for ms in [100, 133, 166] {
			producer.write(frame(ms, ms == 100)).unwrap();
		}
		producer.finish().unwrap();
		let decoder = Buffered::new(2);
		let flushed = decoder.flushed.clone();
		let task = tokio::spawn(playback(&media, delay).run(
			moq_mux::container::Consumer::new(subscriber, format),
			decoder,
			None,
		));
		let mut shown = Vec::new();
		for _ in 0..1_000 {
			tokio::task::yield_now().await;
			if let Some(timestamp) = media.output.present(&media) {
				shown.push(timestamp.as_millis());
			}
			tokio::time::advance(Duration::from_millis(1)).await;
		}
		task.await.unwrap().unwrap();
		assert_eq!(&shown[..3], [0, 33, 66]);
		assert_eq!(shown.len(), 6);
		assert!(shown[3] > shown[2], "the resumed segment follows every old picture");
		assert_eq!(shown[4] - shown[3], 33);
		assert_eq!(shown[5] - shown[4], 33);
		assert_eq!(
			*flushed.lock().unwrap(),
			2,
			"each encoder drains before its replacement"
		);
	}

	/// Subscribe to `track` as `moq play` does, starting `lookback` groups before
	/// the live edge.
	async fn join(track: &moq_net::track::Producer, budget: Duration, lookback: u64) -> Track {
		let mut subscriber = track
			.consume()
			.subscribe(moq_net::track::Subscription::default().with_max_delay(budget))
			.await
			.unwrap();
		let largest = subscriber.largest().await.unwrap();
		subscriber = subscriber.with_range(largest.group.saturating_sub(lookback)..);
		moq_mux::container::Consumer::new(
			subscriber,
			moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Video),
		)
	}

	/// Publishes 33 ms pictures live into sweeps of `sweep` pictures, each its own
	/// group, so a test can hold one back.
	struct Live {
		track: moq_net::track::Producer,
		sweep: u64,
		groups: std::collections::BTreeMap<u64, moq_net::group::Producer>,
	}

	impl Live {
		fn new(track: moq_net::track::Producer, sweep: u64) -> Self {
			Self {
				track,
				sweep,
				groups: Default::default(),
			}
		}

		/// Publish picture `index`, the first of its sweep starting a group.
		fn write(&mut self, index: u64) {
			let sequence = index / self.sweep;
			if !self.groups.contains_key(&sequence) {
				// A publisher finishes a group when it starts the next one.
				self.seal(sequence);
			}
			let group = self
				.groups
				.entry(sequence)
				.or_insert_with(|| self.track.create_group(moq_net::group::Info { sequence }).unwrap());
			let format = moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Video);
			format
				.write(group, &[frame(index * 33, index.is_multiple_of(self.sweep))])
				.unwrap();
		}

		/// Finish every group before `sequence` still open.
		fn seal(&mut self, sequence: u64) {
			let open = self.groups.split_off(&sequence);
			for (_, mut group) in std::mem::replace(&mut self.groups, open) {
				group.finish().unwrap();
			}
		}

		fn finish(&mut self) {
			self.seal(u64::MAX);
			self.track.finish().unwrap();
		}
	}

	/// The min-delay decoder's pictures, then the max-delay decoder's for good.
	fn hands_over(sizes: &[u32]) -> bool {
		let switch = sizes.iter().position(|size| *size == MAX);
		switch.is_some_and(|switch| switch > 0 && sizes[switch..].iter().all(|size| *size == MAX))
	}

	/// Publish one picture every 33 ms from `from` until `until`, skipping
	/// `held` until `late`, presenting every millisecond. Returns what was shown as
	/// (timestamp, picture size).
	async fn live(
		media: &Media<Recorder>,
		mut live: Live,
		task: tokio::task::JoinHandle<anyhow::Result<()>>,
		pictures: std::ops::Range<u64>,
		held: std::ops::Range<u64>,
		late: Duration,
	) -> Vec<(u128, u32)> {
		let start = Instant::now();
		let mut next = pictures.start;
		let mut shown = Vec::new();
		let mut released = false;
		let mut finished = false;
		while !task.is_finished() {
			let elapsed = Instant::now() - start;
			while next < pictures.end && Duration::from_millis((next - pictures.start) * 33) <= elapsed {
				if !held.contains(&next) {
					live.write(next);
				}
				next += 1;
			}
			if !released && elapsed >= late {
				held.clone().for_each(|index| live.write(index));
				// Finished on arrival, like the groups around it.
				live.seal((next - 1) / live.sweep);
				released = true;
			}
			if next == pictures.end && released && !finished {
				live.finish();
				finished = true;
			}
			tokio::task::yield_now().await;
			if let Some(picture) = media.output.present_frame(media) {
				shown.push((picture.timestamp.as_millis(), picture.size().width));
			}
			tokio::time::advance(Duration::from_millis(1)).await;
			assert!(elapsed < Duration::from_secs(30), "playback never finished");
		}
		task.await.unwrap().unwrap();
		// The window still holds the last pictures the decoder queued.
		for _ in 0..1_000 {
			if let Some(picture) = media.output.present_frame(media) {
				shown.push((picture.timestamp.as_millis(), picture.size().width));
			}
			tokio::time::advance(Duration::from_millis(1)).await;
		}
		shown
	}

	/// A cold join shows the min-delay decoder's pictures right away, then the
	/// max-delay decoder's once it has decoded the group before the live edge and
	/// caught up.
	#[tokio::test]
	async fn a_cold_join_hands_over_to_the_max_delay_decoder() {
		tokio::time::pause();
		let delay = Duration::from_millis(200);
		let warmup = Duration::from_secs(1);
		let media = media(delay);
		let broadcast = moq_net::broadcast::Info::new().produce();
		let track = broadcast
			.create_track("video", hang::container::track_info(hang::catalog::PRIORITY.video))
			.unwrap();
		// Sweeps a second long, and the viewer joins at the start of the second.
		let mut publish = Live::new(track.clone(), 30);
		(0..=30).for_each(|index| publish.write(index));
		let min = join(&track, delay, 0).await;
		let max = join(&track, delay + warmup, 1).await;
		let refresh = Refresh {
			track: max,
			// Slower than real time to start, so the earlier sweep takes a while.
			decoder: refresh_decoder(Duration::from_millis(10)),
			interval: Some(Duration::from_millis(33)),
		};
		let task = tokio::spawn(playback(&media, delay).run(min, Buffered::new(0), Some(refresh)));
		let shown = live(&media, publish, task, 31..90, 0..0, Duration::ZERO).await;
		let sizes: Vec<_> = shown.iter().map(|(_, size)| *size).collect();
		assert_eq!(shown.first().map(|(at, _)| *at), Some(30 * 33), "{shown:?}");
		assert!(hands_over(&sizes), "no clean handover: {shown:?}");
		assert!(shown.windows(2).all(|pair| pair[0].0 < pair[1].0), "{shown:?}");
		assert_eq!(shown.last().map(|(at, _)| *at), Some(89 * 33));
	}

	/// After a loss only the min-delay decoder skips, so it shows its pictures
	/// right after the hole while the max-delay decoder catches up on the late
	/// sweep, and then hands back over. Pictures behind the playhead never show.
	#[tokio::test]
	async fn a_skip_recovers_through_the_max_delay_decoder() {
		tokio::time::pause();
		let delay = Duration::from_millis(100);
		let warmup = Duration::from_secs(1);
		let media = media(delay);
		let broadcast = moq_net::broadcast::Info::new().produce();
		let track = broadcast
			.create_track("video", hang::container::track_info(hang::catalog::PRIORITY.video))
			.unwrap();
		// Sweeps a third of a second long.
		let mut publish = Live::new(track.clone(), 10);
		publish.write(0);
		let min = join(&track, delay, 0).await;
		let max = join(&track, delay + warmup, 1).await;
		let refresh = Refresh {
			track: max,
			decoder: refresh_decoder(Duration::from_millis(10)),
			interval: Some(Duration::from_millis(33)),
		};
		let task = tokio::spawn(playback(&media, delay).run(min, Buffered::new(0), Some(refresh)));
		// The fourth sweep is held up past the min-delay budget, not the max-delay one.
		let shown = live(&media, publish, task, 1..90, 30..40, Duration::from_millis(1_500)).await;
		assert!(shown.windows(2).all(|pair| pair[0].0 < pair[1].0), "{shown:?}");
		let (before, after): (Vec<_>, Vec<_>) = shown.iter().partition(|(at, _)| *at < 30 * 33);
		assert_eq!(before.last().map(|(_, size)| *size), Some(MAX), "{shown:?}");
		assert!(
			after.iter().all(|(at, _)| *at >= 40 * 33),
			"the late sweep showed behind the playhead: {shown:?}"
		);
		let sizes: Vec<_> = after.iter().map(|(_, size)| *size).collect();
		assert!(hands_over(&sizes), "no clean handover after the skip: {shown:?}");
	}

	/// A join right after an encoder restart starts the max-delay decoder on the end,
	/// a group before the min-delay one. The clock already maps the replacement, so
	/// that end holds nothing back.
	#[tokio::test]
	async fn a_join_right_after_a_restart_still_hands_over() {
		tokio::time::pause();
		let delay = Duration::from_millis(200);
		let warmup = Duration::from_secs(1);
		let media = media(delay);
		let broadcast = moq_net::broadcast::Info::new().produce();
		let track = broadcast
			.create_track("video", hang::container::track_info(hang::catalog::PRIORITY.video))
			.unwrap();
		let mut producer = moq_mux::container::Producer::new(
			track.clone(),
			moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Video),
		);
		for ms in [0, 33, 66] {
			producer.write(frame(ms, ms == 0)).unwrap();
		}
		producer.discontinuity().unwrap();
		// The replacement is published live, one picture every 33 ms.
		let picture = |index: u64| frame(1_000 + index * 33, index == 0);
		producer.write(picture(0)).unwrap();
		let min = join(&track, delay, 0).await;
		let max = join(&track, delay + warmup, 1).await;
		let refresh = Refresh {
			track: max,
			decoder: refresh_decoder(Duration::from_millis(10)),
			interval: Some(Duration::from_millis(33)),
		};
		let task = tokio::spawn(playback(&media, delay).run(min, Buffered::new(0), Some(refresh)));
		let start = Instant::now();
		let mut next = 1;
		let mut shown = Vec::new();
		while !task.is_finished() {
			let elapsed = Instant::now() - start;
			if next < 60 && Duration::from_millis(next * 33) <= elapsed {
				producer.write(picture(next)).unwrap();
				next += 1;
				if next == 60 {
					producer.finish().unwrap();
				}
			}
			tokio::task::yield_now().await;
			if let Some(picture) = media.output.present_frame(&media) {
				shown.push((picture.timestamp.as_millis(), picture.size().width));
			}
			tokio::time::advance(Duration::from_millis(1)).await;
			assert!(elapsed < Duration::from_secs(30), "playback never finished: {shown:?}");
		}
		task.await.unwrap().unwrap();
		// The window still holds the last pictures the decoder queued.
		for _ in 0..1_000 {
			if let Some(picture) = media.output.present_frame(&media) {
				shown.push((picture.timestamp.as_millis(), picture.size().width));
			}
			tokio::time::advance(Duration::from_millis(1)).await;
		}
		assert_eq!(shown.first().map(|(at, _)| *at), Some(1_000), "{shown:?}");
		assert_eq!(shown.last().map(|(at, _)| *at), Some(1_000 + 59 * 33), "{shown:?}");
		let sizes: Vec<_> = shown.iter().map(|(_, size)| *size).collect();
		assert!(hands_over(&sizes), "no clean handover: {shown:?}");
		assert!(shown.windows(2).all(|pair| pair[0].0 < pair[1].0), "{shown:?}");
	}

	/// The max-delay decoder reads its own subscription, so it can see an encoder
	/// restart before the min-delay decoder does. It holds the replacement until the
	/// clock maps it rather than placing it on the old encoder's timeline.
	#[tokio::test]
	async fn a_restart_the_max_delay_decoder_reads_first_waits_for_the_clock() {
		tokio::time::pause();
		let delay = Duration::from_millis(200);
		let media = media(delay);
		let broadcast = moq_net::broadcast::Info::new().produce();
		let format = || moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Video);
		// Separate tracks with the same groups, so each decoder's delivery is up to the test.
		let publish = async |name: &str| {
			let track = broadcast
				.create_track(name, hang::container::track_info(hang::catalog::PRIORITY.video))
				.unwrap();
			let subscriber = track
				.consume()
				.subscribe(moq_net::track::Subscription::default().with_max_delay(delay))
				.await
				.unwrap();
			let mut producer = moq_mux::container::Producer::new(track, format());
			for ms in [0, 33, 66] {
				producer.write(frame(ms, ms == 0)).unwrap();
			}
			(producer, moq_mux::container::Consumer::new(subscriber, format()))
		};
		let (mut min, min_track) = publish("min").await;
		let (mut max, max_track) = publish("max").await;
		// The replacement encoder's timestamps run ahead of the old one's.
		let restart = |producer: &mut moq_mux::container::Producer<moq_mux::catalog::hang::Container>| {
			producer.discontinuity().unwrap();
			for ms in [1_000, 1_033, 1_066] {
				producer.write(frame(ms, ms == 1_000)).unwrap();
			}
			producer.finish().unwrap();
		};
		restart(&mut max);
		let refresh = Refresh {
			track: max_track,
			decoder: refresh_decoder(Duration::ZERO),
			interval: Some(Duration::from_millis(33)),
		};
		let task = tokio::spawn(playback(&media, delay).run(min_track, Buffered::new(0), Some(refresh)));
		let start = Instant::now();
		let late = Duration::from_secs(2);
		let mut shown = Vec::new();
		let mut restarted = false;
		while !task.is_finished() {
			if !restarted && Instant::now() >= start + late {
				restart(&mut min);
				restarted = true;
			}
			tokio::task::yield_now().await;
			if let Some(picture) = media.output.present_frame(&media) {
				shown.push((picture.timestamp.as_millis(), Instant::now() - start));
			}
			tokio::time::advance(Duration::from_millis(1)).await;
			assert!(
				Instant::now() - start < Duration::from_secs(30),
				"playback never finished"
			);
		}
		task.await.unwrap().unwrap();
		for _ in 0..1_000 {
			if let Some(picture) = media.output.present_frame(&media) {
				shown.push((picture.timestamp.as_millis(), Instant::now() - start));
			}
			tokio::time::advance(Duration::from_millis(1)).await;
		}
		let timestamps: Vec<_> = shown.iter().map(|(at, _)| *at).collect();
		assert_eq!(&timestamps[..3], [0, 33, 66], "{shown:?}");
		assert_eq!(timestamps.len(), 6, "{shown:?}");
		assert!(
			shown[3].1 >= late,
			"the replacement played before the clock mapped it: {shown:?}"
		);
		assert_eq!(timestamps[4] - timestamps[3], 33, "{shown:?}");
		assert_eq!(timestamps[5] - timestamps[4], 33, "{shown:?}");
	}

	/// Play one burst of 33 ms pictures, given by slot in decode order, through a
	/// codec holding `depth` pictures, and check the window shows every one on
	/// time.
	async fn schedule(order: &[u64], depth: usize) {
		tokio::time::pause();
		let delay = Duration::from_secs(2);
		let media = media(delay);
		let track = track(order.iter().map(|&slot| frame(slot * 33, slot == 0)), delay).await;
		let task = tokio::spawn(playback(&media, delay).run(track, Buffered::new(depth), None));
		tokio::task::yield_now().await;
		let last = moq_net::Timestamp::from_millis(order.iter().max().unwrap() * 33).unwrap();
		let end = media.presentation.lock().unwrap().due(last).unwrap();
		let mut shown = Vec::new();
		while Instant::now() <= end + Duration::from_millis(10) {
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
				.map(|(timestamp, _)| timestamp.as_millis())
				.collect::<Vec<_>>(),
			expected,
			"a picture was dropped"
		);
		let presentation = media.presentation.lock().unwrap();
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
}
