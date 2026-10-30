use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::{Poll, ready};

use moq_clock::Instant;
use moq_net::Timestamp;

use super::{Container, Frame, Read, TimestampRewind};

/// Media and group boundaries in delivery order.
pub(crate) enum Event {
	/// A media frame, with when the wire frame carrying it landed in this node's cache
	/// ([`moq_net::frame::Frame::arrived`]).
	Frame { frame: Frame, arrived: Instant },
	/// An endpoint marker. `opens` when no media precedes it in its group: a declared stop
	/// (a marker group or an audio terminal) rather than the previous frame's duration.
	FrameEnd { end: Timestamp, opens: bool },
	/// The group finished cleanly.
	GroupEnd,
	/// The group was lost upstream partway, so nothing more of it arrives. The gap it leaves is
	/// read once the next group resumes the track. `observed` is when the loss was read.
	Cut { observed: Instant },
}

/// Read items and exporter-only boundaries in delivery order.
pub(crate) enum Item {
	/// [`Read::Frame`], with when it landed as [`Event::Frame`] has.
	Frame {
		group: u64,
		frame: Frame,
		arrived: Instant,
	},
	/// [`Read::Gap`].
	Gap {
		timestamp: Timestamp,
		duration: Timestamp,
	},
	/// [`Read::End`].
	End {
		group: u64,
		end: Timestamp,
	},
	FrameEnd(Timestamp),
	GroupEnd,
	Cut {
		observed: Instant,
	},
}

/// Decode a moq-lite track into a stream of media [`Frame`]s in age-bounded
/// presentation order.
///
/// `Consumer` wraps a [`moq_net::track::Subscriber`] and a [`Container`]
/// format implementation, typically
/// [`catalog::hang::Container`](crate::catalog::hang::Container). Yields
/// decoded frames via [`read`](Self::read).
///
/// ## Ordering & age skipping
///
/// Groups can arrive on the wire out of order. The consumer always reads frames *within*
/// a group in arrival order, but across groups it advances by sequence number, skipping
/// a stalled or missing group once everything it could still present (bounded by where
/// the next group begins) falls a full budget behind the newest content, the same reach
/// [`moq_net::track::Subscription::max_delay`] measures. With the default max delay of zero,
/// the consumer skips aggressively: any group that has a newer alternative is dropped.
/// With a non-zero max delay, slow groups are tolerated up to that budget before being
/// skipped. A missing sequence gets the same tolerance: there is no way to tell a stream
/// that lost the delivery race from one the cache evicted.
/// Its budget also expires that much monotonic time after the first group past it arrived, so
/// a marker behind a hole is delivered even when the track is paused, and the holes a stalled
/// source left behind expire together rather than one budget each.
///
/// Delivery starts at the lowest absolute start among the
/// [`Subscription::ranges`](moq_net::track::Subscription::ranges) when every range names one, waiting for that group under the budget; without one it
/// starts wherever the publisher does, adopted from the first served groups. From there
/// the same budget is the one rule that catches the consumer up to the live edge, and
/// history the publisher no longer serves expires like any other gap, since its reach
/// sits a full budget behind the newest content.
///
/// A stalled group is also skipped early, regardless of the max delay budget, once it has
/// presented up to where the next group begins. CMAF frames carry a per-sample duration,
/// so a group whose most recent frame ends (timestamp + duration) at or past the next
/// group's first timestamp has nothing left worth waiting for. Containers without a
/// duration report zero, which disables this check and falls back to the max delay budget.
///
/// Put the initial max delay on the [`moq_net::track::Subscription`] before
/// subscribing. [`new`](Self::new) inherits that budget, and
/// [`set_max_delay`](Self::set_max_delay) changes it mid-stream.
///
/// ## Gaps and ends
///
/// [`read`](Self::read) yields [`Read::Gap`] wherever media after an item already read
/// will never arrive: a latency skip, an evicted current group, or a sequence hole whose
/// boundary is not contiguous within 1 ms. Whatever is lost before the first item is no
/// gap, since nothing was read before it. A marker group (no decodable frames: one empty
/// payload, or on a data track one object with no bytes) is the publisher declaring a stop,
/// read as [`Read::End`]; nothing is missing. A data track's marker carries no timestamp, so
/// it ends at the furthest point read, and reads as nothing before the first item.
/// Empty groups (zero objects) mean nothing. Errors come in delivery order: a payload that fails
/// to decode, or a group below an earlier group's start, ends the reader where it sits, and a
/// failed track once the groups already here drain. A skip returns the error of a malformed group
/// it already read rather than a gap; a group dropped unread is not validated, since decoding data
/// that is thrown away is work a peer could force. After an error the reader reads as finished.
///
/// ## Memory
///
/// Decoded frames waiting to be read are charged to a byte cap, [`set_max_buffer`](Self::set_max_buffer),
/// counting the whole wire payload they share, along with each buffered frame's and group's own
/// overhead. Once it is full the consumer stops
/// decoding ahead and stops pulling new groups past one successor to the cursor's group, so the
/// undecoded rest stays in the transport's cache, which bounds it. The group the cursor is on still decodes a wire frame whenever it holds nothing, so
/// reading never waits on its own lookahead. If that group stalls while the cap is full, the cursor
/// moves on to the next group like a skip past a slow group, read or not: the consumer can hold no
/// more of what it would wait behind.
pub struct Consumer<F: Container> {
	// Dropped once an error ends the reader, releasing the publisher's demand.
	track: Option<moq_net::track::Subscriber>,
	name: std::sync::Arc<str>,

	format: F,

	// The current group that we want to read from
	current: u64,

	// Groups that we are monitoring, sorted by sequence ascending.
	pending: VecDeque<GroupBuffer<F>>,

	// The last group whose input finished, so watching EOF never rescans that prefix.
	received: Option<u64>,

	// Reap unread evictions only after metadata doubles, charging each sweep to new handles.
	collect_at: usize,

	// What the pending groups hold, against the max buffer.
	memory: Memory,

	// The last pull stopped at a full max buffer rather than at the track, so nothing wakes the reader
	// once a skip refunds room: it pulls again itself.
	gated: bool,

	// Latches the cursor onto the publisher's first served group when the
	// subscription names no start. Cleared once a first group is chosen.
	startup: bool,

	// Latch onto the newest group instead, discarding what arrived below it.
	newest: bool,

	// How far we may drift from the live edge before skipping a group.
	max_delay: std::time::Duration,

	// Wakes the reader once the hole at the cursor expires.
	hole_timer: Option<std::pin::Pin<Box<moq_clock::Sleep>>>,

	// The furthest presentation point delivered so far: max(timestamp + duration), endpoints
	// included. `None` until the first item, so a loss before it is no gap.
	edge: Option<std::time::Duration>,

	// The first frame delivered from the latest group, and that group. Group starts never go
	// backwards, so a later group with a frame below this is malformed.
	start: Option<(u64, Timestamp)>,

	// The start of the group the cursor most recently left. B-frames, open-GOP pictures, and a
	// keyframe overlapping the previous group's last frame may dip below that group's content,
	// but a frame below its start is a rewind.
	floor: Option<Timestamp>,

	// Presentation end of the group we most recently advanced past, for the 1 ms
	// contiguity check on a delivered hole.
	presented_end: Option<std::time::Duration>,

	// Media went missing since the last item read, so the next one is preceded by a gap.
	gap: bool,

	// The item a gap was returned ahead of.
	next: Option<Item>,

	// The group the last event came from.
	group: u64,

	// The track failed, so no group can arrive. Returned once the groups already here drain.
	track_error: Option<F::Error>,
}

/// Two adjacent groups are timeline-contiguous when the next start is within this slack
/// of the current end. Per-sample durations and base-decode-times round to microseconds
/// independently, so a genuine boundary can be off by ~1 µs; a real missing group spans
/// about one group duration.
const CONTIGUITY_TOLERANCE: std::time::Duration = std::time::Duration::from_millis(1);

/// The default [`Consumer::set_max_buffer`]: one group at the transport's size cap.
pub const DEFAULT_MAX_BUFFER: usize = moq_net::group::MAX_CACHE_BYTES as usize;

/// The decoded bytes a consumer's groups hold, shared with each group so one dropped by any path
/// refunds what it held.
#[derive(Clone)]
struct Memory(Arc<MemoryState>);

struct MemoryState {
	used: AtomicUsize,
	max: AtomicUsize,
}

impl Memory {
	fn new(max: usize) -> Self {
		Self(Arc::new(MemoryState {
			used: AtomicUsize::new(0),
			max: AtomicUsize::new(max),
		}))
	}

	fn used(&self) -> usize {
		self.0.used.load(Ordering::Relaxed)
	}

	fn full(&self) -> bool {
		self.used() >= self.0.max.load(Ordering::Relaxed)
	}

	fn set_max(&self, max: usize) {
		self.0.max.store(max, Ordering::Relaxed);
	}

	fn charge(&self, bytes: usize) {
		self.0.used.fetch_add(bytes, Ordering::Relaxed);
	}

	fn refund(&self, bytes: usize) {
		self.0.used.fetch_sub(bytes, Ordering::Relaxed);
	}
}

fn pts_contiguous(end: Option<std::time::Duration>, next_start: std::time::Duration) -> bool {
	end.is_some_and(|end| next_start <= end.saturating_add(CONTIGUITY_TOLERANCE))
}

/// A gap from the delivered `edge` to `resume`, in the resume's scale.
fn gap(edge: std::time::Duration, resume: Timestamp) -> Item {
	let scale = resume.scale();
	// A next group may start before the edge (an overlapping endpoint), so the span bottoms out at zero.
	let start = edge
		.as_nanos()
		.saturating_mul(scale.as_u64() as u128)
		.div_euclid(1_000_000_000)
		.min(resume.value() as u128) as u64;
	let timestamp = Timestamp::new(start, scale).expect("at or below a valid timestamp");
	let duration = resume
		.checked_sub(timestamp)
		.expect("at or below the resume, in its scale");
	Item::Gap { timestamp, duration }
}

impl<F: Container> Consumer<F> {
	/// Create a Consumer wrapping the given moq-lite consumer, decoding `format`.
	///
	/// The ordering window inherits the subscriber's current max delay budget, clamped
	/// to the track's retention window. Put that budget on the
	/// [`moq_net::track::Subscription`] before awaiting the subscription, so the
	/// publisher preserves the same replay window.
	pub fn new(track: moq_net::track::Subscriber, format: F) -> Self {
		let subscription = track.subscription();
		// Delivery starts at the subscription floor; without one it starts wherever
		// the publisher does, adopted from the first arrivals (see poll_read). Either
		// way the age budget is the one rule that catches the cursor up to the live
		// edge from there. The budget is clamped to the track's retention window,
		// since history the publisher no longer keeps can't be waited for and would
		// otherwise stall the catch-up by the excess.
		let start = track.floor();
		let max_delay = subscription
			.max_delay
			.unwrap_or(std::time::Duration::MAX)
			.min(track.info().max_age.unwrap_or(std::time::Duration::MAX));
		Self {
			name: track.name().into(),
			track: Some(track),
			format,
			current: start.unwrap_or(0),
			pending: VecDeque::new(),
			received: None,
			collect_at: 1,
			memory: Memory::new(DEFAULT_MAX_BUFFER),
			gated: false,
			startup: start.is_none(),
			newest: false,
			max_delay,
			hole_timer: None,
			edge: None,
			start: None,
			floor: None,
			presented_end: None,
			gap: false,
			next: None,
			group: 0,
			track_error: None,
		}
	}

	/// Start at the newest group, the live edge, rather than the floor or the oldest group the
	/// publisher serves: the publisher's largest when it reports one past what has arrived. The
	/// subscription keeps its budget, so every group after the start arrives as it would for any
	/// reader; what arrived below the start is discarded.
	pub(crate) fn live(mut self) -> Self {
		self.startup = true;
		self.newest = true;
		self
	}

	/// The group sequence of the last event read.
	pub(crate) fn group(&self) -> u64 {
		self.group
	}

	/// The track's name.
	pub(crate) fn name(&self) -> &str {
		&self.name
	}

	/// Read the next frame from the track.
	///
	/// This method handles timestamp decoding, group ordering, and age management
	/// automatically. It will skip groups that are too far behind to maintain the
	/// configured max delay.
	///
	/// Returns `None` when the track has ended, and after an error. See [`Read`] for the items between frames;
	/// an undeclared forward timestamp jump is not a gap, since nothing says media is missing.
	pub async fn read(&mut self) -> Result<Option<Read>, F::Error> {
		kio::wait(|waiter| self.poll_read(waiter)).await
	}

	/// The next frame, passing over gaps and ends.
	#[cfg(test)]
	pub(crate) async fn read_frame(&mut self) -> Result<Option<Frame>, F::Error> {
		loop {
			match self.read().await? {
				Some(Read::Frame { frame, .. }) => return Ok(Some(frame)),
				Some(Read::Gap { .. } | Read::End { .. }) => continue,
				None => return Ok(None),
			}
		}
	}

	/// Poll-based implementation of the read loop.
	///
	/// Uses a single waiter that gets registered on all relevant kio channels,
	/// avoiding the need for `tokio::select!` or `FuturesUnordered`.
	pub fn poll_read(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<Read>, F::Error>> {
		loop {
			let read = match ready!(self.poll_item(waiter))? {
				Some(Item::Frame { group, frame, .. }) => Read::Frame { group, frame },
				Some(Item::Gap { timestamp, duration }) => Read::Gap { timestamp, duration },
				Some(Item::End { group, end }) => Read::End { group, end },
				Some(Item::FrameEnd(_) | Item::GroupEnd | Item::Cut { .. }) => continue,
				None => return Poll::Ready(Ok(None)),
			};
			return Poll::Ready(Ok(Some(read)));
		}
	}

	/// Read items while preserving boundaries needed to finish exporter fragments.
	pub(crate) fn poll_item(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<Item>, F::Error>> {
		if let Some(item) = self.next.take() {
			return Poll::Ready(Ok(Some(item)));
		}
		let edge = self.edge;
		let event = ready!(self.poll_event(waiter))?;
		let (item, resume) = match event {
			Some(Event::Frame { frame, arrived }) => {
				let resume = frame.timestamp;
				let group = self.group;
				(Item::Frame { group, frame, arrived }, resume)
			}
			Some(Event::FrameEnd { end, opens: true }) => (Item::End { group: self.group, end }, end),
			Some(Event::FrameEnd { end, opens: false }) => return Poll::Ready(Ok(Some(Item::FrameEnd(end)))),
			Some(Event::GroupEnd) => return Poll::Ready(Ok(Some(Item::GroupEnd))),
			Some(Event::Cut { observed }) => return Poll::Ready(Ok(Some(Item::Cut { observed }))),
			None => return Poll::Ready(Ok(None)),
		};
		if std::mem::take(&mut self.gap)
			&& let Some(edge) = edge
		{
			self.next = Some(item);
			return Poll::Ready(Ok(Some(gap(edge, resume))));
		}
		Poll::Ready(Ok(Some(item)))
	}

	/// Read media or a clean group boundary without waiting for a successor group.
	pub(crate) fn poll_event(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<Event>, F::Error>> {
		// An error ended the reader, which reads as finished from then on: a decode error has
		// already consumed the frame it came from, so reading on would skip it.
		if self.track.is_none() {
			return Poll::Ready(Ok(None));
		}
		let event = ready!(self.poll_event_inner(waiter));
		if event.is_err() {
			// Nothing more is read, so release the subscription and the groups it holds rather
			// than pin the publisher's demand and cache for as long as the caller keeps this.
			self.track = None;
			self.pending.clear();
		}
		Poll::Ready(event)
	}

	fn poll_event_inner(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<Event>, F::Error>> {
		loop {
			match self.poll_event_once(waiter) {
				// A skip refunded room for groups the max buffer held back, so pull them.
				Poll::Pending if std::mem::take(&mut self.gated) && !self.pull_gated() => continue,
				res => return res,
			}
		}
	}

	fn poll_event_once(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<Event>, F::Error>> {
		// Grab any new groups from the track, recording whether the track is finished.
		let finished = self.poll_read_finish(waiter).is_ready();

		// A subscription with no explicit start begins wherever the publisher does:
		// the lowest sequence it actually serves. Timestamps can't reveal that point
		// (a gap below the served window looks like in-flight groups until the budget
		// expires, or forever on a quiet track), so the cursor adopts it from the
		// first arrivals instead of assuming group 0. Skipping stays the budget's job.
		// A relative start names its floor once the first group to arrive resolves it.
		if self.startup
			&& !self.newest
			&& let Some(start) = self.subscriber().floor()
		{
			self.pending.retain(|group| group.sequence >= start);
			self.current = start;
			self.startup = false;
		}
		if self.startup {
			// NOTE: poll_min_timestamp buffers at least one frame per group and
			// registers the waiter on the ones still empty. Any wire frame counts, so a
			// paused track whose only group is a marker still reads its end. The oldest
			// buffers past a full max buffer, so a start is always found.
			let any_frame = self.pending.iter_mut().enumerate().any(|(i, group)| {
				let _ = group.poll_min_timestamp(waiter, &self.format, i == 0);
				!group.empty
			});
			if any_frame {
				// A live reader takes the newest group, though its frames may still be on their
				// way, and discards the older ones the budget also delivers. The publisher's
				// largest names it before its stream lands: the backlog's oldest group can arrive
				// first, and starting there loses the groups after it once the live edge moves a
				// budget past them.
				let start = match self.newest {
					true => {
						let mut start = self.pending.back().expect("a group has a frame").sequence;
						while let Poll::Ready(Some(largest)) = self.subscriber().poll_largest(waiter) {
							start = start.max(largest.group);
						}
						start
					}
					false => self.pending.front().expect("a group has a frame").sequence,
				};
				// Nothing has been read, so discarding what lies below the start is no gap.
				self.discard(|group| group.sequence < start)?;
				self.current = start;
				self.startup = false;
			}
		}

		loop {
			if self.pending.front().is_none_or(|group| group.sequence <= self.current) {
				self.hole_timer = None;
			}

			// Return the next frame from the current group if possible.
			// If the current group is finished or errored, advance to the next group.
			if let Some(group) = self.pending.front_mut()
				&& group.sequence <= self.current
			{
				match group.poll_read(waiter, &self.format) {
					Poll::Ready(Ok(Some(Event::Frame { frame, arrived }))) => {
						let seq = group.group.sequence;
						let ts = frame.timestamp;
						if let Some(floor) = self.floor
							&& ts.as_micros() < floor.as_micros()
						{
							return Poll::Ready(Err(TimestampRewind { timestamp: ts, floor }.into()));
						}
						if self.start.is_none_or(|(start, _)| start != seq) {
							self.start = Some((seq, ts));
						}
						let end = std::time::Duration::from(ts)
							+ frame.duration.map(std::time::Duration::from).unwrap_or_default();
						self.advance(end);
						self.group = seq;
						return Poll::Ready(Ok(Some(Event::Frame { frame, arrived })));
					}
					Poll::Ready(Ok(Some(Event::FrameEnd { end, opens }))) => {
						self.group = group.group.sequence;
						self.advance(end.into());
						return Poll::Ready(Ok(Some(Event::FrameEnd { end, opens })));
					}
					Poll::Ready(Ok(Some(event))) => return Poll::Ready(Ok(Some(event))),
					// Still blocked on this group, don't skip it yet.
					Poll::Pending => {}
					// A decode error is real and the caller must see it, not have the group
					// silently dropped.
					Poll::Ready(Err(Failure::Decode(e))) => {
						tracing::warn!(
							track = &*self.name,
							group = group.group.sequence,
							error = ?e,
							"group payload failed to decode; ending the reader"
						);
						return Poll::Ready(Err(e));
					}
					Poll::Ready(Err(Failure::Lost(e))) => {
						// The group aged out of the relay cache (`Error::Old`) or was otherwise
						// aborted. Any sequences between it and the next buffered group were
						// evicted alongside it, so jump straight to that group instead of
						// stepping one-by-one and then blocking on a sequence gap of groups
						// that will never arrive.
						tracing::warn!(
							track = &*self.name,
							group = group.group.sequence,
							error = ?e,
							"current group evicted; skipping to next buffered group"
						);
						// Whatever the group still held is lost, so this is a skip like any other.
						self.lose();
						let current = self.current;
						self.discard(|group| group.sequence <= current)?;
						self.current = self.pending.front().map_or(self.current + 1, |g| g.sequence);
						// Said now rather than with the gap, which waits for the next group: what
						// the caller holds of this one need not wait for a successor.
						return Poll::Ready(Ok(Some(Event::Cut {
							observed: moq_clock::now(),
						})));
					}
					// Cleanly finished group: advance to the next sequence.
					Poll::Ready(Ok(None)) => {
						// A data track's marker carries no timestamp, so it ends where this reader
						// got to. Before the first item there is nothing to end.
						if group.bare
							&& !std::mem::replace(&mut group.ended, true)
							&& let Some(edge) = self.edge
						{
							let end = u64::try_from(edge.as_micros())
								.ok()
								.and_then(|micros| Timestamp::from_micros(micros).ok())
								.ok_or(moq_net::Error::Decode(moq_net::DecodeError::BoundsExceeded))?;
							self.group = group.group.sequence;
							return Poll::Ready(Ok(Some(Event::FrameEnd { end, opens: true })));
						}
						if let Some(end) = group.max_end {
							self.presented_end = Some(end);
						}
						self.pending.pop_front();
						self.current += 1;
						self.note_group_edge();
						return Poll::Ready(Ok(Some(Event::GroupEnd)));
					}
				}
			}

			// Reap later aborts only when delivery blocks. An aborted group beyond a
			// sequence gap must not park the reader forever. Buffered frames and faults
			// stay in delivery order; the current group has its eviction path above.
			// Live groups register the waiter here so a later abort wakes the reader.
			let current = self.current;
			self.discard(|group| {
				group.sequence > current
					&& group.buffered.is_empty()
					&& group.fault.is_none()
					&& group.poll_aborted(waiter)
			})?;

			// The current group's furthest presentation point (timestamp + duration).
			let current_end = if let Some(current) = self.pending.front_mut()
				&& current.sequence <= self.current
			{
				match current.poll_min_timestamp(waiter, &self.format, true) {
					Poll::Ready(Some(_)) => current.max_end,
					_ => None,
				}
			} else {
				None
			};

			// Find the first newer group with data (our skip target) and where it starts.
			let mut next_group = None;
			for (i, group) in self.pending.iter_mut().enumerate() {
				if group.sequence <= self.current {
					continue;
				}

				if let Poll::Ready(Some(ts)) = group.poll_min_timestamp(waiter, &self.format, false) {
					next_group = Some((i, std::time::Duration::from(ts)));
					break;
				}
			}

			// Find the max timestamp across all newer groups.
			let mut max_timestamp = std::time::Duration::ZERO;
			for group in self.pending.iter_mut().rev() {
				if group.sequence <= self.current {
					break;
				}

				if let Poll::Ready(Some(ts)) = group.poll_max_timestamp(waiter, &self.format) {
					max_timestamp = max_timestamp.max(ts.into());
					break; // We know older groups won't be newer than this.
				}
			}

			// Walk the cursor over missing sequences below the first arrived group.
			// Groups race on independent QUIC streams (newer ones at higher priority),
			// so a buffered higher sequence proves nothing: the missing one may be
			// merely late, and there is no way to tell that from an eviction. The age
			// budget is the gate: everything a missing group could still present is
			// bounded by where the next stamped group begins. A finished track closes
			// the gap outright, since no new group can arrive. That proof covers only
			// sequences that never arrived: finishing the track ends new groups, not
			// the frames still flowing on ones already open, so arrived groups are
			// settled below by their own FIN, abort, or the budget. Sequences the
			// subscriber's own ranges exclude never arrive either, so a gap between two
			// ranges closes at once.
			if let Some(front) = self.pending.front()
				&& front.sequence > self.current
			{
				let sequence = front.sequence;
				// Late since a group past it arrived, not since the cursor got here: once the edge
				// stops moving, a backlog with every other group missing would otherwise drain one
				// budget per hole, which reads downstream as a source far slower than real time.
				let since = front.late;
				let elapsed = since.elapsed() >= self.max_delay;
				let aged = next_group.is_some_and(|(_, start)| max_timestamp.saturating_sub(start) >= self.max_delay);
				let excluded = self
					.track
					.as_ref()
					.is_some_and(|track| track.next_in_range(self.current).is_none_or(|next| next >= sequence));
				// The groups past the hole already fill the max buffer, so it can hold none of what
				// would arrive into it.
				let full = self.memory.full();
				if finished || elapsed || aged || excluded || full {
					let _ = self.pending.front_mut().unwrap().look_all(waiter, &self.format);
					let front = self.pending.front_mut().unwrap();
					let empty = front.empty && matches!(front.group.poll_finished(waiter), Poll::Ready(Ok(_)));
					let front = self.pending.front().unwrap();
					let next_start = front.min_timestamp.map(std::time::Duration::from).or(front.max_end);
					// A data marker has no timestamp to prove continuity across the hole.
					if !empty && next_start.is_none_or(|start| !pts_contiguous(self.presented_end, start)) {
						self.lose();
					}
					self.current = sequence;
					self.note_group_edge();
					continue;
				}
				if let Some(wake) = since.checked_add(self.max_delay) {
					let timer = self
						.hole_timer
						.get_or_insert_with(|| Box::pin(moq_clock::sleep_until(wake)));
					timer.as_mut().reset(wake);
					let _ = waiter.poll_future(timer.as_mut());
				}
			}

			// The current group is blocking. Everything it could still present ends
			// where the next group begins, so skip once that reach falls a full budget
			// behind the newest content, the same measure as a missing group. Its own
			// first frame is no bound: a group longer than the budget would be cut
			// short whenever it blocks behind a newer one, as a join's backlog does
			// while the newer groups race ahead on their own streams. Skip early too
			// once it has presented up to where the next group begins (duration
			// coverage), since nothing is left worth waiting for.
			let should_skip = next_group.is_some_and(|(_, next_start)| {
				max_timestamp.saturating_sub(next_start) >= self.max_delay
					|| current_end.is_some_and(|end| end >= next_start)
			});
			let target = match should_skip {
				true => next_group.map(|(i, start)| (i, Some(start))),
				// Once the later groups fill the max buffer, no more of them can be held while it waits,
				// so move on to the next group even with nothing of it read yet: it reads as the
				// cursor's group, and each such skip refunds what the group left behind held.
				false if self.memory.full() => self
					.pending
					.iter()
					.position(|group| group.sequence > self.current)
					.map(|i| (i, self.pending[i].min_timestamp.map(std::time::Duration::from))),
				false => None,
			};

			if let Some((new_idx, next_start)) = target {
				// Without a start read yet, nothing proves the timeline continues.
				let hole = next_start.is_none_or(|start| !pts_contiguous(current_end.or(self.presented_end), start));
				let had_marker = self.pending.iter().take(new_idx).any(GroupBuffer::marker);
				// Buffered frames are thrown away, and `current_end` counts them, so contiguity alone would hide them.
				let discarded = self
					.pending
					.iter()
					.take(new_idx)
					.any(|group| !group.buffered.is_empty());
				let target = self.pending[new_idx].sequence;
				self.discard(|group| group.sequence < target)?;
				if hole || had_marker || discarded {
					self.lose();
				}
				let new_current = self.pending.front().map(|g| g.sequence).unwrap();

				tracing::debug!(old = self.current, new = new_current, "skipping slow groups");

				self.current = new_current;
				self.note_group_edge();
				continue;
			}

			if finished && self.pending.is_empty() {
				if let Some(err) = self.track_error.take() {
					// The caller only gets the bare error, with nothing to say which track died.
					tracing::warn!(track = &*self.name, error = ?err, "track failed; ending the reader");
					return Poll::Ready(Err(err));
				}
				return Poll::Ready(Ok(None));
			}

			return Poll::Pending;
		}
	}

	// The subscription, which only an error ending the reader drops.
	fn subscriber(&mut self) -> &mut moq_net::track::Subscriber {
		self.track.as_mut().expect("read only until an error ends the reader")
	}

	// Media this reader will never deliver: the next item is preceded by a gap.
	fn lose(&mut self) {
		self.gap = true;
	}

	fn advance(&mut self, end: std::time::Duration) {
		if self.edge.is_none_or(|edge| end > edge) {
			self.edge = Some(end);
		}
	}

	fn note_group_edge(&mut self) {
		self.hole_timer = None;
		if let Some((_, ts)) = self.start {
			self.floor = Some(ts);
		}
	}

	/// Receive group handles and wait for the track and every received group to finish.
	pub(crate) fn poll_received(&mut self, waiter: &kio::Waiter) -> Poll<()> {
		if self.track.is_none() {
			return Poll::Ready(());
		}
		ready!(self.poll_read_finish(waiter));
		if self.track_error.is_some() {
			return Poll::Pending;
		}
		let first = self
			.pending
			.partition_point(|group| self.received.is_some_and(|last| group.sequence <= last));
		for group in self.pending.range_mut(first..) {
			if !matches!(group.group.poll_finished(waiter), Poll::Ready(Ok(_))) {
				return Poll::Pending;
			}
			self.received = Some(group.sequence);
		}
		Poll::Ready(())
	}

	// Reads any new groups from the track until it finished or failed.
	//
	// Returns Pending until all groups have been consumed.
	fn poll_read_finish(&mut self, waiter: &kio::Waiter) -> Poll<()> {
		self.collect_pending(waiter);
		self.gated = false;
		if self.track_error.is_some() {
			return Poll::Ready(());
		}
		loop {
			// Past the max buffer, later groups wait in the transport's cache. A missing group at the
			// cursor then expires like one past the budget: see the hole below.
			if self.pull_gated() {
				self.gated = true;
				return Poll::Pending;
			}
			let next = match ready!(self.subscriber().poll_recv_group(waiter)) {
				Ok(next) => next,
				Err(err) => {
					self.track_error = Some(err.into());
					return Poll::Ready(());
				}
			};

			let Some(group) = next else {
				// Track is finished.
				return Poll::Ready(());
			};

			let mut reader = GroupBuffer::new(group, self.memory.clone());
			let sequence = reader.group.sequence;

			if sequence < self.current {
				tracing::debug!(old = ?sequence, current = ?self.current, "skipping old group");
				continue;
			}

			let idx = self
				.pending
				.partition_point(|g| g.group.sequence < reader.group.sequence);
			// Each group is late from the earliest arrival among it and the groups past it. A drained
			// backlog comes in sequence order, not arrival order, so an arrival can lower those below it.
			if let Some(next) = self.pending.get(idx) {
				reader.late = reader.late.min(next.late);
			}
			for below in self.pending.range_mut(..idx).rev() {
				if below.late <= reader.late {
					break;
				}
				below.late = reader.late;
			}
			self.pending.insert(idx, reader);
			self.collect_pending(waiter);
		}
	}

	fn collect_pending(&mut self, waiter: &kio::Waiter) {
		if self.pending.len() < self.collect_at {
			return;
		}
		let current = self.current;
		self.pending.retain_mut(|group| {
			// Keep the cursor's gap and anything already decoded, including markers and faults.
			// A never-read evicted group holds no event that its missing sequence cannot say.
			group.sequence == current || !group.empty || !group.poll_aborted(waiter)
		});
		self.collect_at = self.pending.len().saturating_mul(2).max(1);
	}

	// Whether the max buffer holds back the next group: it is full, and the cursor's group already has a
	// successor to move on to, so however small the cap, a stalled group can always be skipped.
	fn pull_gated(&self) -> bool {
		self.memory.full() && self.pending.len() >= 2
	}

	// Drop the pending groups `pick` selects without delivering them. A drop must not hide what
	// was already found, so the lowest one holding a fault, or with frames below the start of the
	// latest delivered group, ends the reader instead. What was never read stays unread: decoding
	// data only to throw it away is work a peer could force.
	fn discard(&mut self, mut pick: impl FnMut(&mut GroupBuffer<F>) -> bool) -> Result<(), F::Error> {
		let start = self.start;
		let mut fault = None;
		self.pending.retain_mut(|group| {
			if !pick(group) {
				return true;
			}
			if fault.is_none() {
				fault = match (start, group.min_timestamp) {
					(Some((prev, floor)), Some(min))
						if group.sequence > prev && min.as_micros() < floor.as_micros() =>
					{
						Some(TimestampRewind { timestamp: min, floor }.into())
					}
					_ => group.fault.take(),
				};
			}
			false
		});
		match fault {
			Some(err) => {
				tracing::warn!(track = &*self.name, error = ?err, "discarded a malformed group; ending the reader");
				Err(err)
			}
			None => Ok(()),
		}
	}

	/// Set the max delay mid-stream, clamped to the track's retention window like
	/// [`new`](Self::new). The subscription keeps the requested value verbatim,
	/// matching [`moq_net::track::Subscription::max_delay`].
	pub fn set_max_delay(&mut self, max_delay: std::time::Duration) {
		// An ended reader has no subscription left to tune.
		let Some(track) = &mut self.track else {
			return;
		};
		self.max_delay = max_delay.min(track.info().max_age.unwrap_or(std::time::Duration::MAX));
		// The transport enforces the same budget on the subscription itself, so a
		// tolerance set here has to reach it: otherwise moq-net skips the very groups
		// this consumer was told to wait for, before they ever get here.
		let subscription = track.subscription().with_max_delay(max_delay);
		let _ = track.update(subscription);
	}

	/// Cap the bytes of decoded frames held ahead of the reader, counting the wire payload they share
	/// and each frame's and group's own overhead (default [`DEFAULT_MAX_BUFFER`]). Past it the consumer stops decoding ahead
	/// and pulling new groups; see the type docs.
	pub fn set_max_buffer(&mut self, bytes: usize) {
		self.memory.set_max(bytes);
	}

	/// The bytes currently charged against [`set_max_buffer`](Self::set_max_buffer).
	#[cfg(test)]
	pub(crate) fn buffered_bytes(&self) -> usize {
		self.memory.used()
	}
}

/// Why a group stopped buffering.
enum Failure<E> {
	/// A consumed payload failed to decode, so the track is malformed.
	Decode(E),
	/// The transport aborted or evicted the group before its next frame arrived.
	Lost(E),
}

/// Internal reader for a group of frames.
///
/// Handles two-phase frame reading (get FrameConsumer, then read all data),
/// timestamp parsing, and min/max timestamp tracking for age decisions.
struct GroupBuffer<F: Container> {
	group: moq_net::group::Consumer,

	// From then on, any sequence missing below this group is late: the earliest arrival among it
	// and the groups past it, kept on insert so a hole reads it without scanning the backlog.
	late: moq_clock::Instant,

	// The current frame index within the group.
	index: usize,

	// Whether the group has carried any wire frame. Empty groups (zero objects) mean
	// nothing; a finished group with wire frames and no media is a marker.
	empty: bool,

	// Whether a decodable (non-marker) frame was buffered.
	media: bool,

	// Whether this is a data track's marker group: one object with no bytes.
	bare: bool,

	// Whether a marker with no timestamp of its own was read as an end.
	ended: bool,

	// A decode error found while looking ahead, delivered after the frames buffered before it.
	fault: Option<F::Error>,

	// Read frames that haven't been consumed yet, each with when its wire frame landed.
	// Each with when it arrived and what it holds against the max buffer.
	buffered: VecDeque<(Frame, Instant, usize)>,
	markers: VecDeque<(usize, Timestamp)>,
	delivered: usize,

	// The consumer's max buffer, and how much of it this group holds, refunded when it drops.
	memory: Memory,
	charged: usize,

	// The minimum timestamp in the group.
	min_timestamp: Option<Timestamp>,

	// The maximum timestamp in the group.
	max_timestamp: Option<Timestamp>,

	// The furthest presentation point reached so far, i.e. max(timestamp + duration).
	// Equals the max timestamp when the container carries no per-frame duration.
	// Stored as a wall-clock duration so cross-scale comparisons are cheap.
	max_end: Option<std::time::Duration>,
}

impl<F: Container> GroupBuffer<F> {
	fn new(group: moq_net::group::Consumer, memory: Memory) -> Self {
		let mut this = Self {
			// When the group landed, not when it was pulled: a full max buffer delays pulls.
			late: group.arrived(),
			group,
			index: 0,
			empty: true,
			media: false,
			bare: false,
			ended: false,
			fault: None,
			buffered: VecDeque::new(),
			markers: VecDeque::new(),
			delivered: 0,
			max_timestamp: None,
			min_timestamp: None,
			max_end: None,
			memory,
			charged: 0,
		};
		this.charge(std::mem::size_of::<Self>());
		this
	}

	/// Poll for the next frame from this group.
	fn poll_read(&mut self, waiter: &kio::Waiter, format: &F) -> Poll<Result<Option<Event>, Failure<F::Error>>> {
		loop {
			if self.markers.front().is_some_and(|(index, _)| *index <= self.delivered) {
				let (index, end) = self.markers.pop_front().unwrap();
				self.refund(MARKER_COST);
				return Poll::Ready(Ok(Some(Event::FrameEnd { end, opens: index == 0 })));
			}
			if let Some((frame, arrived, cost)) = self.buffered.pop_front() {
				self.delivered += 1;
				self.refund(cost);
				return Poll::Ready(Ok(Some(Event::Frame { frame, arrived })));
			}
			if !ready!(self.buffer_once(waiter, format)?) {
				return Poll::Ready(Ok(None));
			}
		}
	}

	// Add one more frame to the buffer if possible, first returning a decode error the lookahead kept.
	//
	// Returns false if the group is finished.
	fn buffer_once(&mut self, waiter: &kio::Waiter, format: &F) -> Poll<Result<bool, Failure<F::Error>>> {
		if let Some(err) = self.fault.take() {
			return Poll::Ready(Err(Failure::Decode(err)));
		}
		// A failed read is the transport's, while a failed decode is the payload's: an abort of
		// the tail behind a malformed frame must not hide that frame.
		let wire = match ready!(self.group.poll_read_frame(waiter)) {
			Ok(Some(wire)) => wire,
			Ok(None) => return Poll::Ready(Ok(false)),
			Err(err) if self.poll_aborted(waiter) => return Poll::Ready(Err(Failure::Lost(err.into()))),
			Err(err) => return Poll::Ready(Err(Failure::Decode(err.into()))),
		};
		self.empty = false;
		let arrived = wire.arrived();
		let size = wire.payload.len();
		let frames = format.decode(wire).map_err(Failure::Decode)?;
		if format.kind() == super::Kind::Data {
			// A data track's marker is one object with no bytes, alone in its group.
			if self.bare || (frames.is_empty() && self.media) {
				let err = moq_net::Error::Decode(moq_net::DecodeError::ExpectedEnd).into();
				return Poll::Ready(Err(Failure::Decode(err)));
			}
			self.bare = frames.is_empty();
		}

		// Decoded payloads are views of the wire payload, which stays allocated while any of them is
		// held, so the last one read also holds whatever of it the views leave out, such as a header.
		let mut views = 0;
		let first = self.buffered.len();
		for mut frame in frames {
			if let Some(bound) = format.end(&frame) {
				self.note_end(bound);
				self.markers.push_back((self.index, bound));
				self.charge(MARKER_COST);
				continue;
			}

			self.min_timestamp = Some(match self.min_timestamp {
				Some(existing) => existing.min(frame.timestamp),
				None => frame.timestamp,
			});

			self.max_timestamp = Some(match self.max_timestamp {
				Some(existing) => existing.max(frame.timestamp),
				None => frame.timestamp,
			});

			// Furthest presentation point, in wall-clock terms so timestamp and
			// duration can be at different scales without extra conversions. A frame
			// with no duration contributes only its timestamp.
			self.note_end(frame.timestamp);
			if let Some(duration) = frame.duration {
				let end = std::time::Duration::from(frame.timestamp) + std::time::Duration::from(duration);
				self.max_end = Some(self.max_end.map_or(end, |existing| existing.max(end)));
			}

			// First frame of a group is always a keyframe by protocol invariant; trust
			// the container's flag otherwise so CMAF mid-group keyframes survive.
			frame.keyframe = frame.keyframe || self.index == 0;
			self.index += 1;
			self.media = true;

			let cost = frame_cost(&frame);
			views += frame.payload.len();
			self.charge(cost);
			self.buffered.push_back((frame, arrived, cost));
		}
		// Views that leave out more than they keep are copied out, freeing the wire payload, so
		// whoever holds a frame after the reader retains at most twice its payload.
		let rest = size.saturating_sub(views);
		if rest > views {
			for (frame, ..) in self.buffered.range_mut(first..) {
				frame.payload = bytes::Bytes::copy_from_slice(&frame.payload);
			}
		} else if self.buffered.len() > first
			&& rest > 0
			&& let Some((_, _, cost)) = self.buffered.back_mut()
		{
			*cost += rest;
			self.charge(rest);
		}

		Poll::Ready(Ok(true))
	}

	// Buffer one more frame ahead of the cursor. Nothing here hands an error to a caller, so a
	// decode error is kept for the reader rather than dropped with the frame it consumed; a
	// transport abort is left for the cursor to find.
	//
	// Returns false once this group can buffer nothing more. When `capped`, pends without registering
	// the waiter while the max buffer is full: only the reader taking frames frees it.
	fn look_once(&mut self, waiter: &kio::Waiter, format: &F, capped: bool) -> Poll<bool> {
		if self.fault.is_some() {
			return Poll::Ready(false);
		}
		if capped && self.memory.full() {
			return Poll::Pending;
		}
		match ready!(self.buffer_once(waiter, format)) {
			Ok(more) => Poll::Ready(more),
			Err(Failure::Decode(err)) => {
				self.fault = Some(err);
				Poll::Ready(false)
			}
			Err(Failure::Lost(_)) => Poll::Ready(false),
		}
	}

	fn look_all(&mut self, waiter: &kio::Waiter, format: &F) -> Poll<()> {
		while ready!(self.look_once(waiter, format, true)) {}
		Poll::Ready(())
	}

	/// Poll for the maximum timestamp in this group, or `None` if it ended without one.
	fn poll_max_timestamp(&mut self, waiter: &kio::Waiter, format: &F) -> Poll<Option<Timestamp>> {
		// Keep reading more frames just to advance the max timestamp.
		let looked = self.look_all(waiter, format);

		if let Some(max) = self.max_timestamp {
			return Poll::Ready(Some(max));
		}
		// Frames left unread behind a full max buffer may still carry one.
		ready!(looked);
		self.poll_ended(waiter)
	}

	/// Poll for the minimum timestamp in this group, or `None` if it ended without one. A `lead` group
	/// buffers past a full max buffer only while it holds nothing, endpoint markers included.
	fn poll_min_timestamp(&mut self, waiter: &kio::Waiter, format: &F, lead: bool) -> Poll<Option<Timestamp>> {
		// A wire frame can decode to no media frames, so keep going until one is buffered.
		let mut looked = Poll::Ready(());
		while self.buffered.is_empty() {
			let capped = !lead || !self.markers.is_empty();
			match self.look_once(waiter, format, capped) {
				Poll::Ready(true) => continue,
				Poll::Ready(false) => break,
				Poll::Pending => {
					looked = Poll::Pending;
					break;
				}
			}
		}

		if let Some(min) = self.min_timestamp {
			return Poll::Ready(Some(min));
		}
		// Frames left unread behind a full max buffer may still carry one.
		ready!(looked);
		self.poll_ended(waiter)
	}

	// Ready with no timestamp once the group can no longer produce one: it ended or failed to decode.
	fn poll_ended(&mut self, waiter: &kio::Waiter) -> Poll<Option<Timestamp>> {
		if self.fault.is_some() || self.group.poll_finished(waiter).is_ready() {
			return Poll::Ready(None);
		}
		Poll::Pending
	}

	/// True if the transport can no longer deliver the frame this group is stopped on:
	/// the stream was reset (evicted, `Old`, cancelled, oversized, ...).
	fn poll_aborted(&mut self, waiter: &kio::Waiter) -> bool {
		matches!(self.group.poll_finished(waiter), Poll::Ready(Err(_)))
	}

	fn note_end(&mut self, timestamp: Timestamp) {
		let end = std::time::Duration::from(timestamp);
		self.max_end = Some(self.max_end.map_or(end, |existing| existing.max(end)));
	}

	fn marker(&self) -> bool {
		!self.empty && !self.media
	}

	fn charge(&mut self, bytes: usize) {
		self.charged += bytes;
		self.memory.charge(bytes);
	}

	fn refund(&mut self, bytes: usize) {
		self.charged -= bytes;
		self.memory.refund(bytes);
	}
}

impl<F: Container> Drop for GroupBuffer<F> {
	fn drop(&mut self) {
		self.memory.refund(self.charged);
	}
}

/// What a buffered endpoint marker holds against the max buffer.
const MARKER_COST: usize = std::mem::size_of::<(usize, Timestamp)>();

/// What a buffered frame holds against the max buffer: its payload and its own entry.
fn frame_cost(frame: &Frame) -> usize {
	std::mem::size_of::<(Frame, Instant, usize)>() + frame.payload.len()
}

impl<F: Container> std::ops::Deref for GroupBuffer<F> {
	type Target = moq_net::group::Consumer;

	fn deref(&self) -> &Self::Target {
		&self.group
	}
}

#[cfg(test)]
mod tests {
	use super::Container as ContainerTrait;
	use super::*;
	use crate::catalog::hang::Container;
	use std::time::Duration;

	use bytes::Bytes;

	/// A millisecond track: the frames written here carry timestamps.
	fn timed() -> moq_net::track::Info {
		moq_net::track::Info::default().with_timescale(moq_net::Timescale::MILLI)
	}

	/// Mint a standalone track for tests via a throwaway broadcast, since tracks are
	/// born from their broadcast (no public `track::Producer::new`).
	fn track_producer(
		name: impl Into<std::sync::Arc<str>>,
		info: impl Into<Option<moq_net::track::Info>>,
	) -> moq_net::track::Producer {
		moq_net::broadcast::Info::new()
			.produce()
			.create_track(name, info)
			.unwrap()
	}

	fn ts(micros: u64) -> Timestamp {
		Timestamp::from_micros(micros).unwrap()
	}

	/// Test-only container that round-trips a per-sample duration on the wire, so the
	/// duration-based skip can be exercised without building a real CMAF init segment.
	/// Each frame is `[timestamp_us: u64 LE][duration_us: u64 LE][payload]`.
	struct DurationWire;

	/// Encode a `[timestamp][duration][payload]` DurationWire frame.
	fn encode_duration_frame(timestamp: Timestamp, duration: Timestamp) -> Vec<u8> {
		let mut buf = Vec::with_capacity(18);
		buf.extend_from_slice(&(timestamp.as_micros() as u64).to_le_bytes());
		buf.extend_from_slice(&(duration.as_micros() as u64).to_le_bytes());
		buf.extend_from_slice(&[0xDE, 0xAD]);
		buf
	}

	impl ContainerTrait for DurationWire {
		type Error = crate::Error;

		fn write(&self, group: &mut moq_net::group::Producer, frames: &[Frame]) -> Result<(), Self::Error> {
			// The duration tests write frames directly via `write_duration_frame`;
			// this path just preserves the timestamp with an unknown duration.
			for frame in frames {
				group.write_frame(frame.timestamp, encode_duration_frame(frame.timestamp, ts(0)))?;
			}
			Ok(())
		}

		fn decode(&self, frame: moq_net::frame::Frame) -> Result<Vec<Frame>, Self::Error> {
			use bytes::Buf;

			let mut data = frame.payload;
			let timestamp = ts(data.get_u64_le());
			let duration = ts(data.get_u64_le());
			let payload = data.copy_to_bytes(data.remaining());

			Ok(vec![Frame {
				timestamp,
				payload,
				keyframe: false,
				duration: Some(duration),
			}])
		}
	}

	/// Write one DurationWire frame (timestamp and duration in µs) into a group.
	fn write_duration_frame(group: &mut moq_net::group::Producer, timestamp: Timestamp, duration: Timestamp) {
		group
			.write_frame(timestamp, encode_duration_frame(timestamp, duration))
			.unwrap();
	}

	/// Write a finished group with explicit sequence and timestamps (Container::Legacy(crate::container::Kind::Data) format).
	fn write_group(track: &mut moq_net::track::Producer, sequence: u64, timestamps: &[Timestamp]) {
		let mut group = track.create_group(moq_net::group::Info { sequence }).unwrap();
		for &timestamp in timestamps {
			let frame = Frame {
				timestamp,
				payload: Bytes::from_static(&[0xDE, 0xAD]),
				keyframe: false,
				duration: None,
			};
			Container::Legacy(crate::container::Kind::Data)
				.write(&mut group, &[frame])
				.unwrap();
		}
		group.finish().unwrap();
	}

	/// Drain all available frames with a per-read timeout.
	async fn read_all(consumer: &mut Consumer<Container>) -> Result<Vec<Frame>, crate::Error> {
		let mut frames = Vec::new();
		loop {
			match tokio::time::timeout(Duration::from_millis(200), consumer.read_frame()).await {
				Ok(Ok(Some(frame))) => frames.push(frame),
				Ok(Ok(None)) => break,
				Ok(Err(e)) => return Err(e),
				Err(_) => panic!(
					"read_all: Consumer::read timed out after 200ms ({} frames collected so far)",
					frames.len()
				),
			}
		}
		Ok(frames)
	}

	/// Read a continuous sequence, refusing gaps and declared stops.
	async fn continuous_frames<F: ContainerTrait>(consumer: &mut Consumer<F>) -> Vec<Frame>
	where
		F::Error: std::fmt::Debug,
	{
		let mut frames = Vec::new();
		while let Some(read) = consumer.read().await.unwrap() {
			let Read::Frame { frame, .. } = read else {
				panic!("unexpected break: {read:?}")
			};
			frames.push(frame);
		}
		frames
	}

	/// Wrap `track` so only the container consumer performs age skips. The
	/// transport gets the media track's full retention window and hands over every
	/// retained group; what the container does with them is what the test measures.
	///
	/// Both layers enforce the same budget, and normally should: the transport skipping
	/// a group the consumer was going to skip anyway just saves the bandwidth. These
	/// tests are the exception, since they are about the consumer's half of it.
	fn container_max_delay_only(
		track: moq_net::track::Subscriber,
		max_delay: std::time::Duration,
	) -> Consumer<Container> {
		let control = track.control();
		let mut consumer = Consumer::new(track, Container::Legacy(crate::container::Kind::Data));
		consumer.set_max_delay(max_delay);
		control
			.update(moq_net::track::Subscription::default().with_max_delay(Duration::from_secs(30)))
			.unwrap();
		consumer
	}

	// ---- Basic Reading ----

	#[tokio::test]
	async fn read_single_group() {
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track =
			track.subscribe(moq_net::track::Subscription::default().with_max_delay(Duration::from_millis(500)));
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Data));

		write_group(&mut track, 0, &[ts(0)]);
		track.finish().unwrap();

		let frames = read_all(&mut consumer).await.unwrap();
		assert_eq!(frames.len(), 1);
		assert_eq!(frames[0].timestamp, ts(0));
		assert!(frames[0].keyframe);

		// Next read returns None (track ended)
		assert!(consumer.read().await.unwrap().is_none());
	}

	fn write_marker_group(track: &mut moq_net::track::Producer, sequence: u64, timestamp: Timestamp) {
		let mut group = track.create_group(moq_net::group::Info { sequence }).unwrap();
		Container::Legacy(crate::container::Kind::Audio)
			.write(
				&mut group,
				&[Frame {
					timestamp,
					payload: Bytes::new(),
					keyframe: false,
					duration: None,
				}],
			)
			.unwrap();
		group.finish().unwrap();
	}

	/// Every item until the track ends: a frame as its timestamp in µs, a gap as its start and
	/// span in µs, or an end.
	async fn items(consumer: &mut Consumer<Container>) -> Vec<String> {
		let mut items = Vec::new();
		while let Some(read) = consumer.read().await.unwrap() {
			items.push(match read {
				Read::Frame { frame, .. } => frame.timestamp.as_micros().to_string(),
				Read::Gap { timestamp, duration } => format!("gap {}+{}", timestamp.as_micros(), duration.as_micros()),
				Read::End { end, .. } => format!("end {}", end.as_micros()),
			});
		}
		items
	}

	/// Joining a paused track, whose only served group is a marker, reads the end without
	/// waiting for media to resume.
	#[tokio::test(start_paused = true)]
	async fn a_join_on_a_marker_group_reads_its_end() {
		let mut track = track_producer("gaps", hang::container::track_info(hang::catalog::PRIORITY.audio));
		let subscription =
			track.subscribe(moq_net::track::Subscription::default().with_max_delay(Duration::from_secs(10)));
		let mut reader = Consumer::new(subscription, Container::Legacy(crate::container::Kind::Audio));
		write_marker_group(&mut track, 16, ts(1_000_000));

		let mut read = async || {
			tokio::time::timeout(Duration::from_secs(1), reader.read())
				.await
				.expect("the end is read while the track stays open")
				.unwrap()
		};
		assert!(matches!(read().await, Some(Read::End { group: 16, end }) if end == ts(1_000_000)));
	}

	/// A data track's marker carries no timestamp, so it reads as an end at the furthest point
	/// read rather than as a gap, and as nothing before the first item.
	#[tokio::test(start_paused = true)]
	async fn a_data_marker_reads_as_an_end_at_the_edge() {
		let hour = Duration::from_secs(3600);
		let info = hang::container::track_info(hang::catalog::PRIORITY.audio).with_max_age(hour);
		let track = track_producer("data", info);
		let subscription = moq_net::track::Subscription::default()
			.with_max_delay(hour)
			.with_range(0..);
		let mut reader = Consumer::new(
			track.subscribe(subscription),
			Container::Legacy(crate::container::Kind::Data),
		);
		let mut producer = crate::container::Producer::new(track, Container::Legacy(crate::container::Kind::Data));
		let frame = |micros, payload: &'static [u8]| Frame {
			timestamp: ts(micros),
			payload: Bytes::from_static(payload),
			keyframe: true,
			duration: None,
		};

		producer.discontinuity().unwrap();
		producer.write(frame(0, b"a")).unwrap();
		producer.write(frame(10_000, b"")).unwrap();
		producer.discontinuity().unwrap();
		producer.write(frame(2_405_070_000, b"b")).unwrap();
		producer.finish().unwrap();

		assert_eq!(items(&mut reader).await, ["0", "10000", "end 10000", "2405070000"]);
	}

	/// A data marker has no timestamp, so the sequences missing before it are lost, not covered
	/// by the break it declares.
	#[tokio::test(start_paused = true)]
	async fn a_hole_before_a_terminal_data_marker_is_a_gap() {
		let mut track = track_producer("data", hang::container::track_info(hang::catalog::PRIORITY.audio));
		let subscription = moq_net::track::Subscription::default()
			.with_max_delay(Duration::from_secs(10))
			.with_range(0..);
		let mut reader = Consumer::new(
			track.subscribe(subscription),
			Container::Legacy(crate::container::Kind::Data),
		);
		write_group(&mut track, 0, &[ts(0), ts(10_000)]);
		let mut group = track.create_group(moq_net::group::Info { sequence: 2 }).unwrap();
		group.write_frame(ts(0), Bytes::new()).unwrap();
		group.finish().unwrap();
		track.finish().unwrap();

		assert_eq!(items(&mut reader).await, ["0", "10000", "gap 10000+0", "end 10000"]);
	}

	#[tokio::test(start_paused = true)]
	async fn a_sequence_hole_before_a_paused_data_marker_expires() {
		for budget in [Duration::ZERO, Duration::from_millis(100)] {
			let mut track = track_producer("data", hang::container::track_info(hang::catalog::PRIORITY.audio));
			let subscriber = track.subscribe(moq_net::track::Subscription::default().with_max_delay(budget));
			let mut reader = Consumer::new(subscriber, Container::Legacy(crate::container::Kind::Data));
			write_group(&mut track, 0, &[ts(10_000)]);
			assert!(matches!(reader.read().await.unwrap(), Some(Read::Frame { .. })));
			let mut marker = track.create_group(moq_net::group::Info { sequence: 2 }).unwrap();
			marker.write_frame(ts(10_000), Bytes::new()).unwrap();
			marker.finish().unwrap();

			if !budget.is_zero() {
				assert!(reader.poll_read(&kio::Waiter::noop()).is_pending());
				tokio::time::advance(budget - Duration::from_millis(1)).await;
				assert!(reader.poll_read(&kio::Waiter::noop()).is_pending());
				let before = moq_clock::now();
				let read = moq_clock::timeout(Duration::from_millis(2), reader.read())
					.await
					.unwrap()
					.unwrap();
				assert!(matches!(read, Some(Read::Gap { .. })));
				assert_eq!(
					before.elapsed(),
					Duration::from_millis(1),
					"the hole timer wakes the reader"
				);
			} else {
				assert!(matches!(
					reader.poll_read(&kio::Waiter::noop()),
					Poll::Ready(Ok(Some(Read::Gap { .. })))
				));
			}
			assert!(
				matches!(reader.poll_read(&kio::Waiter::noop()), Poll::Ready(Ok(Some(Read::End { group: 2, end }))) if end == ts(10_000))
			);
			track.finish().unwrap();
		}
	}

	#[tokio::test(start_paused = true)]
	async fn a_filled_sequence_hole_gives_the_next_hole_a_fresh_budget() {
		let mut track = track_producer("data", hang::container::track_info(hang::catalog::PRIORITY.audio));
		let subscriber =
			track.subscribe(moq_net::track::Subscription::default().with_max_delay(Duration::from_millis(100)));
		let mut reader = Consumer::new(subscriber, Container::Legacy(crate::container::Kind::Data));
		write_group(&mut track, 0, &[ts(0)]);
		assert!(matches!(
			reader.read().await.unwrap(),
			Some(Read::Frame { group: 0, .. })
		));
		write_group(&mut track, 2, &[ts(20_000)]);
		assert!(reader.poll_read(&kio::Waiter::noop()).is_pending());
		tokio::time::advance(Duration::from_millis(50)).await;
		write_group(&mut track, 1, &[ts(10_000)]);
		assert!(matches!(
			reader.read().await.unwrap(),
			Some(Read::Frame { group: 1, .. })
		));
		assert!(matches!(
			reader.read().await.unwrap(),
			Some(Read::Frame { group: 2, .. })
		));
		let mut marker = track.create_group(moq_net::group::Info { sequence: 4 }).unwrap();
		marker.write_frame(ts(20_000), Bytes::new()).unwrap();
		marker.finish().unwrap();
		assert!(reader.poll_read(&kio::Waiter::noop()).is_pending());
		tokio::time::advance(Duration::from_millis(99)).await;
		assert!(reader.poll_read(&kio::Waiter::noop()).is_pending());
		tokio::time::advance(Duration::from_millis(1)).await;
		assert!(matches!(
			reader.poll_read(&kio::Waiter::noop()),
			Poll::Ready(Ok(Some(Read::Gap { .. })))
		));
		assert!(matches!(reader.read().await.unwrap(), Some(Read::End { group: 4, .. })));
		track.finish().unwrap();
	}

	/// The holes a source left behind when it stopped expire together, a budget after the groups
	/// past them arrived, rather than one budget each as the cursor reaches them.
	#[tokio::test(start_paused = true)]
	async fn holes_behind_a_stalled_source_share_one_budget() {
		let budget = Duration::from_millis(100);
		let mut track = track_producer("data", hang::container::track_info(hang::catalog::PRIORITY.audio));
		let subscriber = track.subscribe(moq_net::track::Subscription::default().with_max_delay(budget));
		let mut reader = Consumer::new(subscriber, Container::Legacy(crate::container::Kind::Data));
		write_group(&mut track, 0, &[ts(0)]);
		assert!(matches!(
			reader.read().await.unwrap(),
			Some(Read::Frame { group: 0, .. })
		));
		for sequence in [2, 4, 6] {
			write_group(&mut track, sequence, &[ts(sequence * 10_000)]);
		}
		assert!(reader.poll_read(&kio::Waiter::noop()).is_pending());
		tokio::time::advance(budget).await;
		let mut read = Vec::new();
		while let Poll::Ready(Ok(Some(item))) = reader.poll_read(&kio::Waiter::noop()) {
			read.push(match item {
				Read::Frame { group, .. } => format!("frame {group}"),
				Read::Gap { .. } => "gap".to_string(),
				Read::End { .. } => "end".to_string(),
			});
		}
		assert_eq!(read, ["gap", "frame 2", "gap", "frame 4", "gap", "frame 6"]);
	}

	/// A group that arrives late below an earlier one inherits that earlier arrival: the holes
	/// around it were already observable then.
	#[tokio::test(start_paused = true)]
	async fn a_hole_is_late_from_the_earliest_arrival_past_it() {
		let budget = Duration::from_millis(100);
		let mut track = track_producer("data", hang::container::track_info(hang::catalog::PRIORITY.audio));
		let subscriber = track.subscribe(moq_net::track::Subscription::default().with_max_delay(budget));
		let mut reader = Consumer::new(subscriber, Container::Legacy(crate::container::Kind::Data));
		write_group(&mut track, 0, &[ts(0)]);
		assert!(matches!(
			reader.read().await.unwrap(),
			Some(Read::Frame { group: 0, .. })
		));
		write_group(&mut track, 4, &[ts(40_000)]);
		assert!(reader.poll_read(&kio::Waiter::noop()).is_pending());
		tokio::time::advance(budget / 2).await;
		write_group(&mut track, 2, &[ts(20_000)]);
		assert!(reader.poll_read(&kio::Waiter::noop()).is_pending());
		tokio::time::advance(budget / 2).await;
		let mut read = Vec::new();
		while let Poll::Ready(Ok(Some(item))) = reader.poll_read(&kio::Waiter::noop()) {
			read.push(match item {
				Read::Frame { group, .. } => format!("frame {group}"),
				Read::Gap { .. } => "gap".to_string(),
				Read::End { .. } => "end".to_string(),
			});
		}
		assert_eq!(read, ["gap", "frame 2", "gap", "frame 4"]);
	}

	/// A malformed data marker group found while looking ahead fails the reader, rather than
	/// reading as a gap when a latency skip passes over it.
	#[tokio::test(start_paused = true)]
	async fn a_malformed_marker_passed_by_a_skip_fails_the_reader() {
		let mut track = track_producer("data", hang::container::track_info(hang::catalog::PRIORITY.audio));
		let subscription = moq_net::track::Subscription::default()
			.with_max_delay(Duration::from_secs(10))
			.with_range(0..);
		let mut reader = Consumer::new(
			track.subscribe(subscription),
			Container::Legacy(crate::container::Kind::Data),
		);
		reader.max_delay = Duration::ZERO;
		// The current group stays open, so only a skip moves past it.
		let mut slow = track.create_group(moq_net::group::Info { sequence: 0 }).unwrap();
		hang::container::Frame {
			timestamp: ts(0),
			payload: Bytes::from_static(b"a"),
		}
		.write_to(&mut slow)
		.unwrap();
		let mut group = track.create_group(moq_net::group::Info { sequence: 1 }).unwrap();
		group.write_frame(ts(10_000), Bytes::new()).unwrap();
		hang::container::Frame {
			timestamp: ts(10_000),
			payload: Bytes::from_static(b"b"),
		}
		.write_to(&mut group)
		.unwrap();
		group.finish().unwrap();
		write_group(&mut track, 2, &[ts(1_000_000)]);

		let read = loop {
			match reader.read().await {
				Ok(Some(Read::Frame { frame, .. })) if frame.timestamp == ts(0) => continue,
				other => break other,
			}
		};
		assert!(read.is_err(), "{read:?}");
	}

	/// An object with no bytes marks a break only on a data track: on a media track it is
	/// malformed, and fails to decode rather than read as a marker.
	#[test]
	fn an_empty_object_is_a_marker_only_on_a_data_track() {
		for kind in [
			crate::container::Kind::Data,
			crate::container::Kind::Audio,
			crate::container::Kind::Video,
		] {
			let read = Container::Legacy(kind).decode(moq_net::frame::Frame::new(ts(0), Bytes::new()));
			match kind {
				crate::container::Kind::Data => assert!(matches!(read, Ok(frames) if frames.is_empty())),
				_ => assert!(read.is_err(), "{kind:?}: {read:?}"),
			}
		}
	}

	/// A data track's marker is alone in its group: one sharing it with another object, a
	/// frame or a second marker, is malformed rather than a break or a frame.
	#[tokio::test(start_paused = true)]
	async fn a_data_marker_sharing_its_group_is_malformed() {
		for second in [Bytes::new(), Bytes::from_static(b"a")] {
			for marker_first in [true, false] {
				let mut track = track_producer("data", hang::container::track_info(hang::catalog::PRIORITY.audio));
				let subscription = moq_net::track::Subscription::default()
					.with_max_delay(Duration::from_secs(10))
					.with_range(0..);
				let mut reader = Consumer::new(
					track.subscribe(subscription),
					Container::Legacy(crate::container::Kind::Data),
				);
				write_group(&mut track, 0, &[ts(0)]);
				let mut group = track.create_group(moq_net::group::Info { sequence: 1 }).unwrap();
				let mut objects = [Bytes::new(), second.clone()];
				if !marker_first {
					objects.reverse();
				}
				for object in objects {
					match object.is_empty() {
						true => group.write_frame(ts(10_000), object).unwrap(),
						false => hang::container::Frame {
							timestamp: ts(10_000),
							payload: object,
						}
						.write_to(&mut group)
						.unwrap(),
					}
				}
				group.finish().unwrap();
				track.finish().unwrap();

				let error = loop {
					match reader.read().await {
						// A frame ahead of the marker goes out before the marker is seen.
						Ok(Some(Read::Frame { .. })) => continue,
						other => break other,
					}
				};
				assert!(
					error.is_err(),
					"second {second:?}, marker first {marker_first}: {error:?}"
				);
			}
		}
	}

	/// A gap runs from the delivered edge to the resume point, in the resume's scale, and
	/// never past it.
	#[test]
	fn a_gap_spans_the_edge_to_the_resume_in_its_scale() {
		let scale = moq_net::Timescale::new(90_000).unwrap();
		let resume = Timestamp::new(90_000, scale).unwrap();
		let Item::Gap { timestamp, duration } = gap(Duration::from_millis(500), resume) else {
			unreachable!()
		};
		assert_eq!(timestamp, Timestamp::new(45_000, scale).unwrap());
		assert_eq!(duration, Timestamp::new(45_000, scale).unwrap());

		// A next group overlapping the edge leaves an empty span at the resume point.
		let Item::Gap { timestamp, duration } = gap(Duration::from_secs(2), resume) else {
			unreachable!()
		};
		assert_eq!(timestamp, resume);
		assert_eq!(duration, Timestamp::new(0, scale).unwrap());
	}

	#[tokio::test]
	async fn gaps_mark_holes_while_a_marker_reads_as_an_end() {
		let mut track = track_producer("gaps", hang::container::track_info(hang::catalog::PRIORITY.audio));
		let subscription =
			track.subscribe(moq_net::track::Subscription::default().with_max_delay(Duration::from_secs(10)));
		let mut reader = Consumer::new(subscription, Container::Legacy(crate::container::Kind::Audio));
		write_group(&mut track, 0, &[ts(0), ts(10_000)]);
		write_group(&mut track, 2, &[ts(1_000_000), ts(1_010_000)]);
		write_marker_group(&mut track, 3, ts(1_020_000));
		write_group(&mut track, 4, &[ts(2_000_000), ts(2_010_000)]);
		track.finish().unwrap();
		assert_eq!(
			items(&mut reader).await,
			[
				// Nothing was read before the join, so it is no gap. Legacy carries no duration, so
				// the hole starts at the last frame read.
				"0",
				"10000",
				"gap 10000+990000",
				"1000000",
				"1010000",
				"end 1020000",
				"2000000",
				"2010000"
			]
		);
	}

	#[tokio::test]
	async fn a_latency_skip_is_a_gap() {
		let mut track = track_producer("gaps", hang::container::track_info(hang::catalog::PRIORITY.audio));
		let subscription =
			track.subscribe(moq_net::track::Subscription::default().with_max_delay(Duration::from_secs(10)));
		let mut reader = Consumer::new(subscription, Container::Legacy(crate::container::Kind::Audio));
		reader.max_delay = Duration::ZERO;
		write_group(&mut track, 0, &[ts(0)]);
		write_group(&mut track, 3, &[ts(1_000_000)]);
		track.finish().unwrap();
		assert_eq!(items(&mut reader).await, ["0", "gap 0+1000000", "1000000"]);
	}

	/// A video group's trailing duration marker is that frame's end, not a declared stop.
	#[tokio::test]
	async fn a_trailing_duration_marker_is_not_an_end() {
		let track = track_producer("gaps", hang::container::track_info(hang::catalog::PRIORITY.video));
		let subscription =
			track.subscribe(moq_net::track::Subscription::default().with_max_delay(Duration::from_secs(10)));
		let mut reader = Consumer::new(subscription, Container::Legacy(crate::container::Kind::Video));
		let wire = Container::Legacy(crate::container::Kind::Video);
		for (sequence, start) in [(0, 0), (1, 33_000)] {
			let mut group = track.create_group(moq_net::group::Info { sequence }).unwrap();
			let frame = |timestamp: u64, payload| Frame {
				timestamp: ts(timestamp),
				payload,
				keyframe: timestamp == start,
				duration: None,
			};
			wire.write(
				&mut group,
				&[
					frame(start, Bytes::from_static(&[1])),
					frame(start + 33_000, Bytes::new()),
				],
			)
			.unwrap();
			group.finish().unwrap();
		}
		track.finish().unwrap();
		assert_eq!(items(&mut reader).await, ["0", "33000"]);
	}

	/// An audio terminal opens its group with the endpoint, then the codec's final packets.
	#[tokio::test]
	async fn an_audio_terminal_reads_its_end_before_the_final_packets() {
		let mut track = track_producer("gaps", hang::container::track_info(hang::catalog::PRIORITY.audio));
		let subscription =
			track.subscribe(moq_net::track::Subscription::default().with_max_delay(Duration::from_secs(10)));
		let mut reader = Consumer::new(subscription, Container::Legacy(crate::container::Kind::Audio));
		write_group(&mut track, 0, &[ts(0), ts(20_000)]);
		let mut group = track.create_group(moq_net::group::Info { sequence: 1 }).unwrap();
		let frame = |timestamp: u64, payload| Frame {
			timestamp: ts(timestamp),
			payload,
			keyframe: false,
			duration: None,
		};
		Container::Legacy(crate::container::Kind::Audio)
			.write(
				&mut group,
				&[frame(50_000, Bytes::new()), frame(40_000, Bytes::from_static(&[1]))],
			)
			.unwrap();
		group.finish().unwrap();
		track.finish().unwrap();
		assert_eq!(items(&mut reader).await, ["0", "20000", "end 50000", "40000"]);
	}

	#[tokio::test]
	async fn a_marker_group_declares_an_end() {
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.audio));
		let consumer_track =
			track.subscribe(moq_net::track::Subscription::default().with_max_delay(Duration::from_secs(2)));
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Audio));

		write_group(&mut track, 0, &[ts(0)]);
		write_marker_group(&mut track, 1, ts(0));
		write_group(&mut track, 2, &[ts(1_000_000)]);
		track.finish().unwrap();

		assert_eq!(items(&mut consumer).await, ["0", "end 0", "1000000"]);
	}

	/// Missing media reads as a gap; only the publisher can declare an end.
	#[tokio::test]
	async fn only_a_marker_declares_an_end() {
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.audio));
		let consumer_track =
			track.subscribe(moq_net::track::Subscription::default().with_max_delay(Duration::from_secs(2)));
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Audio));

		write_group(&mut track, 0, &[ts(0)]);
		// Group 1 never arrives.
		write_group(&mut track, 2, &[ts(1_000_000)]);
		write_marker_group(&mut track, 3, ts(1_000_000));
		write_group(&mut track, 4, &[ts(2_000_000)]);
		track.finish().unwrap();

		assert_eq!(
			items(&mut consumer).await,
			["0", "gap 0+1000000", "1000000", "end 1000000", "2000000"]
		);
	}

	#[tokio::test]
	async fn empty_groups_mean_nothing() {
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.audio));
		let consumer_track =
			track.subscribe(moq_net::track::Subscription::default().with_max_delay(Duration::from_secs(2)));
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Audio));

		write_group(&mut track, 0, &[ts(0)]);
		track
			.create_group(moq_net::group::Info { sequence: 1 })
			.unwrap()
			.finish()
			.unwrap();
		write_group(&mut track, 2, &[ts(1_000)]);
		track.finish().unwrap();

		assert_eq!(items(&mut consumer).await, ["0", "1000"]);
	}

	/// A live-edge reader positioned on the largest group while it is still empty does not
	/// stall there: once a budget of newer media lands, it moves on like any stalled group.
	#[tokio::test]
	async fn a_live_edge_start_on_an_empty_group_moves_on() {
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.audio));
		write_group(&mut track, 0, &[ts(0)]);
		let _empty = track.create_group(moq_net::group::Info { sequence: 1 }).unwrap();

		let mut subscriber =
			track.subscribe(moq_net::track::Subscription::default().with_max_delay(Duration::from_millis(500)));
		let edge = match subscriber.poll_largest(&moq_net::kio::Waiter::noop()) {
			std::task::Poll::Ready(Some(edge)) => edge,
			_ => panic!("the empty group is the largest"),
		};
		assert_eq!(edge.group, 1);
		subscriber = subscriber.with_range(edge.group..);
		let mut consumer = Consumer::new(subscriber, Container::Legacy(crate::container::Kind::Audio));

		write_group(&mut track, 2, &[ts(1_000_000), ts(1_600_000)]);
		track.finish().unwrap();

		assert_eq!(items(&mut consumer).await, ["1000000", "1600000"]);
	}

	#[tokio::test]
	async fn latency_skip_reports_a_gap() {
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.audio));
		let consumer_track =
			track.subscribe(moq_net::track::Subscription::default().with_max_delay(Duration::from_secs(2)));
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Audio));
		// Keep transport filtering out of this test so it isolates the mux skip logic.
		consumer.max_delay = Duration::ZERO;

		write_group(&mut track, 0, &[ts(0)]);
		write_group(&mut track, 3, &[ts(1_000_000)]);
		track.finish().unwrap();

		assert_eq!(items(&mut consumer).await, ["0", "gap 0+1000000", "1000000"]);
	}

	#[tokio::test]
	async fn read_multiple_frames_single_group() {
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track =
			track.subscribe(moq_net::track::Subscription::default().with_max_delay(Duration::from_millis(500)));
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Data));

		write_group(&mut track, 0, &[ts(0), ts(33_000), ts(66_000)]);
		track.finish().unwrap();

		let frames = read_all(&mut consumer).await.unwrap();
		assert_eq!(frames.len(), 3);
		assert_eq!(frames[0].timestamp, ts(0));
		assert_eq!(frames[1].timestamp, ts(33_000));
		assert_eq!(frames[2].timestamp, ts(66_000));

		assert!(frames[0].keyframe);
	}

	#[tokio::test]
	async fn read_multiple_groups_within_latency() {
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track =
			track.subscribe(moq_net::track::Subscription::default().with_max_delay(Duration::from_millis(500)));
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Data));

		// 5 groups, 20ms spacing. Total span = 80ms, well within the 500ms max delay.
		for i in 0..5u64 {
			write_group(&mut track, i, &[ts(i * 20_000)]);
		}
		track.finish().unwrap();

		let frames = read_all(&mut consumer).await.unwrap();
		assert_eq!(frames.len(), 5);
	}

	// ---- Age Skipping ----

	#[tokio::test]
	async fn latency_skip_delivers_recent_groups() {
		tokio::time::pause();
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track =
			track.subscribe(moq_net::track::Subscription::default().with_max_delay(Duration::from_millis(100)));
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Data));

		// Group 0: 5 frames, NOT finished (blocks consumer)
		let mut group0 = track.create_group(moq_net::group::Info { sequence: 0 }).unwrap();
		for f in 0..5u64 {
			Container::Legacy(crate::container::Kind::Data)
				.write(
					&mut group0,
					&[Frame {
						timestamp: ts(f * 2_000),
						payload: Bytes::from_static(&[0xDE, 0xAD]),
						keyframe: false,
						duration: None,
					}],
				)
				.unwrap();
		}

		// Groups 1-19: finished, 15ms spacing, 5 frames each
		for g in 1..20u64 {
			let timestamps: Vec<_> = (0..5).map(|f| ts(g * 15_000 + f * 2_000)).collect();
			write_group(&mut track, g, &timestamps);
		}
		track.finish().unwrap();

		// Finish group 0 after consumer has had time to accumulate pending groups
		let finisher = tokio::spawn(async move {
			tokio::time::sleep(Duration::from_millis(50)).await;
			group0.finish().unwrap();
		});

		let frames = read_all(&mut consumer).await.unwrap();
		// Group 0's 5 frames + some later groups (earlier ones skipped by age)
		assert!(frames.len() >= 25, "Expected >= 25 frames, got {}", frames.len());
		finisher.await.expect("finisher task panicked");
	}

	#[tokio::test]
	async fn zero_latency_skips_aggressively() {
		tokio::time::pause();
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track = track.subscribe(None);
		let mut consumer = container_max_delay_only(consumer_track, Duration::ZERO);

		// Group 0 at ts 0 keeps timestamps monotonic with sequence (groups 1-9 follow at
		// g*50 ms), so the test exercises age skipping and not rewind detection.
		let mut group0 = track.create_group(moq_net::group::Info { sequence: 0 }).unwrap();
		Container::Legacy(crate::container::Kind::Data)
			.write(
				&mut group0,
				&[Frame {
					timestamp: ts(0),
					payload: Bytes::from_static(&[0xDE, 0xAD]),
					keyframe: false,
					duration: None,
				}],
			)
			.unwrap();

		for g in 1..10u64 {
			let timestamps: Vec<_> = (0..3).map(|f| ts(g * 50_000 + f * 5_000)).collect();
			write_group(&mut track, g, &timestamps);
		}
		track.finish().unwrap();

		let finisher = tokio::spawn(async move {
			tokio::time::sleep(Duration::from_millis(50)).await;
			group0.finish().unwrap();
		});

		let frames = read_all(&mut consumer).await.unwrap();
		assert_eq!(frames.len(), 28, "Expected group 0 frame + groups 1-9");
		assert!(!frames.is_empty(), "Expected at least some frames");
		finisher.await.expect("finisher task panicked");
	}

	#[tokio::test]
	async fn latency_skip_correctness() {
		tokio::time::pause();
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track = track.subscribe(None);
		let mut consumer = container_max_delay_only(consumer_track, Duration::from_millis(100));

		let mut group0 = track.create_group(moq_net::group::Info { sequence: 0 }).unwrap();
		Container::Legacy(crate::container::Kind::Data)
			.write(
				&mut group0,
				&[Frame {
					timestamp: ts(0),
					payload: Bytes::from_static(&[0xDE, 0xAD]),
					keyframe: false,
					duration: None,
				}],
			)
			.unwrap();

		for g in 1..10u64 {
			write_group(&mut track, g, &[ts(g * 30_000)]);
		}
		track.finish().unwrap();

		let finisher = tokio::spawn(async move {
			tokio::time::sleep(Duration::from_millis(50)).await;
			group0.finish().unwrap();
		});

		let frames = read_all(&mut consumer).await.unwrap();
		assert!(!frames.is_empty(), "Expected at least some frames");
		assert_eq!(frames.len(), 10, "Expected group 0 frame + groups 1-9");
		assert_eq!(frames[0].timestamp, ts(0));

		for i in 1..10u64 {
			assert_eq!(frames[i as usize].timestamp, ts(i * 30_000));
		}
		finisher.await.expect("finisher task panicked");
	}

	/// A group longer than the budget that blocks for a moment behind a newer group is
	/// not cut short while what it could still present (up to where the newer group
	/// begins) is within the budget. A join's backlog does exactly this: the newer group
	/// races ahead on its own stream while the older one is still arriving.
	#[test]
	fn a_long_group_blocked_behind_a_newer_one_is_not_cut_short() {
		let waiter = kio::Waiter::noop();
		let track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let mut consumer = container_max_delay_only(track.subscribe(None), Duration::from_secs(2));
		let write = |group: &mut moq_net::group::Producer, timestamp: Timestamp| {
			let frame = Frame {
				timestamp,
				payload: Bytes::from_static(&[0xDE, 0xAD]),
				keyframe: false,
				duration: None,
			};
			Container::Legacy(crate::container::Kind::Data)
				.write(group, &[frame])
				.unwrap();
		};
		let read = |consumer: &mut Consumer<Container>| loop {
			match consumer.poll_read(&waiter) {
				Poll::Ready(Ok(Some(Read::Frame { frame, .. }))) => break Some(frame.timestamp),
				Poll::Ready(Ok(Some(Read::Gap { .. }))) => continue,
				Poll::Pending => break None,
				other => panic!("unexpected read: {other:?}"),
			}
		};

		// A 2.5 s group, half arrived, and the next one already 0.5 s in.
		let mut group0 = track.create_group(moq_net::group::Info { sequence: 0 }).unwrap();
		write(&mut group0, ts(0));
		write(&mut group0, ts(500_000));
		let mut group1 = track.create_group(moq_net::group::Info { sequence: 1 }).unwrap();
		write(&mut group1, ts(2_500_000));
		write(&mut group1, ts(3_000_000));

		assert_eq!(read(&mut consumer), Some(ts(0)));
		assert_eq!(read(&mut consumer), Some(ts(500_000)));
		// Group 0's reach (2.5 s) is 0.5 s behind the newest frame: wait for it.
		assert_eq!(read(&mut consumer), None, "cut group 0 short");
		write(&mut group0, ts(1_000_000));
		assert_eq!(read(&mut consumer), Some(ts(1_000_000)));
		group0.finish().unwrap();
		assert_eq!(read(&mut consumer), Some(ts(2_500_000)));

		// Once the reach falls a full budget behind, the stalled group is skipped.
		let mut group2 = track.create_group(moq_net::group::Info { sequence: 2 }).unwrap();
		write(&mut group2, ts(5_000_000));
		write(&mut group2, ts(7_000_000));
		assert_eq!(read(&mut consumer), Some(ts(3_000_000)));
		assert_eq!(read(&mut consumer), Some(ts(5_000_000)));
		group1.finish().unwrap();
		group2.finish().unwrap();
	}

	// ---- Malformed rewind ----

	/// The track refuses a group whose first frame rewinds, but not a later frame that does,
	/// so the consumer floors every frame against the previous group's start.
	#[tokio::test]
	async fn a_frame_below_the_previous_start_aborts() {
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let mut consumer = Consumer::new(track.subscribe(None), Container::Legacy(crate::container::Kind::Data));
		write_group(&mut track, 0, &[ts(100_000)]);
		assert_eq!(consumer.read_frame().await.unwrap().unwrap().timestamp, ts(100_000));
		write_group(&mut track, 1, &[ts(100_000), ts(0)]);
		track.finish().unwrap();
		let err = loop {
			match consumer.read().await {
				Ok(Some(_)) => continue,
				Ok(None) => panic!("the rewound frame ended the track cleanly"),
				Err(err) => break err,
			}
		};
		assert!(matches!(
			err,
			crate::Error::TimestampRewind(TimestampRewind { timestamp, floor })
				if timestamp == ts(0) && floor == ts(100_000)
		));
		assert_eq!(
			err.to_string(),
			"frame timestamp 0 µs is below the previous group's start 100000 µs"
		);
	}

	#[tokio::test]
	async fn b_frames_within_a_group_are_accepted() {
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let mut consumer = Consumer::new(track.subscribe(None), Container::Legacy(crate::container::Kind::Data));
		write_group(&mut track, 0, &[ts(0), ts(66_000), ts(33_000)]);
		track.finish().unwrap();
		let frames = continuous_frames(&mut consumer).await;
		assert_eq!(
			frames.iter().map(|f| f.timestamp).collect::<Vec<_>>(),
			vec![ts(0), ts(66_000), ts(33_000)]
		);
	}

	#[tokio::test]
	async fn a_group_starting_at_the_previous_start_is_accepted() {
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.audio));
		let mut consumer = Consumer::new(track.subscribe(None), Container::Legacy(crate::container::Kind::Data));
		write_group(&mut track, 0, &[ts(100_000)]);
		assert_eq!(consumer.read_frame().await.unwrap().unwrap().timestamp, ts(100_000));
		write_group(&mut track, 1, &[ts(100_000)]);
		track.finish().unwrap();
		assert_eq!(consumer.read_frame().await.unwrap().unwrap().timestamp, ts(100_000));
	}

	/// A keyframe one frame below the previous group's last frame, but above its start, is an
	/// overlap rather than a rewind.
	#[tokio::test]
	async fn a_keyframe_overlapping_the_previous_group_is_accepted() {
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let mut consumer = Consumer::new(track.subscribe(None), Container::Legacy(crate::container::Kind::Data));
		write_group(&mut track, 0, &[ts(0), ts(33_000), ts(66_000)]);
		// Read the first group before the second arrives: a blocked group that the next one
		// already covers is skipped for latency, which is not what this checks.
		for expected in [0, 33_000, 66_000] {
			assert_eq!(consumer.read_frame().await.unwrap().unwrap().timestamp, ts(expected));
		}
		write_group(&mut track, 1, &[ts(50_000), ts(83_000)]);
		track.finish().unwrap();
		let frames = read_all(&mut consumer).await.unwrap();
		assert_eq!(
			frames.iter().map(|f| f.timestamp.as_micros()).collect::<Vec<_>>(),
			vec![50_000, 83_000]
		);
	}

	#[tokio::test]
	async fn open_gop_leading_pictures_above_the_previous_group_are_accepted() {
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let mut consumer = Consumer::new(track.subscribe(None), Container::Legacy(crate::container::Kind::Data));
		write_group(&mut track, 0, &[ts(0), ts(33_000)]);
		// Read the first group before the second arrives: a superseded group is skipped
		// for latency, which is not what this checks.
		for expected in [0, 33_000] {
			assert_eq!(consumer.read_frame().await.unwrap().unwrap().timestamp, ts(expected));
		}
		write_group(&mut track, 1, &[ts(66_000), ts(50_000)]);
		track.finish().unwrap();
		let frames = continuous_frames(&mut consumer).await;
		assert_eq!(
			frames.iter().map(|f| f.timestamp.as_micros()).collect::<Vec<_>>(),
			vec![66_000, 50_000]
		);
	}

	#[tokio::test]
	async fn a_marker_group_and_forward_jump_reads_one_end() {
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.audio));
		let mut consumer = Consumer::new(
			track.subscribe(moq_net::track::Subscription::default().with_max_delay(Duration::from_secs(2))),
			Container::Legacy(crate::container::Kind::Audio),
		);
		write_group(&mut track, 0, &[ts(0)]);
		write_marker_group(&mut track, 1, ts(0));
		write_group(&mut track, 2, &[ts(1_000_000)]);
		track.finish().unwrap();
		assert_eq!(items(&mut consumer).await, ["0", "end 0", "1000000"]);
	}

	#[tokio::test]
	async fn a_zero_budget_idle_resume_reports_the_shed_marker_as_a_gap() {
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.audio));
		let mut consumer = Consumer::new(track.subscribe(None), Container::Legacy(crate::container::Kind::Audio));
		consumer.max_delay = Duration::ZERO;
		write_group(&mut track, 0, &[ts(0)]);
		assert_eq!(consumer.read_frame().await.unwrap().unwrap().timestamp, ts(0));
		write_marker_group(&mut track, 1, ts(0));
		write_group(&mut track, 2, &[ts(1_000_000)]);
		track.finish().unwrap();
		assert!(matches!(consumer.read().await.unwrap(), Some(Read::Gap { .. })));
		assert_eq!(consumer.read_frame().await.unwrap().unwrap().timestamp, ts(1_000_000));
	}

	#[tokio::test]
	async fn a_zero_budget_skip_reports_a_contiguous_shed_marker_as_a_gap() {
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.audio));
		let mut consumer = Consumer::new(track.subscribe(None), Container::Legacy(crate::container::Kind::Audio));
		consumer.max_delay = Duration::ZERO;

		let mut group0 = track.create_group(moq_net::group::Info { sequence: 0 }).unwrap();
		Container::Legacy(crate::container::Kind::Audio)
			.write(
				&mut group0,
				&[Frame {
					timestamp: ts(0),
					payload: Bytes::from_static(&[0xDE, 0xAD]),
					keyframe: false,
					duration: None,
				}],
			)
			.unwrap();
		assert_eq!(consumer.read_frame().await.unwrap().unwrap().timestamp, ts(0));

		write_marker_group(&mut track, 1, ts(10_000));
		write_group(&mut track, 2, &[ts(10_000)]);

		assert!(matches!(consumer.read().await.unwrap(), Some(Read::Gap { .. })));
		assert_eq!(consumer.read_frame().await.unwrap().unwrap().timestamp, ts(10_000));
		group0.finish().unwrap();
	}

	#[tokio::test]
	async fn a_later_frame_below_the_previous_group_start_aborts() {
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let mut consumer = Consumer::new(track.subscribe(None), Container::Legacy(crate::container::Kind::Data));
		write_group(&mut track, 0, &[ts(100_000)]);
		assert_eq!(consumer.read_frame().await.unwrap().unwrap().timestamp, ts(100_000));
		write_group(&mut track, 1, &[ts(200_000), ts(50_000)]);
		track.finish().unwrap();
		assert_eq!(consumer.read_frame().await.unwrap().unwrap().timestamp, ts(200_000));
		let err = consumer.read().await.unwrap_err();
		assert!(matches!(
			err,
			crate::Error::TimestampRewind(TimestampRewind { timestamp, floor })
				if timestamp == ts(50_000) && floor == ts(100_000)
		));
		assert_eq!(
			err.to_string(),
			"frame timestamp 50000 µs is below the previous group's start 100000 µs"
		);
	}

	// ---- Empty payloads ----

	/// Write one frame with an empty payload: a marker saying content stops at
	/// `timestamp`, carrying no media.
	fn write_marker(group: &mut moq_net::group::Producer, timestamp: Timestamp) {
		let frame = Frame {
			timestamp,
			payload: Bytes::new(),
			keyframe: false,
			duration: None,
		};
		Container::Legacy(crate::container::Kind::Data)
			.write(group, &[frame])
			.unwrap();
	}

	/// An empty payload carries no media, so it's skipped rather than surfaced as a
	/// frame or raised as an error. It times the previous frame and never means the
	/// track ended.
	#[tokio::test]
	async fn empty_payload_is_skipped() {
		let track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track =
			track.subscribe(moq_net::track::Subscription::default().with_max_delay(Duration::from_millis(500)));
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Video));

		let mut group = track.create_group(moq_net::group::Info { sequence: 0 }).unwrap();
		let media = |timestamp| Frame {
			timestamp,
			payload: Bytes::from_static(&[0xDE, 0xAD]),
			keyframe: false,
			duration: None,
		};
		Container::Legacy(crate::container::Kind::Video)
			.write(&mut group, &[media(ts(0))])
			.unwrap();
		write_marker(&mut group, ts(16_000)); // closes the first frame
		Container::Legacy(crate::container::Kind::Video)
			.write(&mut group, &[media(ts(33_000))])
			.unwrap();
		write_marker(&mut group, ts(50_000)); // the group's end
		group.finish().unwrap();
		track.finish().unwrap();

		let frames = read_all(&mut consumer).await.unwrap();
		assert_eq!(frames.len(), 2, "markers are not surfaced as media");
		assert_eq!(frames[0].timestamp, ts(0));
		assert_eq!(frames[1].timestamp, ts(33_000));
	}

	#[tokio::test]
	async fn leading_marker_preserves_the_first_media_keyframe() {
		let track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track =
			track.subscribe(moq_net::track::Subscription::default().with_max_delay(Duration::from_millis(500)));
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Video));

		let mut group = track.create_group(moq_net::group::Info { sequence: 0 }).unwrap();
		write_marker(&mut group, ts(20_000));
		Container::Legacy(crate::container::Kind::Video)
			.write(
				&mut group,
				&[Frame {
					timestamp: ts(20_000),
					payload: Bytes::from_static(&[0xDE, 0xAD]),
					keyframe: false,
					duration: None,
				}],
			)
			.unwrap();
		group.finish().unwrap();
		track.finish().unwrap();

		let frame = consumer.read_frame().await.unwrap().unwrap();
		assert!(frame.keyframe, "the marker does not consume the first-media slot");
		assert!(frame.duration.is_none(), "a leading marker has no previous frame");
	}

	/// Reading a marker consumes its frame, so a run of them makes progress and the
	/// consumer reaches the next group instead of spinning. `read_all` times out per
	/// read, so a stall or an infinite loop fails this rather than hanging forever.
	#[tokio::test]
	async fn consecutive_markers_do_not_stall() {
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track =
			track.subscribe(moq_net::track::Subscription::default().with_max_delay(Duration::from_millis(500)));
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Video));

		let mut group = track.create_group(moq_net::group::Info { sequence: 0 }).unwrap();
		Container::Legacy(crate::container::Kind::Video)
			.write(
				&mut group,
				&[Frame {
					timestamp: ts(0),
					payload: Bytes::from_static(&[0xDE, 0xAD]),
					keyframe: false,
					duration: None,
				}],
			)
			.unwrap();
		for i in 1..5u64 {
			write_marker(&mut group, ts(i * 1_000));
		}
		group.finish().unwrap();
		write_group(&mut track, 1, &[ts(100_000)]);
		track.finish().unwrap();

		let frames = read_all(&mut consumer).await.unwrap();
		let micros: Vec<u128> = frames.iter().map(|f| f.timestamp.as_micros()).collect();
		assert_eq!(micros, vec![0, 100_000], "markers skipped, next group reached");
	}

	/// LOC video consumers skip an empty payload: it is the duration marker, not media.
	#[tokio::test]
	async fn loc_empty_payload_is_skipped() {
		let track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track =
			track.subscribe(moq_net::track::Subscription::default().with_max_delay(Duration::from_millis(500)));
		let mut consumer = Consumer::new(consumer_track, Container::Loc(crate::container::Kind::Video));

		let mut group = track.create_group(moq_net::group::Info { sequence: 0 }).unwrap();
		let media = Frame {
			timestamp: ts(0),
			payload: Bytes::from_static(&[0xDE, 0xAD]),
			keyframe: true,
			duration: None,
		};
		Container::Loc(crate::container::Kind::Video)
			.write(&mut group, &[media])
			.unwrap();
		Container::Loc(crate::container::Kind::Video)
			.write(
				&mut group,
				&[Frame {
					timestamp: ts(33_000),
					payload: Bytes::new(),
					keyframe: false,
					duration: None,
				}],
			)
			.unwrap();
		group.finish().unwrap();
		track.finish().unwrap();

		let frames = read_all(&mut consumer).await.unwrap();
		assert_eq!(frames.len(), 1, "the empty LOC payload is not submitted");
		assert_eq!(frames[0].timestamp, ts(0));
	}

	// ---- Group Ordering ----

	#[tokio::test]
	async fn groups_delivered_in_sequence_order() {
		tokio::time::pause();
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track =
			track.subscribe(moq_net::track::Subscription::default().with_max_delay(Duration::from_millis(500)));
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Data));

		let mut group0 = track.create_group(moq_net::group::Info { sequence: 0 }).unwrap();
		Container::Legacy(crate::container::Kind::Data)
			.write(
				&mut group0,
				&[Frame {
					timestamp: ts(0),
					payload: Bytes::from_static(&[0xDE, 0xAD]),
					keyframe: false,
					duration: None,
				}],
			)
			.unwrap();

		write_group(&mut track, 2, &[ts(60_000)]);
		write_group(&mut track, 1, &[ts(30_000)]);
		track.finish().unwrap();

		let finisher = tokio::spawn(async move {
			tokio::time::sleep(Duration::from_millis(10)).await;
			group0.finish().unwrap();
		});

		let frames = read_all(&mut consumer).await.unwrap();
		assert_eq!(frames.len(), 3);
		assert_eq!(frames[0].timestamp, ts(0));
		assert_eq!(frames[1].timestamp, ts(30_000));
		assert_eq!(frames[2].timestamp, ts(60_000));
		finisher.await.expect("finisher task panicked");
	}

	#[tokio::test]
	async fn adjacent_group_flushed_immediately() {
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track =
			track.subscribe(moq_net::track::Subscription::default().with_max_delay(Duration::from_millis(500)));
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Data));

		write_group(&mut track, 0, &[ts(0)]);
		write_group(&mut track, 1, &[ts(30_000)]);
		track.finish().unwrap();

		let frames = read_all(&mut consumer).await.unwrap();
		assert_eq!(frames.len(), 2);
		assert_eq!(frames[0].timestamp, ts(0));
		assert_eq!(frames[1].timestamp, ts(30_000));
	}

	// ---- Track Lifecycle ----

	#[tokio::test]
	async fn empty_track_returns_none() {
		tokio::time::pause();
		let track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track =
			track.subscribe(moq_net::track::Subscription::default().with_max_delay(Duration::from_millis(500)));
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Data));

		track.finish().unwrap();

		let result = tokio::time::timeout(Duration::from_millis(200), consumer.read()).await;
		match result {
			Ok(Ok(None)) => {} // expected: track ended
			Ok(Ok(Some(_))) => panic!("expected None for empty track, got Some"),
			Ok(Err(e)) => panic!("expected None for empty track, got error: {e}"),
			Err(_) => panic!("should not hang on empty track"),
		}
	}

	#[tokio::test]
	async fn track_closed_with_error() {
		tokio::time::pause();
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track =
			track.subscribe(moq_net::track::Subscription::default().with_max_delay(Duration::from_millis(500)));
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Data));

		write_group(&mut track, 0, &[ts(0), ts(10_000)]);
		track.abort(moq_net::Error::Cancel).unwrap();

		// The groups already here drain before the track's error, as in JS.
		let mut frames = Vec::new();
		let end = loop {
			let read = tokio::time::timeout(Duration::from_millis(500), consumer.read())
				.await
				.expect("Consumer should not hang after track error");
			match read {
				Ok(Some(Read::Frame { frame, .. })) => frames.push(frame.timestamp),
				Ok(Some(_)) => continue,
				other => break other,
			}
		};
		assert_eq!(frames, [ts(0), ts(10_000)]);
		assert!(end.is_err(), "{end:?}");
		assert!(matches!(consumer.read().await, Ok(None)));
	}

	// ---- Gap Recovery ----

	#[tokio::test]
	async fn gap_in_group_sequence_recovery() {
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track =
			track.subscribe(moq_net::track::Subscription::default().with_max_delay(Duration::from_millis(100)));
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Data));

		write_group(&mut track, 0, &[ts(0), ts(20_000)]);
		write_group(&mut track, 1, &[ts(40_000), ts(60_000)]);
		write_group(&mut track, 3, &[ts(120_000), ts(140_000)]);
		write_group(&mut track, 4, &[ts(160_000), ts(180_000)]);
		write_group(&mut track, 5, &[ts(200_000), ts(220_000)]);
		write_group(&mut track, 6, &[ts(240_000), ts(260_000)]);
		track.finish().unwrap();

		let frames = read_all(&mut consumer).await.unwrap();
		assert!(frames.len() >= 4, "Expected >= 4 frames, got {}", frames.len());
	}

	#[tokio::test]
	async fn gap_at_start_of_sequence() {
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track =
			track.subscribe(moq_net::track::Subscription::default().with_max_delay(Duration::from_millis(80)));
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Data));

		write_group(&mut track, 5, &[ts(0), ts(20_000)]);
		write_group(&mut track, 7, &[ts(80_000), ts(100_000)]);
		write_group(&mut track, 8, &[ts(120_000), ts(140_000)]);
		write_group(&mut track, 9, &[ts(160_000), ts(180_000)]);
		track.finish().unwrap();

		let frames = read_all(&mut consumer).await.unwrap();
		assert!(frames.len() >= 4, "Expected >= 4 frames, got {}", frames.len());
	}

	#[tokio::test(start_paused = true)]
	async fn truncated_resumed_group_skips_to_the_next_clean_group() {
		// One publisher instance behind every route, so each may resume the others.
		let epoch = moq_net::Epoch::mint();
		let origin = crate::source::produce_origin();
		let hops = moq_net::Hops::try_from(vec![moq_net::Hop::new(10).unwrap()]).unwrap();
		let route = |cost: u64| {
			moq_net::origin::Route::default()
				.with_epoch(epoch.clone())
				.with_hops(hops.clone())
				.with_cost(cost)
		};
		let first = origin.create_broadcast("live").unwrap();
		let info = hang::container::track_info(hang::catalog::PRIORITY.video);
		let first_track = first.create_track("video", info.clone()).unwrap();
		first.announce(route(5)).unwrap();
		let broadcast = origin.consume().request("live", None).await.unwrap();
		let track = broadcast
			.track("video")
			.unwrap()
			.subscribe(moq_net::track::Subscription::default().with_max_delay(Duration::from_secs(2)))
			.await
			.unwrap();
		let format = Container::Legacy(crate::container::Kind::Data);
		let mut consumer = Consumer::new(track, Container::Legacy(crate::container::Kind::Data));

		let mut open = first_track.create_group(0u64.into()).unwrap();
		format
			.write(
				&mut open,
				&[Frame {
					timestamp: ts(0),
					payload: Bytes::from_static(&[0xDE, 0xAD]),
					keyframe: true,
					duration: None,
				}],
			)
			.unwrap();

		// Each cheaper route takes over beyond group 0 without holding its continuation,
		// while the first route stays up but silent. They carry the same broadcast, so each
		// holds every group from 1 on. The open group is given up once the track runs a
		// full budget past it.
		let mut sources = Vec::new();
		for sequence in 1..=4 {
			let source = origin.create_broadcast("live").unwrap();
			let mut track = source.create_track("video", info.clone()).unwrap();
			source.announce(route(5 - sequence)).unwrap();
			let mut demand = track.demand();
			while demand.wanted_changed().await.unwrap().is_none() {}
			if sequence == 1 {
				assert_eq!(consumer.read_frame().await.unwrap().unwrap().timestamp, ts(0));
			}
			for sequence in 1..=sequence {
				write_group(&mut track, sequence, &[ts(sequence * 1_000_000)]);
			}
			sources.push((source, track));
		}

		let waiter = kio::Waiter::noop();

		// The aborted group 0 says it was cut, then skips straight to its successor; a short
		// clean group emits GroupEnd.
		assert!(
			matches!(consumer.poll_event(&waiter), Poll::Ready(Ok(Some(Event::Cut { .. })))),
			"the cut is said before the successor"
		);
		let event = consumer.poll_event(&waiter);
		let Poll::Ready(Ok(Some(Event::Frame { frame, .. }))) = event else {
			panic!(
				"expected a successor frame, current={}; pending={}",
				consumer.current,
				event.is_pending()
			);
		};
		assert_eq!(frame.timestamp, ts(1_000_000));
		assert!(frame.keyframe);
		assert_eq!(consumer.current, 1);
		assert!(
			matches!(consumer.poll_event(&waiter), Poll::Ready(Ok(Some(Event::GroupEnd)))),
			"the complete successor still emits its clean boundary"
		);
		assert_eq!(consumer.read_frame().await.unwrap().unwrap().timestamp, ts(2_000_000));
	}

	// ---- Eviction recovery (pause/resume) ----

	/// A group that aged out of the relay cache (aborted with `Error::Old`) while the
	/// consumer was parked on it must not hang the consumer: reading it errors, and
	/// the consumer skips the gap to the next live group even though the track is NOT
	/// finished. This is the resume-from-pause path (the recorder stops reading, the
	/// group + the sequences after it evict, then it resumes).
	#[tokio::test]
	async fn evicted_group_with_gap_skips_to_live() {
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track =
			track.subscribe(moq_net::track::Subscription::default().with_max_delay(Duration::from_millis(100)));
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Data));

		// Group 0: a frame the consumer reads, positioning it there.
		let mut group0 = track.create_group(moq_net::group::Info { sequence: 0 }).unwrap();
		Container::Legacy(crate::container::Kind::Data)
			.write(
				&mut group0,
				&[Frame {
					timestamp: ts(0),
					payload: Bytes::from_static(&[0xDE, 0xAD]),
					keyframe: false,
					duration: None,
				}],
			)
			.unwrap();
		let first = consumer.read_frame().await.unwrap().unwrap();
		assert_eq!(first.timestamp, ts(0));

		// A live group arrives far ahead -- sequences 1..4 never come (evicted). The
		// track stays OPEN (not finished), the failure mode that used to hang.
		write_group(&mut track, 5, &[ts(150_000)]);

		// Group 0 ages out of the cache (the relay aborts it on eviction).
		group0.abort(moq_net::Error::Old).unwrap();

		// Must skip the evicted group + the gap and reach the live group, without
		// hanging on a track that never finishes.
		let next = tokio::time::timeout(Duration::from_secs(1), consumer.read())
			.await
			.expect("consumer hung on an evicted group / gap")
			.unwrap()
			.unwrap();
		assert!(
			matches!(next, Read::Gap { timestamp, duration } if timestamp == ts(0) && duration == ts(150_000)),
			"the evicted media is a gap from the last frame to the live group: {next:?}"
		);
		let next = consumer.read_frame().await.unwrap().unwrap();
		assert_eq!(next.timestamp, ts(150_000), "skipped the evicted gap to the live group");
	}

	/// A missing (evicted) sequence with a newer group buffered must be skipped once the
	/// age budget runs out, even while the track is still LIVE -- not only once it's
	/// finished. This is the recorder resume stall: `current` points at a sequence the
	/// cache dropped, higher groups are buffered, and the track never finishes. The gap
	/// is indistinguishable from a stream that lost the delivery race, so the skip fires
	/// only once the newest content is a full budget past what the gap could still hold.
	#[tokio::test]
	async fn missing_sequence_skips_on_live_track() {
		// The delivery budget runs on max(wall, pts): on wall time a stalled runner ages
		// group 2 out of delivery before the consumer reaches it.
		tokio::time::pause();
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track =
			track.subscribe(moq_net::track::Subscription::default().with_max_delay(Duration::from_millis(100)));
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Data));

		// Read group 0 first, positioning the cursor there before the newer groups
		// arrive; otherwise delivery convicts group 0 and the cursor never meets the gap.
		write_group(&mut track, 0, &[ts(0), ts(20_000)]);
		for expected in [ts(0), ts(20_000)] {
			let frame = consumer.read_frame().await.unwrap().unwrap();
			assert_eq!(frame.timestamp, expected);
		}

		// Group 2 arrives but sequence 1 is missing (evicted) and never does. The track
		// is NOT finished (live), the case that used to hang. Group 3 pushes the live
		// edge a full budget past group 2's start, expiring the gap at 1.
		write_group(&mut track, 2, &[ts(200_000)]);
		write_group(&mut track, 3, &[ts(320_000)]);

		// The age budget alone closes the gap: the deadline sits short of the 100ms
		// hole timer, so waiting it out fails the test.
		let frame = tokio::time::timeout(Duration::from_millis(50), consumer.read_frame())
			.await
			.expect("consumer hung on a missing sequence on a live track")
			.unwrap()
			.unwrap();
		assert_eq!(frame.timestamp, ts(200_000), "skipped the missing sequence to group 2");
	}

	/// The other half of the budget-gated gap: while the newest content is still within
	/// the age budget of what the missing sequence could present, the consumer waits for
	/// it instead of writing it off, and delivers it when its stream loses the race but
	/// still arrives (#3258).
	#[tokio::test]
	async fn gap_keeps_late_arriving_group_within_max_delay() {
		tokio::time::pause();
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track =
			track.subscribe(moq_net::track::Subscription::default().with_max_delay(Duration::from_millis(500)));
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Data));

		// Group 2's stream beats group 1's, which hasn't arrived at all yet.
		write_group(&mut track, 0, &[ts(0)]);
		write_group(&mut track, 2, &[ts(200_000)]);

		let first = consumer.read_frame().await.unwrap().expect("group 0 frame");
		assert_eq!(first.timestamp, ts(0));

		// Group 0 is done; group 1's stream is still racing. Within the budget the
		// consumer waits at the gap rather than skipping to group 2.
		assert!(
			tokio::time::timeout(Duration::from_millis(50), consumer.read())
				.await
				.is_err(),
			"the gap at sequence 1 is within budget and must be waited for"
		);

		// Group 1's stream opens moments later, well within the 500ms budget.
		write_group(&mut track, 1, &[ts(100_000)]);
		track.finish().unwrap();

		let frames = read_all(&mut consumer).await.unwrap();
		let micros: Vec<u128> = frames.iter().map(|f| f.timestamp.as_micros()).collect();
		assert_eq!(micros, vec![100_000, 200_000], "the late group is delivered in order");
	}

	/// An explicit start floor pins where delivery begins: when a later group's stream
	/// wins the arrival race, the consumer waits for the requested head under the age
	/// budget instead of dropping it on arrival (#3258).
	#[tokio::test]
	async fn startup_keeps_late_arriving_head_group_within_max_delay() {
		tokio::time::pause();
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track = track.subscribe(
			moq_net::track::Subscription::default()
				.with_range(0..)
				.with_max_delay(Duration::from_millis(500)),
		);
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Data));

		// Group 1's stream wins the race, and the consumer polls before group 0 lands.
		write_group(&mut track, 1, &[ts(100_000)]);
		assert!(
			tokio::time::timeout(Duration::from_millis(50), consumer.read())
				.await
				.is_err(),
			"the requested head is within budget and must be waited for"
		);

		// Group 0 arrives moments later, well within the 500ms budget.
		write_group(&mut track, 0, &[ts(0)]);
		track.finish().unwrap();

		let frames = read_all(&mut consumer).await.unwrap();
		let micros: Vec<u128> = frames.iter().map(|f| f.timestamp.as_micros()).collect();
		assert_eq!(micros, vec![0, 100_000], "the head group is delivered first");
	}

	/// A group whose frames lost the race to a newer group's stream is still read once
	/// they land: the cursor starts at group 0 and waits under the budget (#3258).
	#[tokio::test]
	async fn startup_keeps_slow_earlier_stream_within_max_delay() {
		tokio::time::pause();
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track =
			track.subscribe(moq_net::track::Subscription::default().with_max_delay(Duration::from_millis(500)));
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Data));

		// Group 0's stream opens first but carries no frames yet; group 1's frame wins.
		let mut group0 = track.create_group(moq_net::group::Info { sequence: 0 }).unwrap();
		write_group(&mut track, 1, &[ts(100_000)]);

		// Startup latches onto the lowest arrived sequence and waits under the budget.
		assert!(
			tokio::time::timeout(Duration::from_millis(50), consumer.read())
				.await
				.is_err(),
			"group 0's frames are within budget and must be waited for"
		);

		Container::Legacy(crate::container::Kind::Data)
			.write(
				&mut group0,
				&[Frame {
					timestamp: ts(0),
					payload: Bytes::from_static(&[0xDE, 0xAD]),
					keyframe: false,
					duration: None,
				}],
			)
			.unwrap();
		group0.finish().unwrap();
		track.finish().unwrap();

		let frames = read_all(&mut consumer).await.unwrap();
		let micros: Vec<u128> = frames.iter().map(|f| f.timestamp.as_micros()).collect();
		assert_eq!(micros, vec![0, 100_000], "the slow stream's frames are delivered");
	}

	// ---- Decode errors ----

	/// A container that decodes each frame's payload as an 8-byte LE microsecond
	/// timestamp, but treats a `FAIL` payload as a malformed frame. Lets a test put a
	/// decodable frame first (so the consumer reads the group) and a decode failure after.
	struct FailingDecode;

	impl ContainerTrait for FailingDecode {
		type Error = crate::Error;

		fn write(&self, group: &mut moq_net::group::Producer, frames: &[Frame]) -> Result<(), Self::Error> {
			for frame in frames {
				group.write_frame(moq_net::Timestamp::ZERO, frame.payload.clone())?;
			}
			Ok(())
		}

		fn decode(&self, frame: moq_net::frame::Frame) -> Result<Vec<Frame>, Self::Error> {
			use bytes::Buf;

			let mut data = frame.payload;
			if data.as_ref() == b"FAIL" {
				return Err(crate::Error::UnknownFormat("malformed payload".into()));
			}
			Ok(vec![Frame {
				timestamp: ts(data.get_u64_le()),
				payload: Bytes::new(),
				keyframe: false,
				duration: None,
			}])
		}
	}

	/// A decode error on a cleanly-finished group must propagate to the caller, not be
	/// mistaken for a relay eviction and silently skipped. Eviction-skip only fires when
	/// the group's stream was actually aborted.
	#[tokio::test]
	async fn decode_error_propagates() {
		tokio::time::pause();
		let track = track_producer("test", timed());
		let consumer_track = track.subscribe(None);
		let mut consumer = Consumer::new(consumer_track, FailingDecode);

		// A decodable frame first (so the consumer reads the group), then a malformed one.
		let mut group = track.create_group(moq_net::group::Info { sequence: 0 }).unwrap();
		group
			.write_frame(moq_net::Timestamp::ZERO, Bytes::from(0u64.to_le_bytes().to_vec()))
			.unwrap();
		group
			.write_frame(moq_net::Timestamp::ZERO, Bytes::from_static(b"FAIL"))
			.unwrap();
		group.finish().unwrap();
		track.finish().unwrap();

		// The first frame decodes; the malformed second frame must surface as an error.
		let first = consumer.read_frame().await;
		assert!(matches!(first, Ok(Some(_))), "first frame should decode, got {first:?}");

		let second = tokio::time::timeout(Duration::from_millis(200), consumer.read())
			.await
			.expect("consumer hung on a decode error");
		assert!(
			matches!(second, Err(crate::Error::UnknownFormat(_))),
			"decode error must propagate, got {second:?}"
		);
	}

	/// Write one FailingDecode frame: a microsecond timestamp, or `None` for a malformed payload.
	fn write_failing(group: &mut moq_net::group::Producer, micros: Option<u64>) {
		let payload = match micros {
			Some(micros) => Bytes::from(micros.to_le_bytes().to_vec()),
			None => Bytes::from_static(b"FAIL"),
		};
		group.write_frame(ts(micros.unwrap_or(0)), payload).unwrap();
	}

	/// Read until something other than a frame, failing if the reader stalls.
	async fn read_past_frames(consumer: &mut Consumer<FailingDecode>) -> Result<Option<Read>, crate::Error> {
		loop {
			let read = tokio::time::timeout(Duration::from_millis(200), consumer.read())
				.await
				.expect("consumer hung");
			match read {
				Ok(Some(Read::Frame { .. })) => continue,
				other => return other,
			}
		}
	}

	/// A malformed object found by the startup lookahead, at the head of the first group, ends the
	/// reader rather than being consumed and dropped.
	#[tokio::test]
	async fn a_decode_error_at_the_head_of_a_group_ends_the_reader() {
		tokio::time::pause();
		let track = track_producer("test", timed());
		let subscription = moq_net::track::Subscription::default().with_max_delay(Duration::from_secs(10));
		let mut consumer = Consumer::new(track.subscribe(subscription), FailingDecode);

		let mut group = track.create_group(moq_net::group::Info { sequence: 0 }).unwrap();
		write_failing(&mut group, None);
		write_failing(&mut group, Some(0));
		group.finish().unwrap();
		track.finish().unwrap();

		let read = read_past_frames(&mut consumer).await;
		assert!(matches!(read, Err(crate::Error::UnknownFormat(_))), "{read:?}");
		// The reader stays ended rather than resuming past the malformed object.
		let read = consumer.read().await;
		assert!(matches!(read, Ok(None)), "{read:?}");
	}

	/// The exporter path reads events directly, and it stays ended after an error too.
	#[tokio::test]
	async fn a_decode_error_ends_the_event_reader() {
		tokio::time::pause();
		let track = track_producer("test", timed());
		let mut consumer = Consumer::new(track.subscribe(None), FailingDecode);

		let mut group = track.create_group(moq_net::group::Info { sequence: 0 }).unwrap();
		write_failing(&mut group, None);
		write_failing(&mut group, Some(0));
		group.finish().unwrap();
		track.finish().unwrap();

		let event = kio::wait(|waiter| consumer.poll_event(waiter)).await;
		assert!(matches!(event, Err(crate::Error::UnknownFormat(_))));
		let event = kio::wait(|waiter| consumer.poll_event(waiter)).await;
		assert!(matches!(event, Ok(None)), "the event reader resumed after an error");
	}

	/// An error releases the subscription and the groups it holds, so a reader kept after it pins
	/// neither the publisher's demand nor its cache.
	#[tokio::test]
	async fn an_error_releases_the_subscription_and_its_groups() {
		tokio::time::pause();
		let track = track_producer("test", timed());
		let subscription = moq_net::track::Subscription::default().with_max_delay(Duration::from_secs(10));
		let mut consumer = Consumer::new(track.subscribe(subscription), FailingDecode);

		let mut malformed = track.create_group(moq_net::group::Info { sequence: 0 }).unwrap();
		write_failing(&mut malformed, None);
		malformed.finish().unwrap();
		let mut later = track.create_group(moq_net::group::Info { sequence: 1 }).unwrap();
		write_failing(&mut later, Some(10_000));

		let read = read_past_frames(&mut consumer).await;
		assert!(matches!(read, Err(crate::Error::UnknownFormat(_))), "{read:?}");
		assert!(track.demand().wanted().is_none(), "the ended reader still subscribes");
		assert!(!malformed.demand().wanted(), "the ended reader still holds its group");
		assert!(!later.demand().wanted(), "the ended reader still holds a later group");
	}

	/// A malformed object mid-group, found while looking ahead past a blocked group, ends the
	/// reader at its place rather than reading as a shorter group.
	#[tokio::test]
	async fn a_decode_error_mid_group_found_by_lookahead_ends_the_reader() {
		tokio::time::pause();
		let track = track_producer("test", timed());
		let subscription = moq_net::track::Subscription::default().with_max_delay(Duration::from_secs(10));
		let mut consumer = Consumer::new(track.subscribe(subscription), FailingDecode);

		// The current group stays open, so the reader looks ahead into the next one.
		let mut slow = track.create_group(moq_net::group::Info { sequence: 0 }).unwrap();
		write_failing(&mut slow, Some(0));
		let mut group = track.create_group(moq_net::group::Info { sequence: 1 }).unwrap();
		write_failing(&mut group, Some(10_000));
		write_failing(&mut group, None);
		write_failing(&mut group, Some(20_000));
		group.finish().unwrap();

		let first = consumer.read_frame().await.unwrap().unwrap();
		assert_eq!(first.timestamp, ts(0));
		// Lookahead finds the error while the reader waits on the open group, which comes first.
		assert!(
			tokio::time::timeout(Duration::from_millis(50), consumer.read())
				.await
				.is_err()
		);

		slow.finish().unwrap();
		let frame = consumer.read_frame().await.unwrap().unwrap();
		assert_eq!(frame.timestamp, ts(10_000));
		let read = tokio::time::timeout(Duration::from_millis(50), consumer.read()).await;
		assert!(matches!(read, Ok(Err(crate::Error::UnknownFormat(_)))), "{read:?}");
		// The reader stays ended rather than resuming with the group's later frames.
		let read = tokio::time::timeout(Duration::from_millis(50), consumer.read()).await;
		assert!(matches!(read, Ok(Ok(None))), "{read:?}");
	}

	/// A malformed head of a later group waits for the current group's frames: errors come in
	/// delivery order, however early the lookahead finds them.
	#[tokio::test]
	async fn a_decode_error_in_a_later_group_waits_for_current_frames() {
		tokio::time::pause();
		let track = track_producer("test", timed());
		let subscription = moq_net::track::Subscription::default().with_max_delay(Duration::from_secs(10));
		let mut consumer = Consumer::new(track.subscribe(subscription), FailingDecode);

		let mut current = track.create_group(moq_net::group::Info { sequence: 0 }).unwrap();
		write_failing(&mut current, Some(0));
		write_failing(&mut current, Some(10_000));
		write_failing(&mut current, Some(20_000));
		let first = consumer.read_frame().await.unwrap().unwrap();
		assert_eq!(first.timestamp, ts(0));

		let mut later = track.create_group(moq_net::group::Info { sequence: 1 }).unwrap();
		write_failing(&mut later, None);
		later.finish().unwrap();
		for expected in [10_000, 20_000] {
			let frame = consumer.read_frame().await.unwrap().unwrap();
			assert_eq!(frame.timestamp, ts(expected));
		}

		current.finish().unwrap();
		let read = read_past_frames(&mut consumer).await;
		assert!(matches!(read, Err(crate::Error::UnknownFormat(_))), "{read:?}");
	}

	/// A later group's rewind does not mask an earlier group's decode error: the first in delivery
	/// order is the one returned.
	#[tokio::test]
	async fn a_decode_error_comes_before_a_later_rewind() {
		tokio::time::pause();
		let track = track_producer("test", timed());
		let subscription = moq_net::track::Subscription::default().with_max_delay(Duration::from_secs(10));
		let mut consumer = Consumer::new(track.subscribe(subscription), FailingDecode);

		let mut current = track.create_group(moq_net::group::Info { sequence: 0 }).unwrap();
		write_failing(&mut current, Some(100_000));
		let first = consumer.read_frame().await.unwrap().unwrap();
		assert_eq!(first.timestamp, ts(100_000));

		let mut malformed = track.create_group(moq_net::group::Info { sequence: 1 }).unwrap();
		write_failing(&mut malformed, Some(110_000));
		malformed.write_frame(ts(110_000), Bytes::from_static(b"FAIL")).unwrap();
		malformed.finish().unwrap();
		// The transport only checks a group's first frame, so the rewind is its second.
		let mut rewound = track.create_group(moq_net::group::Info { sequence: 2 }).unwrap();
		write_failing(&mut rewound, Some(150_000));
		write_failing(&mut rewound, Some(50_000));
		rewound.finish().unwrap();
		// The reader looks ahead into both while it waits on the open group.
		assert!(
			tokio::time::timeout(Duration::from_millis(50), consumer.read())
				.await
				.is_err()
		);

		current.finish().unwrap();
		track.finish().unwrap();
		let read = read_past_frames(&mut consumer).await;
		assert!(matches!(read, Err(crate::Error::UnknownFormat(_))), "{read:?}");
	}

	/// A live reader discards the groups below the newest, and a malformed one it already read
	/// while finding the start still ends the reader.
	#[tokio::test]
	async fn a_startup_discard_returns_a_malformed_group() {
		tokio::time::pause();
		let track = track_producer("test", timed());
		let subscription = moq_net::track::Subscription::default().with_max_delay(Duration::from_secs(10));
		let mut consumer = Consumer::new(track.subscribe(subscription), FailingDecode).live();

		let mut old = track.create_group(moq_net::group::Info { sequence: 0 }).unwrap();
		write_failing(&mut old, None);
		old.finish().unwrap();
		let mut newest = track.create_group(moq_net::group::Info { sequence: 1 }).unwrap();
		write_failing(&mut newest, Some(10_000));
		newest.finish().unwrap();
		track.finish().unwrap();

		let read = read_past_frames(&mut consumer).await;
		assert!(matches!(read, Err(crate::Error::UnknownFormat(_))), "{read:?}");
	}

	/// An aborted later group holding a fault is not reaped ahead of its place: its error waits
	/// for the current group's frames, as a live group's does.
	#[tokio::test]
	async fn an_aborted_later_group_keeps_its_fault_in_delivery_order() {
		tokio::time::pause();
		let track = track_producer("test", timed());
		let subscription = moq_net::track::Subscription::default().with_max_delay(Duration::from_secs(10));
		let mut consumer = Consumer::new(track.subscribe(subscription), FailingDecode);

		let mut current = track.create_group(moq_net::group::Info { sequence: 0 }).unwrap();
		write_failing(&mut current, Some(0));
		let first = consumer.read_frame().await.unwrap().unwrap();
		assert_eq!(first.timestamp, ts(0));

		let mut later = track.create_group(moq_net::group::Info { sequence: 1 }).unwrap();
		write_failing(&mut later, None);
		// The reader looks ahead into the later group while it waits on the open one.
		assert!(
			tokio::time::timeout(Duration::from_millis(50), consumer.read())
				.await
				.is_err()
		);
		later.abort(moq_net::Error::Cancel).unwrap();

		write_failing(&mut current, Some(10_000));
		let frame = consumer.read_frame().await.unwrap().unwrap();
		assert_eq!(frame.timestamp, ts(10_000));

		current.finish().unwrap();
		track.finish().unwrap();
		let read = read_past_frames(&mut consumer).await;
		assert!(matches!(read, Err(crate::Error::UnknownFormat(_))), "{read:?}");
	}

	/// A malformed object read from the current group ends the reader even when the group's
	/// unread tail was reset afterwards: the reset is loss after it, not a reason to drop it.
	#[tokio::test]
	async fn a_decode_error_before_a_reset_tail_ends_the_reader() {
		tokio::time::pause();
		let track = track_producer("test", timed());
		let subscription = moq_net::track::Subscription::default().with_max_delay(Duration::from_secs(10));
		let mut consumer = Consumer::new(track.subscribe(subscription), FailingDecode);

		let mut group = track.create_group(moq_net::group::Info { sequence: 0 }).unwrap();
		write_failing(&mut group, Some(0));
		write_failing(&mut group, None);
		let first = consumer.read_frame().await.unwrap().unwrap();
		assert_eq!(first.timestamp, ts(0));

		group.abort(moq_net::Error::Cancel).unwrap();
		track.finish().unwrap();
		let read = tokio::time::timeout(Duration::from_millis(50), consumer.read()).await;
		assert!(matches!(read, Ok(Err(crate::Error::UnknownFormat(_)))), "{read:?}");
	}

	/// The lookahead keeps a malformed object it already holds even when the group is evicted
	/// before it gets there: the eviction is loss behind that object, not a reason to drop it.
	#[test]
	fn the_lookahead_keeps_a_decode_error_before_an_evicted_tail() {
		let waiter = kio::Waiter::noop();
		let track = track_producer("test", timed());
		let mut group = track.create_group(moq_net::group::Info { sequence: 0 }).unwrap();
		write_failing(&mut group, Some(0));
		write_failing(&mut group, None);
		write_failing(&mut group, Some(10_000));
		group.finish().unwrap();

		// Finding the start buffers one frame; the read behind it already holds the rest.
		let mut buffer = GroupBuffer::<FailingDecode>::new(group.consume(), Memory::new(DEFAULT_MAX_BUFFER));
		assert_eq!(
			buffer.poll_min_timestamp(&waiter, &FailingDecode, false),
			Poll::Ready(Some(ts(0)))
		);
		group.abort(moq_net::Error::Old).unwrap();

		let _ = buffer.look_all(&waiter, &FailingDecode);
		assert!(
			matches!(buffer.fault, Some(crate::Error::UnknownFormat(_))),
			"{:?}",
			buffer.fault
		);
	}

	// ---- Frame Decoding ----

	#[tokio::test]
	async fn frame_payload_preserved() {
		let track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track =
			track.subscribe(moq_net::track::Subscription::default().with_max_delay(Duration::from_millis(500)));
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Data));

		let payload_bytes = vec![0x01, 0x02, 0x03, 0x04, 0x05];
		let mut group = track.create_group(moq_net::group::Info { sequence: 0 }).unwrap();
		Container::Legacy(crate::container::Kind::Data)
			.write(
				&mut group,
				&[Frame {
					timestamp: ts(0),
					payload: Bytes::from(payload_bytes.clone()),

					keyframe: false,
					duration: None,
				}],
			)
			.unwrap();
		group.finish().unwrap();
		track.finish().unwrap();

		let frames = read_all(&mut consumer).await.unwrap();
		assert_eq!(frames.len(), 1);

		use bytes::Buf;
		let mut received = Vec::new();
		let mut payload = frames[0].payload.clone();
		while payload.has_remaining() {
			received.push(payload.get_u8());
		}
		assert_eq!(received, payload_bytes);
	}

	// ---- Regression ----

	#[tokio::test]
	async fn no_infinite_loop_with_buffered_frames() {
		tokio::time::pause();
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track =
			track.subscribe(moq_net::track::Subscription::default().with_max_delay(Duration::from_secs(10)));
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Data));

		let mut group0 = track.create_group(moq_net::group::Info { sequence: 0 }).unwrap();
		Container::Legacy(crate::container::Kind::Data)
			.write(
				&mut group0,
				&[Frame {
					timestamp: ts(0),
					payload: Bytes::from_static(&[0xDE, 0xAD]),
					keyframe: false,
					duration: None,
				}],
			)
			.unwrap();

		write_group(&mut track, 1, &[ts(100_000)]);

		let finisher = tokio::spawn(async move {
			tokio::time::sleep(Duration::from_millis(20)).await;
			// Write group 2: recv_group fires, drops current buffer_until for group 1
			write_group(&mut track, 2, &[ts(200_000)]);
			tokio::time::sleep(Duration::from_millis(20)).await;
			group0.finish().unwrap();
			track.finish().unwrap();
		});

		let frames = tokio::time::timeout(Duration::from_secs(2), async {
			let mut frames = Vec::new();
			while let Some(frame) = consumer.read_frame().await.unwrap() {
				frames.push(frame);
			}
			frames
		})
		.await
		.expect("consumer hung — possible infinite loop regression");

		assert_eq!(frames.len(), 3);
		finisher.await.expect("finisher task panicked");
	}

	// ---- Edge Cases ----

	#[tokio::test]
	async fn large_timestamps() {
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track =
			track.subscribe(moq_net::track::Subscription::default().with_max_delay(Duration::from_secs(3700)));
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Data));

		let one_hour = 3_600_000_000u64;
		write_group(&mut track, 0, &[ts(one_hour)]);
		track.finish().unwrap();

		let frames = read_all(&mut consumer).await.unwrap();
		assert_eq!(frames.len(), 1);
		assert_eq!(frames[0].timestamp, ts(one_hour));
		assert_eq!(frames[0].timestamp.as_micros(), one_hour as u128);
	}

	#[tokio::test]
	async fn set_max_delay_changes_behavior() {
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track =
			track.subscribe(moq_net::track::Subscription::default().with_max_delay(Duration::from_secs(10)));
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Data));

		write_group(&mut track, 0, &[ts(0)]);
		track.finish().unwrap();

		let frame = consumer.read_frame().await.unwrap().unwrap();
		assert_eq!(frame.timestamp, ts(0));

		consumer.set_max_delay(Duration::from_millis(100));

		assert!(consumer.read().await.unwrap().is_none());
	}

	#[tokio::test]
	async fn max_timestamp_tracks_through_bframes() {
		tokio::time::pause();
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track =
			track.subscribe(moq_net::track::Subscription::default().with_max_delay(Duration::from_millis(110)));
		// max delay must exceed (group1_max - group0_min) = 100ms - 0ms = 100ms
		// to avoid the age skip and test B-frame timestamp tracking.
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Data));

		let mut group0 = track.create_group(moq_net::group::Info { sequence: 0 }).unwrap();
		for &timestamp in &[ts(0), ts(66_000), ts(33_000)] {
			Container::Legacy(crate::container::Kind::Data)
				.write(
					&mut group0,
					&[Frame {
						timestamp,
						payload: Bytes::from_static(&[0xDE, 0xAD]),
						keyframe: false,
						duration: None,
					}],
				)
				.unwrap();
		}

		write_group(&mut track, 1, &[ts(100_000)]);
		track.finish().unwrap();

		let finisher = tokio::spawn(async move {
			tokio::time::sleep(Duration::from_millis(50)).await;
			group0.finish().unwrap();
		});

		let frames = tokio::time::timeout(Duration::from_secs(2), async {
			let mut frames = Vec::new();
			while let Some(frame) = consumer.read_frame().await.unwrap() {
				frames.push(frame);
			}
			frames
		})
		.await
		.expect("consumer hung — max_timestamp regression");

		assert_eq!(frames.len(), 4, "Expected all 4 frames, got {}", frames.len());
		assert_eq!(frames[0].timestamp, ts(0));
		assert_eq!(frames[1].timestamp, ts(66_000));
		assert_eq!(frames[2].timestamp, ts(33_000));
		assert_eq!(frames[3].timestamp, ts(100_000));
		finisher.await.expect("finisher task panicked");
	}

	// ---- Startup Behavior ----

	#[tokio::test]
	async fn startup_selects_earliest_group() {
		tokio::time::pause();
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track =
			track.subscribe(moq_net::track::Subscription::default().with_max_delay(Duration::from_millis(100)));
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Data));

		write_group(&mut track, 3, &[ts(0)]);
		write_group(&mut track, 5, &[ts(150_000)]);

		let mut group7 = track.create_group(moq_net::group::Info { sequence: 7 }).unwrap();
		Container::Legacy(crate::container::Kind::Data)
			.write(
				&mut group7,
				&[Frame {
					timestamp: ts(300_000),
					payload: Bytes::from_static(&[0xDE, 0xAD]),
					keyframe: false,
					duration: None,
				}],
			)
			.unwrap();

		let finisher = tokio::spawn(async move {
			tokio::time::sleep(Duration::from_millis(50)).await;
			Container::Legacy(crate::container::Kind::Data)
				.write(
					&mut group7,
					&[Frame {
						timestamp: ts(400_000),
						payload: Bytes::from_static(&[0xBE, 0xEF]),
						keyframe: false,
						duration: None,
					}],
				)
				.unwrap();
			group7.finish().unwrap();
			track.finish().unwrap();
		});

		let _frames = tokio::time::timeout(Duration::from_secs(2), async {
			let mut frames = Vec::new();
			while let Some(frame) = consumer.read_frame().await.unwrap() {
				frames.push(frame);
			}
			frames
		})
		.await
		.expect("should not hang");

		finisher.await.unwrap();
	}

	/// An arrived group with no frames yet is settled only by its own FIN, abort, or the
	/// age budget, never by the track finishing: the track boundary ends new groups, not
	/// the frames still flowing on open ones. Here the budget expires it.
	#[tokio::test]
	async fn startup_skips_groups_without_data() {
		tokio::time::pause();
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track =
			track.subscribe(moq_net::track::Subscription::default().with_max_delay(Duration::from_millis(500)));
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Data));

		let _group5 = track.create_group(moq_net::group::Info { sequence: 5 }).unwrap();
		write_group(&mut track, 7, &[ts(210_000)]);

		// Group 5 is open and within budget: its frames may still arrive, so wait.
		assert!(
			tokio::time::timeout(Duration::from_millis(50), consumer.read())
				.await
				.is_err(),
			"an open frameless group within budget must be waited for"
		);

		// Group 9 pushes the newest content a full budget past what group 5 could still
		// present (bounded by group 7's start), expiring it.
		write_group(&mut track, 9, &[ts(800_000)]);
		track.finish().unwrap();

		let frames = read_all(&mut consumer).await.unwrap();
		let micros: Vec<u128> = frames.iter().map(|f| f.timestamp.as_micros()).collect();
		assert_eq!(micros, vec![210_000, 800_000], "the expired frameless group is skipped");
	}

	/// An aborted frameless group beyond a sequence gap is reaped rather than parked on
	/// forever: nothing else settles a group the cursor hasn't reached, so a finished
	/// track would otherwise never report its end.
	#[tokio::test]
	async fn aborted_frameless_group_after_a_gap_ends_the_track() {
		tokio::time::pause();
		let track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track =
			track.subscribe(moq_net::track::Subscription::default().with_max_delay(Duration::from_millis(500)));
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Data));

		// Sequence 0 never arrives; sequence 1's stream opens but never carries a frame.
		let group1 = track.create_group(moq_net::group::Info { sequence: 1 }).unwrap();
		track.finish().unwrap();

		// While group 1 is open its frames may still come, so the consumer waits.
		assert!(
			tokio::time::timeout(Duration::from_millis(50), consumer.read())
				.await
				.is_err(),
			"an open frameless group must be waited for"
		);

		// The abort (an eviction) settles it, and the finished track ends cleanly.
		group1.abort(moq_net::Error::Old).unwrap();
		let end = tokio::time::timeout(Duration::from_millis(200), consumer.read())
			.await
			.expect("an aborted frameless group must not park the consumer");
		assert!(end.unwrap().is_none(), "the track ends cleanly");
	}

	/// Track completion is not group completion: a group already open when the track
	/// finishes can still receive frames, so it must not be skipped as if it were a
	/// missing sequence. Its late frames are delivered when they land within budget.
	#[tokio::test]
	async fn finished_track_waits_for_an_open_head_group() {
		tokio::time::pause();
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track =
			track.subscribe(moq_net::track::Subscription::default().with_max_delay(Duration::from_millis(500)));
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Data));

		// Group 0's stream is open but its frames lose the race; the track boundary
		// arrives before them.
		let mut group0 = track.create_group(moq_net::group::Info { sequence: 0 }).unwrap();
		write_group(&mut track, 1, &[ts(100_000)]);
		track.finish().unwrap();

		assert!(
			tokio::time::timeout(Duration::from_millis(50), consumer.read())
				.await
				.is_err(),
			"the open head group is within budget and must be waited for"
		);

		// Its frames land moments later, well within the 500ms budget.
		Container::Legacy(crate::container::Kind::Data)
			.write(
				&mut group0,
				&[Frame {
					timestamp: ts(0),
					payload: Bytes::from_static(&[0xDE, 0xAD]),
					keyframe: false,
					duration: None,
				}],
			)
			.unwrap();
		group0.finish().unwrap();

		let frames = read_all(&mut consumer).await.unwrap();
		let micros: Vec<u128> = frames.iter().map(|f| f.timestamp.as_micros()).collect();
		assert_eq!(micros, vec![0, 100_000], "the open group's late frames are delivered");
	}

	/// A gap between two ranges is never read, so the reader walks over it at once rather
	/// than waiting a budget, or forever on an open track, for groups it never gets.
	#[tokio::test]
	async fn an_excluded_gap_between_ranges_does_not_stall() {
		tokio::time::pause();
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let subscription = moq_net::track::Subscription::default()
			.with_max_delay(Duration::from_secs(10))
			.with_ranges([
				moq_net::track::Range::groups(0..1),
				moq_net::track::Range::groups(10..11),
			]);
		let mut consumer = Consumer::new(
			track.subscribe(subscription),
			Container::Legacy(crate::container::Kind::Data),
		);
		for sequence in [0, 5, 10] {
			write_group(&mut track, sequence, &[ts(sequence * 100_000)]);
		}

		let mut micros = Vec::new();
		for _ in 0..2 {
			let frame = tokio::time::timeout(Duration::from_millis(200), consumer.read_frame())
				.await
				.expect("the open track still delivers the later range")
				.unwrap()
				.expect("a frame");
			micros.push(frame.timestamp.as_micros());
		}
		assert_eq!(micros, vec![0, 1_000_000]);
	}

	/// A relative start names its floor once resolved: delivery starts that many groups
	/// back from the latest, rather than at whichever group arrives first.
	#[tokio::test]
	async fn startup_at_a_relative_start() {
		tokio::time::pause();
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		for sequence in 5..=9 {
			write_group(&mut track, sequence, &[ts(sequence * 100_000)]);
		}
		let subscription = moq_net::track::Subscription::default()
			.with_max_delay(Duration::from_secs(10))
			.with_ranges([moq_net::track::Range {
				start: moq_net::track::Anchor::Relative(2),
				end: None,
			}]);
		let consumer_track = track.subscribe(subscription);
		assert_eq!(consumer_track.floor(), Some(7));
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Data));
		track.finish().unwrap();

		let frames = read_all(&mut consumer).await.unwrap();
		let micros: Vec<u128> = frames.iter().map(|f| f.timestamp.as_micros()).collect();
		assert_eq!(micros, vec![700_000, 800_000, 900_000]);
	}

	#[tokio::test]
	async fn startup_single_group_mid_stream() {
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track =
			track.subscribe(moq_net::track::Subscription::default().with_max_delay(Duration::from_millis(500)));
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Data));

		write_group(&mut track, 100, &[ts(3_000_000)]);
		track.finish().unwrap();

		let frames = read_all(&mut consumer).await.unwrap();
		assert_eq!(frames.len(), 1);
	}

	/// An unfloored mid-stream join adopts the publisher's served start instead of
	/// waiting for a gap below it to expire: on a quiet track that gap never would,
	/// since nothing newer arrives to age it out.
	#[tokio::test]
	async fn startup_mid_stream_live_track_starts_at_the_served_group() {
		tokio::time::pause();
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track =
			track.subscribe(moq_net::track::Subscription::default().with_max_delay(Duration::from_millis(500)));
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Data));

		// The track is far along and stays live (never finished); only the current
		// group is served, and nothing else arrives.
		write_group(&mut track, 100, &[ts(3_000_000)]);

		let frame = tokio::time::timeout(Duration::from_millis(200), consumer.read_frame())
			.await
			.expect("an unfloored join must not wait out a gap below the served start")
			.unwrap()
			.expect("track still live");
		assert_eq!(frame.timestamp, ts(3_000_000));
	}

	#[tokio::test]
	async fn multiple_sequential_latency_skips() {
		tokio::time::pause();
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track =
			track.subscribe(moq_net::track::Subscription::default().with_max_delay(Duration::from_millis(50)));
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Data));

		let mut group0 = track.create_group(moq_net::group::Info { sequence: 0 }).unwrap();
		Container::Legacy(crate::container::Kind::Data)
			.write(
				&mut group0,
				&[Frame {
					timestamp: ts(0),
					payload: Bytes::from_static(&[0xAA]),

					keyframe: false,
					duration: None,
				}],
			)
			.unwrap();

		write_group(&mut track, 1, &[ts(100_000)]);
		write_group(&mut track, 2, &[ts(200_000)]);
		write_group(&mut track, 3, &[ts(300_000)]);
		track.finish().unwrap();

		let finisher = tokio::spawn(async move {
			tokio::time::sleep(Duration::from_millis(20)).await;
			group0.finish().unwrap();
		});

		let frames = read_all(&mut consumer).await.unwrap();
		assert!(!frames.is_empty());
		finisher.await.unwrap();
	}

	#[tokio::test]
	async fn latency_skip_boundary_exact() {
		tokio::time::pause();
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track =
			track.subscribe(moq_net::track::Subscription::default().with_max_delay(Duration::from_millis(100)));
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Data));

		let mut group0 = track.create_group(moq_net::group::Info { sequence: 0 }).unwrap();
		Container::Legacy(crate::container::Kind::Data)
			.write(
				&mut group0,
				&[Frame {
					timestamp: ts(0),
					payload: Bytes::from_static(&[0xAA]),

					keyframe: false,
					duration: None,
				}],
			)
			.unwrap();

		write_group(&mut track, 1, &[ts(100_000)]);
		track.finish().unwrap();

		let finisher = tokio::spawn(async move {
			tokio::time::sleep(Duration::from_millis(20)).await;
			group0.finish().unwrap();
		});

		let frames = read_all(&mut consumer).await.unwrap();
		assert!(!frames.is_empty());
		finisher.await.unwrap();
	}

	/// Regression: a single stalled group with one newer group should trigger
	/// a delay skip when the timestamp difference exceeds the max delay.
	/// Previously, the span was computed across newer groups only (zero for one
	/// group), so the skip never fired.
	#[tokio::test]
	async fn single_newer_group_triggers_skip() {
		tokio::time::pause();
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track = track.subscribe(None);
		let mut consumer = container_max_delay_only(consumer_track, Duration::from_millis(100));

		// Group 0: stalled at ts=0, NOT finished
		let mut group0 = track.create_group(moq_net::group::Info { sequence: 0 }).unwrap();
		Container::Legacy(crate::container::Kind::Data)
			.write(
				&mut group0,
				&[Frame {
					timestamp: ts(0),
					payload: Bytes::from_static(&[0xDE, 0xAD]),
					keyframe: false,
					duration: None,
				}],
			)
			.unwrap();

		// Group 1: finished, 200ms ahead (well beyond the 100ms max delay)
		write_group(&mut track, 1, &[ts(200_000)]);
		track.finish().unwrap();

		let finisher = tokio::spawn(async move {
			tokio::time::sleep(Duration::from_millis(50)).await;
			group0.finish().unwrap();
		});

		let frames = read_all(&mut consumer).await.unwrap();
		assert_eq!(frames.len(), 2, "Expected group 0 frame + group 1 frame");
		finisher.await.unwrap();
	}

	/// Regression: when the current group is fully consumed and the next sequence
	/// is missing (gap), the consumer should skip to the next available group
	/// once the track is fully received, rather than hanging forever.
	#[tokio::test]
	async fn single_missing_sequence_near_eof_skips() {
		tokio::time::pause();
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track = track.subscribe(None);
		let mut consumer = container_max_delay_only(consumer_track, Duration::from_millis(100));

		// Group 0: finished normally
		write_group(&mut track, 0, &[ts(0), ts(20_000)]);
		// Group 2: finished (group 1 is missing — sequence gap)
		write_group(&mut track, 2, &[ts(200_000)]);
		track.finish().unwrap();

		let frames = read_all(&mut consumer).await.unwrap();
		assert_eq!(frames.len(), 3, "Expected group 0 (2 frames) + group 2 (1 frame)");
	}

	#[tokio::test]
	async fn group_error_skips_to_next() {
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track =
			track.subscribe(moq_net::track::Subscription::default().with_max_delay(Duration::from_millis(500)));
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Data));

		let group0 = track.create_group(moq_net::group::Info { sequence: 0 }).unwrap();
		group0.abort(moq_net::Error::Cancel).unwrap();

		write_group(&mut track, 1, &[ts(30_000)]);
		track.finish().unwrap();

		let frames = read_all(&mut consumer).await.unwrap();
		assert_eq!(frames.len(), 1);
	}

	/// A finished group aborted afterwards (aged out of the track's max age window)
	/// keeps serving the frames it still holds: an abort only matters where a frame is
	/// missing, so a reader that stopped short of the end drains the rest and moves on
	/// to the next buffered group instead of ending the track.
	#[tokio::test]
	async fn finished_group_aborted_mid_read_drains_then_continues() {
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track =
			track.subscribe(moq_net::track::Subscription::default().with_max_delay(Duration::from_millis(500)));
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Data));

		let mut group0 = track.create_group(moq_net::group::Info { sequence: 0 }).unwrap();
		for timestamp in [ts(0), ts(10_000)] {
			let frame = Frame {
				timestamp,
				payload: Bytes::from_static(&[0xDE, 0xAD]),
				keyframe: false,
				duration: None,
			};
			Container::Legacy(crate::container::Kind::Data)
				.write(&mut group0, &[frame])
				.unwrap();
		}
		group0.finish().unwrap();
		write_group(&mut track, 1, &[ts(30_000)]);
		track.finish().unwrap();

		// Take the first frame only, leaving the second unread.
		let first = consumer.read_frame().await.unwrap().unwrap();
		assert_eq!(first.timestamp, ts(0));

		// Expiry aborts the finished group while the reader is still inside it.
		group0.abort(moq_net::Error::Old).unwrap();

		let rest = read_all(&mut consumer).await.unwrap();
		assert_eq!(rest.len(), 2, "expected the held frame then group 1, got {rest:?}");
		assert_eq!(rest[0].timestamp, ts(10_000));
		assert_eq!(rest[1].timestamp, ts(30_000));
	}

	/// A live group its upstream gave up as too large is aborted. A reader whose current
	/// group died that way skips forward; the gap is not a decode error.
	#[tokio::test]
	async fn oversized_group_skips_to_next() {
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track =
			track.subscribe(moq_net::track::Subscription::default().with_max_delay(Duration::from_millis(500)));
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Data));

		let mut group0 = track.create_group(moq_net::group::Info { sequence: 0 }).unwrap();
		let first = Frame {
			timestamp: ts(0),
			payload: Bytes::from(vec![0xDEu8; 1024]),
			keyframe: false,
			duration: None,
		};
		Container::Legacy(crate::container::Kind::Data)
			.write(&mut group0, &[first])
			.unwrap();
		group0.abort(moq_net::Error::GroupTooLarge).unwrap();

		write_group(&mut track, 1, &[ts(30_000)]);
		track.finish().unwrap();

		let frames = read_all(&mut consumer).await.unwrap();
		assert_eq!(frames.len(), 1, "expected group 1 only, got {} frames", frames.len());
		assert_eq!(frames[0].timestamp, ts(30_000));
	}

	#[tokio::test]
	async fn track_finishes_while_reading() {
		tokio::time::pause();
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track =
			track.subscribe(moq_net::track::Subscription::default().with_max_delay(Duration::from_millis(500)));
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Data));

		write_group(&mut track, 0, &[ts(0)]);

		let finisher = tokio::spawn(async move {
			tokio::time::sleep(Duration::from_millis(20)).await;
			write_group(&mut track, 1, &[ts(30_000)]);
			tokio::time::sleep(Duration::from_millis(20)).await;
			track.finish().unwrap();
		});

		let frames = tokio::time::timeout(Duration::from_secs(2), async {
			let mut frames = Vec::new();
			while let Some(frame) = consumer.read_frame().await.unwrap() {
				frames.push(frame);
			}
			frames
		})
		.await
		.expect("consumer should not hang");

		assert_eq!(frames.len(), 2);
		finisher.await.unwrap();
	}

	#[tokio::test]
	async fn empty_group_advances() {
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track =
			track.subscribe(moq_net::track::Subscription::default().with_max_delay(Duration::from_millis(500)));
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Data));

		let mut group0 = track.create_group(moq_net::group::Info { sequence: 0 }).unwrap();
		group0.finish().unwrap();

		write_group(&mut track, 1, &[ts(30_000)]);
		track.finish().unwrap();

		let frames = read_all(&mut consumer).await.unwrap();
		assert_eq!(frames.len(), 1);
	}

	// ---- VideoConfig Container ----

	#[tokio::test]
	async fn video_container_legacy() {
		tokio::time::pause();

		let track = track_producer("video", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track =
			track.subscribe(moq_net::track::Subscription::default().with_max_delay(Duration::from_millis(500)));
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Data));

		// Write frames using Container::Legacy(crate::container::Kind::Data) encoding
		let mut group = track.create_group(moq_net::group::Info { sequence: 0 }).unwrap();
		for i in 0..3u64 {
			let frame = Frame {
				timestamp: ts(i * 33_333),
				payload: Bytes::from_static(&[0xDE, 0xAD]),
				keyframe: false,
				duration: None,
			};
			Container::Legacy(crate::container::Kind::Data)
				.write(&mut group, &[frame])
				.unwrap();
		}
		group.finish().unwrap();
		track.finish().unwrap();

		let mut frames = Vec::new();
		while let Some(frame) = consumer.read_frame().await.unwrap() {
			frames.push(frame);
		}

		assert_eq!(frames.len(), 3);
		assert_eq!(frames[0].timestamp, ts(0));
		assert!(frames[0].keyframe);
		assert_eq!(frames[1].timestamp, ts(33_333));
		assert!(!frames[1].keyframe);
		assert_eq!(frames[2].timestamp, ts(66_666));
		assert!(!frames[2].keyframe);
	}

	// ---- Duration Skipping ----

	/// A stalled group whose frame covers up to the next group's start is skipped
	/// immediately, even with a max delay budget far larger than the gap. Without
	/// duration support the consumer would block on the unfinished group forever.
	#[tokio::test]
	async fn duration_skip_advances_to_next_group() {
		tokio::time::pause();
		// DurationWire is a test-only container that doesn't stamp moq_net frame
		// timestamps; leave the track untimed so model-layer validation matches.
		let track = track_producer("test", timed());
		let consumer_track =
			track.subscribe(moq_net::track::Subscription::default().with_max_delay(Duration::from_secs(10)));
		// The max delay dwarfs the gap, so only duration coverage can trigger the skip.
		let mut consumer = Consumer::new(consumer_track, DurationWire);

		// Group 0: one frame at ts=0 lasting 33ms, never finished.
		let mut group0 = track.create_group(moq_net::group::Info { sequence: 0 }).unwrap();
		write_duration_frame(&mut group0, ts(0), ts(33_000));

		// Group 1: finished, starts exactly where group 0's frame ends.
		let mut group1 = track.create_group(moq_net::group::Info { sequence: 1 }).unwrap();
		write_duration_frame(&mut group1, ts(33_000), ts(33_000));
		group1.finish().unwrap();

		track.finish().unwrap();

		let frames = tokio::time::timeout(Duration::from_secs(2), async { continuous_frames(&mut consumer).await })
			.await
			.expect("consumer hung — duration skip regression");

		assert_eq!(frames.len(), 2);
		assert_eq!(frames[0].timestamp, ts(0));
		assert_eq!(frames[1].timestamp, ts(33_000));

		// group0 is intentionally never finished.
		drop(group0);
	}

	#[tokio::test]
	async fn a_nonsequential_contiguous_jump_has_no_gap() {
		let track = track_producer("test", timed());
		let mut consumer = Consumer::new(
			track.subscribe(moq_net::track::Subscription::default().with_max_delay(Duration::from_secs(10))),
			DurationWire,
		);
		let mut group0 = track
			.create_group(moq_net::group::Info { sequence: 1_000_000 })
			.unwrap();
		write_duration_frame(&mut group0, ts(0), ts(33_000));
		group0.finish().unwrap();
		let mut group1 = track
			.create_group(moq_net::group::Info { sequence: 1_090_000 })
			.unwrap();
		write_duration_frame(&mut group1, ts(33_000), ts(33_000));
		group1.finish().unwrap();
		track.finish().unwrap();
		let frames = continuous_frames(&mut consumer).await;
		assert_eq!(frames.len(), 2);
	}

	/// When the current group's frame ends before the next group begins, there is
	/// still a gap to cover, so we don't skip early: a late-arriving frame on the
	/// slow group is delivered rather than dropped.
	#[tokio::test]
	async fn duration_below_gap_does_not_skip() {
		tokio::time::pause();
		// DurationWire is untimed at the moq_net frame layer.
		let track = track_producer("test", timed());
		let consumer_track =
			track.subscribe(moq_net::track::Subscription::default().with_max_delay(Duration::from_secs(10)));
		let mut consumer = Consumer::new(consumer_track, DurationWire);

		// Group 0: frame at ts=0 lasting only 10ms, far short of group 1 at 33ms.
		let mut group0 = track.create_group(moq_net::group::Info { sequence: 0 }).unwrap();
		write_duration_frame(&mut group0, ts(0), ts(10_000));

		// Group 1: finished at 33ms.
		let mut group1 = track.create_group(moq_net::group::Info { sequence: 1 }).unwrap();
		write_duration_frame(&mut group1, ts(33_000), ts(33_000));
		group1.finish().unwrap();
		track.finish().unwrap();

		// A second frame lands on group 0 and finishes it after the consumer has
		// had a chance to (incorrectly) skip.
		let finisher = tokio::spawn(async move {
			tokio::time::sleep(Duration::from_millis(20)).await;
			write_duration_frame(&mut group0, ts(20_000), ts(10_000));
			group0.finish().unwrap();
		});

		let frames = tokio::time::timeout(Duration::from_secs(2), async {
			let mut frames = Vec::new();
			while let Some(frame) = consumer.read_frame().await.unwrap() {
				frames.push(frame);
			}
			frames
		})
		.await
		.expect("consumer hung");

		// The slow group's late frame survives because nothing covered the gap.
		assert_eq!(frames.len(), 3);
		assert_eq!(frames[0].timestamp, ts(0));
		assert_eq!(frames[1].timestamp, ts(20_000));
		assert_eq!(frames[2].timestamp, ts(33_000));
		finisher.await.unwrap();
	}
	#[tokio::test]
	async fn live_duration_marker_follows_an_immediately_delivered_frame() {
		let track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let mut consumer = Consumer::new(track.subscribe(None), Container::Legacy(crate::container::Kind::Video));
		let mut group = track.append_group().unwrap();
		Container::Legacy(crate::container::Kind::Video)
			.write(
				&mut group,
				&[Frame {
					timestamp: ts(0),
					payload: Bytes::from_static(b"video"),
					keyframe: true,
					duration: None,
				}],
			)
			.unwrap();
		let frame = tokio::time::timeout(Duration::from_secs(1), consumer.read_frame())
			.await
			.unwrap()
			.unwrap()
			.unwrap();
		assert_eq!(frame.timestamp, ts(0));
		write_marker(&mut group, ts(15_000));
		let event = kio::wait(|waiter| consumer.poll_event(waiter)).await.unwrap();
		assert!(matches!(event, Some(Event::FrameEnd { end, opens: false }) if end == ts(15_000)));
	}

	// ---- Max buffer ----

	/// Write one unfinished Legacy data frame into a new group, leaving it open.
	fn open_group(
		track: &mut moq_net::track::Producer,
		sequence: u64,
		timestamp: Timestamp,
	) -> moq_net::group::Producer {
		let mut group = track.create_group(moq_net::group::Info { sequence }).unwrap();
		Container::Legacy(crate::container::Kind::Data)
			.write(
				&mut group,
				&[Frame {
					timestamp,
					payload: Bytes::from_static(&[0xDE, 0xAD]),
					keyframe: false,
					duration: None,
				}],
			)
			.unwrap();
		group
	}

	/// Read every item, checking after each that the consumer holds about the max buffer at most:
	/// past it, one more wire frame and the groups the cursor still needs.
	async fn read_bounded(consumer: &mut Consumer<Container>, max_buffer: usize) -> Vec<Read> {
		let slack = 4 * std::mem::size_of::<GroupBuffer<Container>>() + 1024;
		let mut reads = Vec::new();
		loop {
			let read = tokio::time::timeout(Duration::from_secs(5), consumer.read())
				.await
				.expect("consumer hung")
				.unwrap();
			let used = consumer.buffered_bytes();
			assert!(
				used <= max_buffer + slack,
				"holding {used} bytes past a {max_buffer} byte max buffer"
			);
			match read {
				Some(read) => reads.push(read),
				None => return reads,
			}
		}
	}

	fn timestamps(reads: &[Read]) -> Vec<Timestamp> {
		reads
			.iter()
			.filter_map(|read| match read {
				Read::Frame { frame, .. } => Some(frame.timestamp),
				_ => None,
			})
			.collect()
	}

	/// Frames packed into one timestamp have no span, so the age budget never skips the stalled group
	/// they queue behind. Read ahead as the newest group, they fill the max buffer instead, which skips
	/// it, and the rest stays unread in the transport's cache until the reader gets to it.
	#[tokio::test]
	async fn frames_sharing_a_timestamp_stay_within_the_max_buffer() {
		tokio::time::pause();
		const MAX_BUFFER: usize = 4096;
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let mut consumer = container_max_delay_only(track.subscribe(None), Duration::from_secs(5));
		consumer.set_max_buffer(MAX_BUFFER);

		let _stalled = open_group(&mut track, 0, ts(0));
		write_group(&mut track, 1, &vec![ts(10_000); 2_000]);
		track.finish().unwrap();

		let reads = read_bounded(&mut consumer, MAX_BUFFER).await;
		let timestamps = timestamps(&reads);
		assert_eq!(timestamps.len(), 1 + 2_000);
		assert_eq!(timestamps.first(), Some(&ts(0)));
		assert_eq!(timestamps.last(), Some(&ts(10_000)));
		// The stalled group's tail is lost.
		assert!(matches!(reads[1], Read::Gap { .. }), "{:?}", reads[1]);
	}

	/// A publisher opening many groups fills the max buffer with their overhead and first frames, so
	/// the rest wait in the transport's cache: playback still reaches the live edge, and what arrives
	/// below the cursor once there is room is dropped as old.
	#[tokio::test]
	async fn many_groups_stay_within_the_max_buffer() {
		tokio::time::pause();
		const MAX_BUFFER: usize = 8192;
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let subscriber =
			track.subscribe(moq_net::track::Subscription::default().with_max_delay(Duration::from_millis(100)));
		let mut consumer = Consumer::new(subscriber, Container::Legacy(crate::container::Kind::Data));
		consumer.set_max_buffer(MAX_BUFFER);

		let _stalled = open_group(&mut track, 0, ts(0));
		for g in 1..=500u64 {
			write_group(&mut track, g, &[ts(g * 1_000)]);
		}
		track.finish().unwrap();

		let reads = read_bounded(&mut consumer, MAX_BUFFER).await;
		let timestamps = timestamps(&reads);
		assert_eq!(timestamps.last(), Some(&ts(500_000)));
		assert!(timestamps.len() < 501, "nothing was skipped: {}", timestamps.len());
	}

	/// A successor holding only endpoint markers stops buffering at the max buffer like any other
	/// lookahead, rather than reading past it in search of a media frame.
	#[test]
	fn an_endpoint_only_successor_stays_within_the_max_buffer() {
		const MAX_BUFFER: usize = 1024;
		let waiter = kio::Waiter::noop();
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let subscriber =
			track.subscribe(moq_net::track::Subscription::default().with_max_delay(Duration::from_secs(30)));
		let mut consumer = Consumer::new(subscriber, Container::Legacy(crate::container::Kind::Video));
		consumer.set_max_buffer(MAX_BUFFER);

		let _stalled = open_group(&mut track, 0, ts(0));
		let mut markers = track.create_group(moq_net::group::Info { sequence: 1 }).unwrap();
		for _ in 0..2_000 {
			write_marker(&mut markers, ts(10_000));
		}
		Container::Legacy(crate::container::Kind::Data)
			.write(
				&mut markers,
				&[Frame {
					timestamp: ts(20_000),
					payload: Bytes::from_static(b"video"),
					keyframe: false,
					duration: None,
				}],
			)
			.unwrap();
		markers.finish().unwrap();

		// The markers fill the max buffer behind the stalled group, which moves the cursor on to them,
		// and the media past them still plays.
		let slack = 2 * std::mem::size_of::<GroupBuffer<Container>>() + MARKER_COST;
		let mut frames = Vec::new();
		loop {
			let read = consumer.poll_read(&waiter);
			let used = consumer.buffered_bytes();
			assert!(
				used <= MAX_BUFFER + slack,
				"holding {used} bytes past a {MAX_BUFFER} byte max buffer"
			);
			match read {
				Poll::Ready(Ok(Some(Read::Frame { frame, .. }))) => frames.push(frame.timestamp),
				Poll::Ready(Ok(Some(_))) => continue,
				Poll::Pending => break,
				other => panic!("unexpected read: {other:?}"),
			}
		}
		assert_eq!(frames, vec![ts(0), ts(20_000)]);
	}

	/// A decoded payload is a view of its wire frame, which stays allocated while the view is held, so a
	/// view that leaves out most of its wire frame is copied out: what holds the frame after the reader,
	/// such as an export's jitter buffer, retains only its payload. Here each is a 1 byte payload behind
	/// a large LOC property block.
	#[test]
	fn a_small_view_of_a_large_wire_frame_is_copied_out() {
		const PADDING: usize = 60_000;

		// A QUIC varint, which is all LOC's framing needs.
		fn varint(buf: &mut Vec<u8>, value: usize) {
			match value {
				0..64 => buf.push(value as u8),
				64..16_384 => buf.extend_from_slice(&(value as u16 | 0x4000).to_be_bytes()),
				_ => buf.extend_from_slice(&(value as u32 | 0x8000_0000).to_be_bytes()),
			}
		}

		let waiter = kio::Waiter::noop();
		let track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let subscriber =
			track.subscribe(moq_net::track::Subscription::default().with_max_delay(Duration::from_secs(30)));
		let mut consumer = Consumer::new(subscriber, Container::Loc(crate::container::Kind::Data));

		let mut padded = track.create_group(moq_net::group::Info { sequence: 0 }).unwrap();
		for i in 0..8 {
			let timestamp = i * 1_000;
			// An unknown video-config property (0x0d) padding the frame, then the timestamp (0x10).
			let mut props = Vec::new();
			varint(&mut props, 0x0d);
			varint(&mut props, PADDING);
			props.resize(props.len() + PADDING, 0);
			varint(&mut props, 0x10 - 0x0d);
			varint(&mut props, timestamp);
			let mut wire = Vec::new();
			varint(&mut wire, props.len());
			wire.extend_from_slice(&props);
			wire.push(0xAD);
			padded.write_frame(ts(timestamp as u64), wire).unwrap();
		}
		padded.finish().unwrap();

		let mut reads = Vec::new();
		while let Poll::Ready(read) = consumer.poll_read(&waiter) {
			match read.unwrap() {
				Some(Read::Frame { frame, .. }) => {
					assert_eq!(frame.payload[..], [0xAD]);
					// The group still holds its wire frames, so a view of one would share it.
					assert!(frame.payload.is_unique(), "the payload still shares its wire frame");
					reads.push(frame.timestamp);
				}
				Some(Read::End { .. }) | None => break,
				other => panic!("unexpected read: {other:?}"),
			}
		}
		assert_eq!(reads, (0..8).map(|i| ts(i * 1_000)).collect::<Vec<_>>());
	}

	/// A stalled successor does not hide a readable group behind it once group overhead alone fills the
	/// max buffer: the cursor moves on group by group until one has something to read.
	#[test]
	fn a_stalled_successor_does_not_hide_a_readable_group() {
		let waiter = kio::Waiter::noop();
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let mut consumer = container_max_delay_only(track.subscribe(None), Duration::from_secs(5));
		consumer.set_max_buffer(2 * std::mem::size_of::<GroupBuffer<Container>>());

		let _stalled = open_group(&mut track, 0, ts(0));
		let _empty = track.create_group(moq_net::group::Info { sequence: 1 }).unwrap();
		write_group(&mut track, 2, &[ts(20_000)]);

		let mut reads = Vec::new();
		while let Poll::Ready(read) = consumer.poll_read(&waiter) {
			match read.unwrap() {
				Some(Read::Frame { frame, .. }) => reads.push(Some(frame.timestamp)),
				Some(Read::Gap { .. }) => reads.push(None),
				other => panic!("unexpected read: {other:?}"),
			}
		}
		assert_eq!(reads, vec![Some(ts(0)), None, Some(ts(20_000))]);
	}

	/// Groups 0 and 1 open behind a cursor that read group 0's frame, and group 2's large frame that
	/// fills the max buffer once read ahead. Returns groups 0 and 1.
	fn fill_behind_a_stalled_group(
		track: &mut moq_net::track::Producer,
		consumer: &mut Consumer<Container>,
		waiter: &kio::Waiter,
	) -> (moq_net::group::Producer, moq_net::group::Producer) {
		let stalled = open_group(track, 0, ts(0));
		assert!(matches!(
			consumer.poll_read(waiter),
			Poll::Ready(Ok(Some(Read::Frame { group: 0, .. })))
		));
		let between = track.create_group(moq_net::group::Info { sequence: 1 }).unwrap();
		let mut later = track.create_group(moq_net::group::Info { sequence: 2 }).unwrap();
		Container::Legacy(crate::container::Kind::Data)
			.write(
				&mut later,
				&[Frame {
					timestamp: ts(20_000),
					payload: Bytes::from(vec![0; 4096]),
					keyframe: false,
					duration: None,
				}],
			)
			.unwrap();
		later.finish().unwrap();
		(stalled, between)
	}

	// A max buffer that holds three groups' overhead with room to spare, short of group 2's large frame.
	fn pressured_data_consumer(track: &mut moq_net::track::Producer) -> Consumer<Container> {
		let mut consumer = container_max_delay_only(track.subscribe(None), Duration::from_secs(5));
		consumer.set_max_buffer(1024 + 3 * std::mem::size_of::<GroupBuffer<Container>>());
		consumer
	}

	/// A malformed frame landing in the group the cursor moves on to under pressure fails the reader
	/// rather than leaving it waiting on the group.
	#[test]
	fn a_malformed_frame_in_a_group_moved_to_under_pressure_fails_the_reader() {
		let waiter = kio::Waiter::noop();
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let mut consumer = pressured_data_consumer(&mut track);
		let (_stalled, mut between) = fill_behind_a_stalled_group(&mut track, &mut consumer, &waiter);

		between.write_frame(ts(10_000), Bytes::from_static(&[0x80])).unwrap();
		let read = loop {
			match consumer.poll_read(&waiter) {
				Poll::Ready(Ok(Some(Read::Gap { .. }))) => continue,
				other => break other,
			}
		};
		assert!(matches!(read, Poll::Ready(Err(_))), "{read:?}");
	}

	/// A data marker group that finished before the cursor moved on to it under pressure still reads
	/// its end before the group after it.
	#[test]
	fn a_finished_marker_group_moved_to_under_pressure_reads_its_end() {
		let waiter = kio::Waiter::noop();
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let mut consumer = pressured_data_consumer(&mut track);
		let (_stalled, mut between) = fill_behind_a_stalled_group(&mut track, &mut consumer, &waiter);

		between.write_frame(ts(10_000), Bytes::new()).unwrap();
		between.finish().unwrap();
		let mut reads = Vec::new();
		while let Poll::Ready(read) = consumer.poll_read(&waiter) {
			reads.push(match read.unwrap() {
				Some(Read::Frame { group, frame }) => format!("frame {group} {}", frame.timestamp.as_micros()),
				Some(Read::Gap { .. }) => "gap".to_string(),
				Some(Read::End { group, .. }) => format!("end {group}"),
				None => break,
			});
		}
		assert_eq!(reads, ["gap", "end 1", "frame 2 20000"]);
	}

	/// A hole's budget runs from when the group past it landed, not from when the reader pulled it:
	/// a full max buffer holds pulls back, so a hole that already expired is passed at once.
	#[tokio::test(start_paused = true)]
	async fn a_hole_behind_a_full_max_buffer_expires_from_its_successors_arrival() {
		let max_delay = Duration::from_millis(500);
		let track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let mut consumer = container_max_delay_only(track.subscribe(None), max_delay);
		consumer.set_max_buffer(4096);
		for sequence in 0..2u64 {
			let mut group = track.create_group(moq_net::group::Info { sequence }).unwrap();
			Container::Legacy(crate::container::Kind::Data)
				.write(
					&mut group,
					&[Frame {
						timestamp: ts(sequence * 10_000),
						payload: Bytes::from(vec![0; 4096]),
						keyframe: false,
						duration: None,
					}],
				)
				.unwrap();
			group.finish().unwrap();
		}
		let mut marker = track.create_group(moq_net::group::Info { sequence: 3 }).unwrap();
		marker.write_frame(ts(20_000), Bytes::new()).unwrap();
		marker.finish().unwrap();

		// The reader gets to the track only after the hole before group 3 has expired.
		tokio::time::advance(2 * max_delay).await;
		let waiter = kio::Waiter::noop();
		let mut reads = Vec::new();
		while let Poll::Ready(read) = consumer.poll_read(&waiter) {
			reads.push(match read.unwrap() {
				Some(Read::Frame { group, frame }) => format!("frame {group} {}", frame.timestamp.as_micros()),
				Some(Read::Gap { .. }) => "gap".to_string(),
				Some(Read::End { group, .. }) => format!("end {group}"),
				None => break,
			});
		}
		assert_eq!(reads, ["frame 0 0", "frame 1 10000", "gap", "end 3"]);
	}

	/// A joining reader drains the backlog in sequence order, not arrival order, so a group can land
	/// before the one below it is pulled: the hole before that lower group is late from the earlier one.
	#[tokio::test(start_paused = true)]
	async fn a_hole_in_a_drained_backlog_is_late_from_the_earliest_arrival_past_it() {
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let marker = |track: &mut moq_net::track::Producer, sequence: u64| {
			let mut group = track.create_group(moq_net::group::Info { sequence }).unwrap();
			group.write_frame(ts(sequence * 10_000), Bytes::new()).unwrap();
			group.finish().unwrap();
		};
		write_group(&mut track, 0, &[ts(0)]);
		marker(&mut track, 5);
		tokio::time::advance(Duration::from_millis(900)).await;
		marker(&mut track, 3);
		tokio::time::advance(Duration::from_millis(100)).await;

		let mut consumer = container_max_delay_only(track.subscribe(None), Duration::from_millis(500));
		let waiter = kio::Waiter::noop();
		let mut reads = Vec::new();
		while let Poll::Ready(read) = consumer.poll_read(&waiter) {
			reads.push(match read.unwrap() {
				Some(Read::Frame { group, .. }) => format!("frame {group}"),
				Some(Read::Gap { .. }) => "gap".to_string(),
				Some(Read::End { group, .. }) => format!("end {group}"),
				None => break,
			});
		}
		// Group 5 landed a second ago, so the holes before groups 3 and 5 both expired.
		assert_eq!(reads, ["frame 0", "gap", "end 3", "gap", "end 5"]);
	}

	/// No cap is too small to read through: the cursor's group and one successor are always pulled, so
	/// a stalled group can be moved past even when one group's overhead fills the max buffer.
	#[test]
	fn a_zero_max_buffer_still_moves_past_a_stalled_group() {
		let waiter = kio::Waiter::noop();
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let mut consumer = container_max_delay_only(track.subscribe(None), Duration::from_secs(5));
		consumer.set_max_buffer(0);

		let _stalled = open_group(&mut track, 0, ts(0));
		write_group(&mut track, 1, &[ts(10_000)]);

		let mut reads = Vec::new();
		while let Poll::Ready(read) = consumer.poll_read(&waiter) {
			match read.unwrap() {
				Some(Read::Frame { frame, .. }) => reads.push(Some(frame.timestamp)),
				Some(Read::Gap { .. }) => reads.push(None),
				other => panic!("unexpected read: {other:?}"),
			}
		}
		assert_eq!(reads, vec![Some(ts(0)), None, Some(ts(10_000))]);
	}

	/// A paused payload reader must not retain metadata for cache entries that were evicted.
	#[moq_clock::test]
	async fn receiving_under_backpressure_releases_aborted_group_handles() {
		let track = track_producer("backpressured", timed());
		let mut consumer = Consumer::new(
			track.subscribe(moq_net::track::Subscription::default()),
			Container::Legacy(crate::container::Kind::Data),
		);
		for sequence in 0..256 {
			let mut group = track.create_group(moq_net::group::Info { sequence }).unwrap();
			Container::Legacy(crate::container::Kind::Data)
				.write(
					&mut group,
					&[Frame {
						timestamp: ts(sequence * 20_000),
						payload: Bytes::from_static(b"payload"),
						keyframe: true,
						duration: None,
					}],
				)
				.unwrap();
			assert!(consumer.poll_received(&kio::Waiter::noop()).is_pending());
			group.abort(moq_net::Error::Evicted).unwrap();
		}
		let _ = consumer.poll_received(&kio::Waiter::noop());
		assert!(
			consumer.pending.len() <= 2,
			"doubling compaction bounds metadata by twice the cursor's one retained gap, retained {} aborted group handles",
			consumer.pending.len()
		);
	}

	/// A finished track may still have bytes in flight on a group already received.
	#[moq_clock::test]
	async fn received_input_waits_for_group_fin_without_consuming_payload() {
		let mut track = track_producer("finite", timed());
		let mut consumer = Consumer::new(
			track.subscribe(moq_net::track::Subscription::default().with_max_delay(Duration::from_millis(500))),
			Container::Legacy(crate::container::Kind::Data),
		);
		write_group(&mut track, 0, &[ts(0)]);
		let mut group = track.create_group(moq_net::group::Info { sequence: 1 }).unwrap();
		Container::Legacy(crate::container::Kind::Data)
			.write(
				&mut group,
				&[Frame {
					timestamp: ts(20_000),
					payload: Bytes::from_static(b"tail"),
					keyframe: true,
					duration: None,
				}],
			)
			.unwrap();
		track.finish().unwrap();
		assert!(consumer.poll_received(&kio::Waiter::noop()).is_pending());
		assert!(
			consumer.pending.iter().all(|group| group.empty),
			"receipt must not decode payload"
		);
		group.finish().unwrap();
		assert!(consumer.poll_received(&kio::Waiter::noop()).is_ready());
		let mut frames = Vec::new();
		while let Some(frame) = consumer.read_frame().await.unwrap() {
			frames.push(frame.timestamp);
		}
		assert_eq!(frames, [ts(0), ts(20_000)]);
	}
}
