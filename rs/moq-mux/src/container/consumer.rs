use std::collections::VecDeque;
use std::task::{Poll, ready};

use moq_net::Timestamp;

use super::{Container, Frame};

/// Decode a moq-lite track into a stream of media [`Frame`]s in latency-bounded
/// presentation order.
///
/// `Consumer` wraps a [`moq_net::track::Subscriber`] and a [`Container`]
/// format implementation, typically
/// [`catalog::hang::Container`](crate::catalog::hang::Container). Yields
/// decoded frames via [`read`](Self::read).
///
/// ## Ordering & latency skipping
///
/// Groups can arrive on the wire out of order. The consumer always reads frames *within*
/// a group in arrival order, but across groups it advances by sequence number, skipping
/// stalled or missing groups when the difference between the oldest pending timestamp
/// and the newest available timestamp exceeds the configured latency. With the default
/// latency of zero, the consumer skips aggressively. Any group that has a newer
/// alternative is dropped. With a non-zero latency, slow groups are tolerated up to that
/// budget before being skipped.
///
/// A stalled group is also skipped early, regardless of the latency budget, once it has
/// presented up to where the next group begins. CMAF frames carry a per-sample duration,
/// so a group whose most recent frame ends (timestamp + duration) at or past the next
/// group's first timestamp has nothing left worth waiting for. Containers without a
/// duration report zero, which disables this check and falls back to the latency budget.
///
/// Set the latency with [`with_latency`](Self::with_latency) (builder) or
/// [`set_latency`](Self::set_latency) (mid-stream).
///
/// ## Timeline rewinds
///
/// If a newer group's timestamps jump backwards past the live edge, the publisher is
/// reneging the buffered tail (e.g. a voice agent interrupted mid-utterance). The consumer
/// drops the reneged groups, resumes at the rewound timeline, and bumps
/// [`discontinuity`](Self::discontinuity) so downstream consumers can flush their own
/// buffers. This is always on.
pub struct Consumer<F: Container> {
	track: moq_net::track::Subscriber,

	format: F,

	// The current group that we want to read from
	current: u64,

	// Groups that we are monitoring, sorted by sequence ascending.
	pending: VecDeque<GroupBuffer>,

	// When true, we haven't returned a frame yet and need to select the first group.
	// We wait until we have at least one frame before finalizing `current`
	startup: bool,

	// The maximum buffer size before skipping a group.
	latency: std::time::Duration,

	// Timeline-rewind tracking: the live edge, the active boundary, and the discontinuity count.
	rewind: Rewind,
}

/// Live state for detecting timeline rewinds and classifying out-of-order groups.
///
/// A publisher reneges its buffered tail by rewinding timestamps while group sequence keeps
/// climbing (e.g. a voice agent interrupted mid-utterance). We track the live edge to spot the
/// jump, a [`Reset`] boundary to classify out-of-order groups across it, and a counter that
/// downstream consumers watch to flush their own queues.
#[derive(Default)]
struct Rewind {
	// The live edge of playback: the largest timestamp delivered so far and the group that
	// carried it. `None` until the first frame is delivered.
	live_edge: Option<(u64, Timestamp)>,

	// The active rewind boundary, if any. Out-of-order groups are classified against it so a
	// late new-epoch group is kept while a reneged old-epoch straggler is dropped.
	boundary: Option<Reset>,

	// Increments on every rewind. Downstream consumers compare it across reads and, when it
	// changes, drop media still queued in their decoder or render buffers.
	discontinuity: u64,
}

/// A recorded rewind boundary.
///
/// After a backwards timestamp jump, groups can still arrive out of order, so a single
/// sequence floor is not enough: a late new-epoch group can have a *lower* sequence than
/// the group that triggered detection. We keep just enough state to classify any group by
/// `(sequence, timestamp)`.
#[derive(Clone, Copy)]
struct Reset {
	// Highest-sequence old-epoch group seen at detection (it held the old live edge).
	// Sequences at or below this are old: drop.
	prev_max: u64,

	// The group whose backwards timestamp triggered detection. Sequences at or above this
	// are new: keep.
	group: u64,

	// That group's timestamp. Within the ambiguous span `(prev_max, group)` a group is a
	// new-epoch gap-filler if its timestamp is below this, else an old straggler whose
	// higher timestamp simply hadn't arrived yet.
	timestamp: Timestamp,
}

impl Reset {
	// Classify by sequence alone. `Some(true)` = old/drop, `Some(false)` = new/keep,
	// `None` = ambiguous (the caller must resolve it with the group's timestamp).
	fn by_sequence(&self, sequence: u64) -> Option<bool> {
		if sequence <= self.prev_max {
			Some(true)
		} else if sequence >= self.group {
			Some(false)
		} else {
			None
		}
	}

	// Whether a group belongs to the reneged old epoch and should be dropped. In the
	// ambiguous span, old stragglers sit at or above the reset timestamp; new gap-fillers
	// fall below it.
	fn is_stale(&self, sequence: u64, timestamp: Timestamp) -> bool {
		self.by_sequence(sequence).unwrap_or(timestamp >= self.timestamp)
	}
}

impl<F: Container> Consumer<F> {
	/// Create a Consumer wrapping the given moq-lite consumer, decoding `format`.
	///
	/// Skips aggressively by default; raise the tolerance with [`with_latency`](Self::with_latency).
	pub fn new(track: moq_net::track::Subscriber, format: F) -> Self {
		Self {
			track,
			format,
			current: 0,
			pending: VecDeque::new(),
			startup: true,
			latency: std::time::Duration::ZERO,
			rewind: Rewind::default(),
		}
	}

	/// Set the maximum latency tolerance.
	///
	/// Groups with timestamps older than the newest timestamp minus this value are skipped. Zero
	/// (the default) skips aggressively: any group with a newer alternative is dropped.
	pub fn with_latency(mut self, latency: std::time::Duration) -> Self {
		self.latency = latency;
		self
	}

	/// A counter that increments each time the consumer detects a timeline rewind and drops
	/// the reneged buffer.
	///
	/// When a newer group's timestamps jump backwards past the live edge, the publisher is
	/// reneging everything buffered after that point (e.g. a voice agent interrupted
	/// mid-utterance). Downstream consumers should compare this across reads and, when it
	/// changes, flush any media still queued in their decoder or render buffers. The frame
	/// returned by the read that bumps it is the first of the new timeline.
	pub fn discontinuity(&self) -> u64 {
		self.rewind.discontinuity
	}

	/// Read the next frame from the track.
	///
	/// This method handles timestamp decoding, group ordering, and latency management
	/// automatically. It will skip groups that are too far behind to maintain the
	/// configured latency target.
	///
	/// Returns `None` when the track has ended.
	pub async fn read(&mut self) -> Result<Option<Frame>, F::Error> {
		kio::wait(|waiter| self.poll_read(waiter)).await
	}

	/// Poll-based implementation of the read loop.
	///
	/// Uses a single waiter that gets registered on all relevant kio channels,
	/// avoiding the need for `tokio::select!` or `FuturesUnordered`.
	pub fn poll_read(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<Frame>, F::Error>> {
		// Grab any new groups from the track, recording whether the track is finished.
		let finished = self.poll_read_finish(waiter)?.is_ready();

		// On startup, we want to poll every pending group and advance self.current to the first with a frame.
		if self.startup {
			// NOTE: We loop in ascending order, so earlier groups will win the race.
			for (i, group) in self.pending.iter_mut().enumerate() {
				// We call poll_min_timestamp to try to buffer at least one frame per group.
				// This returns Ready(Ok) if there is a buffered frame.
				if !matches!(group.poll_min_timestamp(waiter, &self.format), Poll::Ready(Ok(_))) {
					continue;
				}

				// Start reading from this group and skip any previous groups.
				self.current = group.sequence;
				self.startup = false;
				self.pending.drain(0..i);
				break;
			}
		}

		loop {
			// A newer group whose timestamps jumped backwards means the publisher reneged
			// the buffered tail. Record the boundary and resume from the new epoch, then restart.
			if self.poll_reset(waiter)? {
				continue;
			}

			// Drop any reneged stragglers whose timestamps have since resolved them as old.
			self.poll_classify(waiter)?;

			// Return the next frame from the current group if possible.
			// If the current group is finished or errored, advance to the next group.
			while let Some(group) = self.pending.front_mut()
				&& group.sequence <= self.current
			{
				match group.poll_read(waiter, &self.format) {
					Poll::Ready(Ok(Some(frame))) => {
						// Track the live edge (the max timestamp and the group that carries it) so a
						// later backwards jump is detectable and the old epoch's tail is anchored.
						let seq = group.group.sequence;
						let ts = frame.timestamp;
						if self.rewind.live_edge.is_none_or(|(_, high)| ts > high) {
							self.rewind.live_edge = Some((seq, ts));
						}
						return Poll::Ready(Ok(Some(frame)));
					}
					// Still blocked on this group, don't skip it yet.
					Poll::Pending => break,
					Poll::Ready(Err(e)) => {
						// Tell a relay group eviction/abort (skip) from a payload decode error
						// (propagate). The moq_net group's own terminal state is the source of
						// truth: an evicted/aborted group reports the transport error from
						// poll_finished, while a malformed payload leaves the group live or
						// cleanly finished. A decode error is real and the caller must see it,
						// not have the group silently dropped.
						if !group.poll_aborted(waiter) {
							return Poll::Ready(Err(e));
						}
						// The group aged out of the relay cache (`Error::Old`) or was otherwise
						// aborted. Any sequences between it and the next buffered group were
						// evicted alongside it, so jump straight to that group instead of
						// stepping one-by-one and then blocking on a sequence gap of groups
						// that will never arrive.
						tracing::warn!(error = ?e, "current group evicted; skipping to next buffered group");
						self.pending.pop_front();
						self.current = self.pending.front().map_or(self.current + 1, |g| g.sequence);
					}
					// Cleanly finished group: advance to the next sequence.
					Poll::Ready(Ok(None)) => {
						self.pending.pop_front();
						self.current += 1;
					}
				}
			}

			// Get the current group's min timestamp (the reference for latency
			// comparison) and its furthest presentation point (timestamp + duration).
			let (oldest_timestamp, current_end) = if let Some(current) = self.pending.front_mut()
				&& current.sequence <= self.current
			{
				match current.poll_min_timestamp(waiter, &self.format) {
					Poll::Ready(Ok(ts)) => (Some(std::time::Duration::from(ts)), current.max_end),
					_ => (None, None),
				}
			} else {
				(None, None)
			};

			// Find the first newer group with data (our skip target) and where it starts.
			let mut next_group = None;
			for (i, group) in self.pending.iter_mut().enumerate() {
				if group.sequence <= self.current {
					continue;
				}

				if let Poll::Ready(Ok(ts)) = group.poll_min_timestamp(waiter, &self.format) {
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

				if let Poll::Ready(Ok(ts)) = group.poll_max_timestamp(waiter, &self.format) {
					max_timestamp = max_timestamp.max(ts.into());
					break; // We know older groups won't be newer than this.
				}
			}

			let should_skip = if let Some((_, next_start)) = next_group {
				if let Some(oldest) = oldest_timestamp {
					// Current group is blocking. Skip if newer groups have pulled past
					// the latency budget, or if the current group has already presented
					// up to where the next group begins (duration coverage) so there's
					// nothing left worth waiting for.
					let over_latency = max_timestamp.saturating_sub(oldest) >= self.latency;
					let covered = current_end.is_some_and(|end| end >= next_start);
					over_latency || covered
				} else {
					// The current group can't produce a timestamp: either it's missing
					// entirely -- a lower sequence the cache evicted, so `front` is already
					// past `current` -- or it's finished/empty. With a newer group buffered,
					// skip if the track is done OR the current sequence is simply gone. On a
					// live track a buffered higher sequence means the missing one was evicted
					// (the relay delivers in order), not merely late, so waiting is futile.
					finished || self.pending.front().is_some_and(|g| g.sequence > self.current)
				}
			} else {
				false
			};

			if let Some((new_idx, _)) = next_group
				&& should_skip
			{
				self.pending.drain(0..new_idx);
				let new_current = self.pending.front().map(|g| g.sequence).unwrap();

				tracing::debug!(old = self.current, new = new_current, "skipping slow groups");

				self.current = new_current;
				continue;
			}

			if finished && self.pending.is_empty() {
				return Poll::Ready(Ok(None));
			}

			return Poll::Pending;
		}
	}

	// Reads any new groups from the track until we're completely finished.
	//
	// Returns Pending until all groups have been consumed.
	fn poll_read_finish(&mut self, waiter: &kio::Waiter) -> Poll<Result<(), F::Error>> {
		loop {
			let Some(group) = ready!(self.track.poll_recv_group(waiter)?) else {
				// Track is finished.
				return Poll::Ready(Ok(()));
			};

			let reader = GroupBuffer::new(group);
			let sequence = reader.group.sequence;

			// Normally we drop anything behind the playback cursor. With an active reset the
			// cursor isn't a valid floor: a late new-epoch group can sit below it. Defer to
			// the boundary, admitting ambiguous groups so poll_classify can rule on them once
			// their timestamps arrive.
			let drop = match &self.rewind.boundary {
				Some(reset) => match reset.by_sequence(sequence) {
					Some(true) => true,                     // old epoch: reneged
					Some(false) => sequence < self.current, // new epoch, but already played past
					None => false,                          // ambiguous: admit, classify later
				},
				None => sequence < self.current,
			};
			if drop {
				tracing::debug!(old = ?sequence, current = ?self.current, "skipping old group");
				continue;
			}

			let idx = self
				.pending
				.partition_point(|g| g.group.sequence < reader.group.sequence);
			self.pending.insert(idx, reader);
		}
	}

	// Detect a publisher "rewind" and record the reneged boundary.
	//
	// A newer group (sequence climbs) whose first frame lands before the live edge (timestamp
	// goes backwards) can only be an explicit reneg of the buffered tail. We record a [`Reset`]
	// from `(live-edge group, rewound group, rewound timestamp)`, drop the buffered groups it
	// can already prove stale, bump the discontinuity counter, and resume playback from the
	// earliest survivor. Groups still ambiguous (a late new-epoch group vs. an old straggler)
	// are kept and resolved by [`poll_classify`](Self::poll_classify) once their timestamps
	// arrive.
	//
	// Returns true if a reset happened, signalling the caller to restart the read loop.
	fn poll_reset(&mut self, waiter: &kio::Waiter) -> Result<bool, F::Error> {
		let Some((prev_max, live_edge)) = self.rewind.live_edge else {
			return Ok(false);
		};

		// Scan newer groups from the back (highest sequence first) for a rewind: a group whose
		// timestamp went strictly backwards past the live edge. Checking only `back()` would
		// miss a rewind that a higher-sequence group masks by having already caught back up
		// (timestamp >= live edge) or by having no frame yet. Pending is sequence-sorted, so we
		// take the highest-sequence group that actually rewound.
		let reset = {
			let mut found = None;
			for group in self.pending.iter_mut().rev() {
				// Once we reach the playback cursor, older groups can't rewind the timeline.
				if group.group.sequence <= self.current {
					break;
				}

				// Skip groups with no frame yet; a lower-sequence one may still have rewound.
				let Poll::Ready(Ok(min)) = group.poll_min_timestamp(waiter, &self.format) else {
					continue;
				};

				if min < live_edge {
					found = Some(Reset {
						prev_max,
						group: group.group.sequence,
						timestamp: min,
					});
					break;
				}
			}

			let Some(reset) = found else {
				return Ok(false);
			};
			reset
		};

		// Drop buffered groups the boundary can already prove are old-epoch. Ambiguous ones
		// (no verdict by sequence, or timestamp not read yet) are kept for poll_classify.
		self.pending.retain(|g| match reset.by_sequence(g.group.sequence) {
			Some(stale) => !stale,
			None => g.min_timestamp.is_none_or(|ts| !reset.is_stale(g.group.sequence, ts)),
		});

		self.rewind.discontinuity += 1;
		tracing::debug!(
			prev_max = reset.prev_max,
			group = reset.group,
			discontinuity = self.rewind.discontinuity,
			"buffer reset: group timestamps rewound"
		);
		self.rewind.boundary = Some(reset);
		// Resume from the earliest survivor; if none buffered yet, from the rewound group.
		self.current = self.pending.front().map_or(reset.group, |g| g.group.sequence);
		self.rewind.live_edge = Some((reset.group, reset.timestamp));

		Ok(true)
	}

	// Resolve groups left ambiguous by a reset once their timestamps arrive.
	//
	// A group whose sequence falls in the reset's ambiguous span could be a late new-epoch
	// group (keep) or an old straggler whose higher timestamp simply hadn't been seen at
	// detection time (drop). We can only tell once it has a frame, so we re-check each loop
	// iteration and drop the ones that resolve to stale.
	fn poll_classify(&mut self, waiter: &kio::Waiter) -> Result<(), F::Error> {
		let Some(reset) = self.rewind.boundary else {
			return Ok(());
		};

		let mut i = 0;
		while i < self.pending.len() {
			let group = &mut self.pending[i];
			// Only ambiguous-by-sequence groups need a timestamp verdict.
			if reset.by_sequence(group.group.sequence).is_some() {
				i += 1;
				continue;
			}

			match group.poll_min_timestamp(waiter, &self.format) {
				Poll::Ready(Ok(min)) if reset.is_stale(group.group.sequence, min) => {
					self.pending.remove(i);
				}
				_ => i += 1,
			}
		}

		Ok(())
	}

	/// Set the maximum latency tolerance.
	pub fn set_latency(&mut self, latency: std::time::Duration) {
		self.latency = latency;
	}
}

/// Internal reader for a group of frames.
///
/// Handles two-phase frame reading (get FrameConsumer, then read all data),
/// timestamp parsing, and min/max timestamp tracking for latency decisions.
struct GroupBuffer {
	group: moq_net::group::Consumer,

	// The current frame index within the group.
	index: usize,

	// Read frames that haven't been consumed yet.
	buffered: VecDeque<Frame>,

	// The minimum timestamp in the group.
	min_timestamp: Option<Timestamp>,

	// The maximum timestamp in the group.
	max_timestamp: Option<Timestamp>,

	// The furthest presentation point reached so far, i.e. max(timestamp + duration).
	// Equals the max timestamp when the container carries no per-frame duration.
	// Stored as a wall-clock duration so cross-scale comparisons are cheap.
	max_end: Option<std::time::Duration>,
}

impl GroupBuffer {
	fn new(group: moq_net::group::Consumer) -> Self {
		Self {
			group,
			index: 0,
			buffered: VecDeque::new(),
			max_timestamp: None,
			min_timestamp: None,
			max_end: None,
		}
	}

	/// Poll for the next frame from this group.
	fn poll_read<F: Container>(&mut self, waiter: &kio::Waiter, format: &F) -> Poll<Result<Option<Frame>, F::Error>> {
		if let Some(frame) = self.buffered.pop_front() {
			return Poll::Ready(Ok(Some(frame)));
		}

		match ready!(self.buffer_one(waiter, format)?) {
			true => Poll::Ready(Ok(Some(self.buffered.pop_front().unwrap()))),
			false => Poll::Ready(Ok(None)),
		}
	}

	// Add one more frame to the buffer if possible.
	//
	// Returns false if the group is finished.
	fn buffer_once<F: Container>(&mut self, waiter: &kio::Waiter, format: &F) -> Poll<Result<bool, F::Error>> {
		let Some(frames) = ready!(format.poll_read(&mut self.group, waiter)?) else {
			return Poll::Ready(Ok(false));
		};

		for mut frame in frames {
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
			let duration = frame.duration.map(std::time::Duration::from).unwrap_or_default();
			let end = std::time::Duration::from(frame.timestamp) + duration;
			self.max_end = Some(match self.max_end {
				Some(existing) => existing.max(end),
				None => end,
			});

			// First frame of a group is always a keyframe by protocol invariant; trust
			// the container's flag otherwise so CMAF mid-group keyframes survive.
			frame.keyframe = frame.keyframe || self.index == 0;
			self.index += 1;

			self.buffered.push_back(frame);
		}

		Poll::Ready(Ok(true))
	}

	fn buffer_one<F: Container>(&mut self, waiter: &kio::Waiter, format: &F) -> Poll<Result<bool, F::Error>> {
		loop {
			if !self.buffered.is_empty() {
				return Poll::Ready(Ok(true));
			}
			if !ready!(self.buffer_once(waiter, format)?) {
				return Poll::Ready(Ok(false));
			}
			// poll_read returned Some(vec![]): a wire frame decoded to no media
			// frames, so loop and try again.
		}
	}

	fn buffer_all<F: Container>(&mut self, waiter: &kio::Waiter, format: &F) -> Poll<Result<(), F::Error>> {
		while ready!(self.buffer_once(waiter, format)?) {}
		Poll::Ready(Ok(()))
	}

	/// Poll for the maximum timestamp in this group.
	fn poll_max_timestamp<F: Container>(
		&mut self,
		waiter: &kio::Waiter,
		format: &F,
	) -> Poll<Result<Timestamp, F::Error>> {
		// Keep reading more frames just to advance the max timestamp.
		let _ = self.buffer_all(waiter, format)?;

		if let Some(max) = self.max_timestamp {
			return Poll::Ready(Ok(max));
		}

		if let Poll::Ready(_frames) = self.group.poll_finished(waiter)? {
			return Poll::Ready(Err(moq_net::Error::Decode(moq_net::DecodeError::Short).into()));
		}

		Poll::Pending
	}

	fn poll_min_timestamp<F: Container>(
		&mut self,
		waiter: &kio::Waiter,
		format: &F,
	) -> Poll<Result<Timestamp, F::Error>> {
		let _ = self.buffer_one(waiter, format)?;

		if let Some(min) = self.min_timestamp {
			return Poll::Ready(Ok(min));
		}

		if let Poll::Ready(_frames) = self.group.poll_finished(waiter)? {
			return Poll::Ready(Err(moq_net::Error::Decode(moq_net::DecodeError::Short).into()));
		}

		Poll::Pending
	}

	/// True if the group's moq_net stream was reset/aborted (evicted, `Old`,
	/// cancelled, ...), as opposed to still live or cleanly finished. Lets the
	/// consumer tell a transport eviction from a payload decode error: the former
	/// surfaces as a terminal transport error from `poll_finished`, the latter
	/// leaves the group readable or finished.
	fn poll_aborted(&mut self, waiter: &kio::Waiter) -> bool {
		matches!(self.group.poll_finished(waiter), Poll::Ready(Err(_)))
	}
}

impl std::ops::Deref for GroupBuffer {
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

		fn poll_read(
			&self,
			group: &mut moq_net::group::Consumer,
			waiter: &kio::Waiter,
		) -> Poll<Result<Option<Vec<Frame>>, Self::Error>> {
			use bytes::Buf;

			let Some(mut data) = ready!(group.poll_read_frame(waiter)?).map(|f| f.payload) else {
				return Poll::Ready(Ok(None));
			};

			let timestamp = ts(data.get_u64_le());
			let duration = ts(data.get_u64_le());
			let payload = data.copy_to_bytes(data.remaining());

			Poll::Ready(Ok(Some(vec![Frame {
				timestamp,
				payload,
				keyframe: false,
				duration: Some(duration),
			}])))
		}
	}

	/// Write one DurationWire frame (timestamp and duration in µs) into a group.
	fn write_duration_frame(group: &mut moq_net::group::Producer, timestamp: Timestamp, duration: Timestamp) {
		group
			.write_frame(timestamp, encode_duration_frame(timestamp, duration))
			.unwrap();
	}

	/// Write a finished group with explicit sequence and timestamps (Container::Legacy format).
	fn write_group(track: &mut moq_net::track::Producer, sequence: u64, timestamps: &[Timestamp]) {
		let mut group = track.create_group(moq_net::group::Info { sequence }).unwrap();
		for &timestamp in timestamps {
			let frame = Frame {
				timestamp,
				payload: Bytes::from_static(&[0xDE, 0xAD]),
				keyframe: false,
				duration: None,
			};
			Container::Legacy.write(&mut group, &[frame]).unwrap();
		}
		group.finish().unwrap();
	}

	/// Drain all available frames with a per-read timeout.
	async fn read_all(consumer: &mut Consumer<Container>) -> Result<Vec<Frame>, crate::Error> {
		let mut frames = Vec::new();
		loop {
			match tokio::time::timeout(Duration::from_millis(200), consumer.read()).await {
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

	// ---- Basic Reading ----

	#[tokio::test]
	async fn read_single_group() {
		let mut track = track_producer("test", hang::container::track_info());
		let consumer_track = track.subscribe(None);
		let mut consumer = Consumer::new(consumer_track, Container::Legacy).with_latency(Duration::from_millis(500));

		write_group(&mut track, 0, &[ts(0)]);
		track.finish().unwrap();

		let frames = read_all(&mut consumer).await.unwrap();
		assert_eq!(frames.len(), 1);
		assert_eq!(frames[0].timestamp, ts(0));
		assert!(frames[0].keyframe);

		// Next read returns None (track ended)
		assert!(consumer.read().await.unwrap().is_none());
	}

	#[tokio::test]
	async fn read_multiple_frames_single_group() {
		let mut track = track_producer("test", hang::container::track_info());
		let consumer_track = track.subscribe(None);
		let mut consumer = Consumer::new(consumer_track, Container::Legacy).with_latency(Duration::from_millis(500));

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
		let mut track = track_producer("test", hang::container::track_info());
		let consumer_track = track.subscribe(None);
		let mut consumer = Consumer::new(consumer_track, Container::Legacy).with_latency(Duration::from_millis(500));

		// 5 groups, 20ms spacing. Total span = 80ms, well within 500ms latency.
		for i in 0..5u64 {
			write_group(&mut track, i, &[ts(i * 20_000)]);
		}
		track.finish().unwrap();

		let frames = read_all(&mut consumer).await.unwrap();
		assert_eq!(frames.len(), 5);
	}

	// ---- Latency Skipping ----

	#[tokio::test]
	async fn latency_skip_delivers_recent_groups() {
		tokio::time::pause();
		let mut track = track_producer("test", hang::container::track_info());
		let consumer_track = track.subscribe(None);
		let mut consumer = Consumer::new(consumer_track, Container::Legacy).with_latency(Duration::from_millis(100));

		// Group 0: 5 frames, NOT finished (blocks consumer)
		let mut group0 = track.create_group(moq_net::group::Info { sequence: 0 }).unwrap();
		for f in 0..5u64 {
			Container::Legacy
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
		// Group 0's 5 frames + some later groups (earlier ones skipped by latency)
		assert!(frames.len() >= 25, "Expected >= 25 frames, got {}", frames.len());
		finisher.await.expect("finisher task panicked");
	}

	#[tokio::test]
	async fn zero_latency_skips_aggressively() {
		tokio::time::pause();
		let mut track = track_producer("test", hang::container::track_info());
		let consumer_track = track.subscribe(None);
		let mut consumer = Consumer::new(consumer_track, Container::Legacy).with_latency(Duration::ZERO);

		// Group 0 at ts 0 keeps timestamps monotonic with sequence (groups 1-9 follow at
		// g*50 ms), so the test exercises latency skipping and not rewind detection.
		let mut group0 = track.create_group(moq_net::group::Info { sequence: 0 }).unwrap();
		Container::Legacy
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
		let mut track = track_producer("test", hang::container::track_info());
		let consumer_track = track.subscribe(None);
		let mut consumer = Consumer::new(consumer_track, Container::Legacy).with_latency(Duration::from_millis(100));

		let mut group0 = track.create_group(moq_net::group::Info { sequence: 0 }).unwrap();
		Container::Legacy
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

	// ---- Rewind / reneg ----

	/// The reset boundary classifies out-of-order groups by `(sequence, timestamp)`.
	/// Old epoch peaked at group 55 (ts 100); group 58 rewound to ts 90.
	#[test]
	fn reset_classifies_out_of_order_groups() {
		let reset = Reset {
			prev_max: 55,
			group: 58,
			timestamp: ts(90),
		};

		// Late new-epoch gap-filler: sequence in (55, 58), ts below the rewind. Keep.
		assert!(!reset.is_stale(57, ts(88)));
		// Old straggler from before the peak (low sequence). Drop, even though its ts (86)
		// is below the rewind — sequence is what separates it from group 57.
		assert!(reset.is_stale(52, ts(86)));
		// Old straggler in the gap whose higher ts hadn't arrived at detection. Drop.
		assert!(reset.is_stale(56, ts(105)));
		// At or after the rewound group: new epoch. Keep.
		assert!(!reset.is_stale(58, ts(90)));
		assert!(!reset.is_stale(59, ts(92)));
		// At or before the old peak: old epoch. Drop.
		assert!(reset.is_stale(55, ts(100)));
	}

	/// A new-epoch group that arrives out of order *below* the resume point is kept and
	/// played, not dropped — the bug a plain "floor = detection group" would have.
	#[tokio::test]
	async fn reset_keeps_out_of_order_new_group() {
		tokio::time::pause();
		let mut track = track_producer("test", hang::container::track_info());
		let consumer_track = track.subscribe(None);
		let mut consumer = Consumer::new(consumer_track, Container::Legacy).with_latency(Duration::from_secs(10));

		// Old epoch, played forward until the live edge passes the rewind point.
		write_group(&mut track, 0, &[ts(0)]);
		write_group(&mut track, 1, &[ts(100_000)]);
		write_group(&mut track, 2, &[ts(200_000)]);
		// New epoch's later group (seq 5, ts 3 ms) arrives first and triggers the reset.
		write_group(&mut track, 5, &[ts(3_000)]);

		// Its earlier gap-fillers (seq 3, 4) land after the reset, below the resume point.
		let finisher = tokio::spawn(async move {
			tokio::time::sleep(Duration::from_millis(50)).await;
			write_group(&mut track, 3, &[ts(1_000)]);
			write_group(&mut track, 4, &[ts(2_000)]);
			track.finish().unwrap();
		});

		let frames = read_all(&mut consumer).await.unwrap();
		let micros: Vec<u128> = frames.iter().map(|f| f.timestamp.as_micros()).collect();

		// Old epoch played before the reset, and all three new-epoch groups survived —
		// including the two out-of-order gap-fillers that arrived below the resume point.
		assert!(micros.contains(&100_000), "old epoch played before the reset");
		assert!(
			micros.contains(&1_000) && micros.contains(&2_000) && micros.contains(&3_000),
			"out-of-order new-epoch groups kept, got {micros:?}"
		);
		assert_eq!(consumer.discontinuity(), 1, "one rewind detected");
		finisher.await.expect("finisher task panicked");
	}

	/// A rewind is detected even when a higher-sequence group has already caught back up past
	/// the live edge (so the newest pending group looks forward). Scanning only `back()` would
	/// miss the lower-sequence rewound group and play the reneged tail without a discontinuity.
	#[tokio::test]
	async fn reset_detected_behind_forward_newest_group() {
		tokio::time::pause();
		let mut track = track_producer("test", hang::container::track_info());
		let consumer_track = track.subscribe(None);
		let mut consumer = Consumer::new(consumer_track, Container::Legacy).with_latency(Duration::from_secs(10));

		// Old timeline, played to a live edge of 200 ms.
		write_group(&mut track, 0, &[ts(0)]);
		write_group(&mut track, 1, &[ts(100_000)]);
		write_group(&mut track, 2, &[ts(200_000)]);
		// Group 6 (highest sequence) is forward of the live edge, masking...
		write_group(&mut track, 6, &[ts(250_000)]);
		// ...group 5, a lower-sequence group that rewound below it.
		write_group(&mut track, 5, &[ts(50_000)]);
		track.finish().unwrap();

		let frames = read_all(&mut consumer).await.unwrap();
		let micros: Vec<u128> = frames.iter().map(|f| f.timestamp.as_micros()).collect();

		assert_eq!(
			consumer.discontinuity(),
			1,
			"rewind detected behind a forward newest group"
		);
		assert!(micros.contains(&50_000), "resumed at the rewound group, got {micros:?}");
		assert!(
			!micros.contains(&200_000),
			"the reneged tail was dropped, got {micros:?}"
		);
	}

	/// A newer group whose timestamps jump backwards past the buffered tail drops the
	/// reneged groups and resumes from the rewound group. Models a voice agent that
	/// runs ahead of playback and then interrupts to start a new utterance.
	#[tokio::test]
	async fn backwards_timestamp_resets_buffer() {
		tokio::time::pause();
		let mut track = track_producer("test", hang::container::track_info());
		let consumer_track = track.subscribe(None);
		// Large latency so the slow-group skip never fires; isolate the rewind path.
		let mut consumer = Consumer::new(consumer_track, Container::Legacy).with_latency(Duration::from_secs(10));

		// Publisher runs ahead: groups 0-4 at 0, 100, 200, 300, 400 ms.
		for i in 0..5u64 {
			write_group(&mut track, i, &[ts(i * 100_000)]);
		}
		// Then it reneges and rewinds: group 5 restarts the timeline at 0 ms.
		write_group(&mut track, 5, &[ts(0), ts(20_000)]);
		track.finish().unwrap();

		let frames = read_all(&mut consumer).await.unwrap();
		let timestamps: Vec<_> = frames.iter().map(|f| f.timestamp).collect();

		// We play forward until the live edge passes the rewind point (through 100 ms), then
		// the rewind drops the buffered-ahead groups (200/300/400 ms) and resumes at group 5.
		assert_eq!(timestamps, vec![ts(0), ts(100_000), ts(0), ts(20_000)]);
		assert_eq!(consumer.discontinuity(), 1);
	}

	/// Rewind detection is always on: a backwards group timestamp resets the buffer with no
	/// configuration. Here group 2 rewinds the timeline and bumps the discontinuity counter.
	#[tokio::test]
	async fn backwards_timestamp_always_resets() {
		let mut track = track_producer("test", hang::container::track_info());
		let consumer_track = track.subscribe(None);
		let mut consumer = Consumer::new(consumer_track, Container::Legacy).with_latency(Duration::from_secs(10));

		write_group(&mut track, 0, &[ts(0)]);
		write_group(&mut track, 1, &[ts(500_000)]);
		write_group(&mut track, 2, &[ts(0)]); // rewind
		track.finish().unwrap();

		let frames = read_all(&mut consumer).await.unwrap();
		let timestamps: Vec<_> = frames.iter().map(|f| f.timestamp).collect();

		assert_eq!(timestamps, vec![ts(0), ts(500_000), ts(0)]);
		assert_eq!(consumer.discontinuity(), 1, "the backwards group triggered a reset");
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
		Container::Legacy.write(group, &[frame]).unwrap();
	}

	/// An empty payload carries no media, so it's skipped rather than surfaced as a
	/// frame or raised as an error. A marker can sit anywhere -- mid-group (a gap) or
	/// last (a group's end) -- and a publisher emitting them must not break us.
	#[tokio::test]
	async fn empty_payload_is_skipped() {
		let mut track = track_producer("test", hang::container::track_info());
		let consumer_track = track.subscribe(None);
		let mut consumer = Consumer::new(consumer_track, Container::Legacy).with_latency(Duration::from_millis(500));

		let mut group = track.create_group(moq_net::group::Info { sequence: 0 }).unwrap();
		let media = |timestamp| Frame {
			timestamp,
			payload: Bytes::from_static(&[0xDE, 0xAD]),
			keyframe: false,
			duration: None,
		};
		Container::Legacy.write(&mut group, &[media(ts(0))]).unwrap();
		write_marker(&mut group, ts(16_000)); // mid-group gap marker
		Container::Legacy.write(&mut group, &[media(ts(33_000))]).unwrap();
		write_marker(&mut group, ts(50_000)); // the group's end
		group.finish().unwrap();
		track.finish().unwrap();

		let frames = read_all(&mut consumer).await.unwrap();
		assert_eq!(frames.len(), 2, "markers are not surfaced as media");
		assert_eq!(frames[0].timestamp, ts(0));
		assert_eq!(frames[1].timestamp, ts(33_000));
	}

	/// Reading a marker consumes its frame, so a run of them makes progress and the
	/// consumer reaches the next group instead of spinning. `read_all` times out per
	/// read, so a stall or an infinite loop fails this rather than hanging forever.
	#[tokio::test]
	async fn consecutive_markers_do_not_stall() {
		let mut track = track_producer("test", hang::container::track_info());
		let consumer_track = track.subscribe(None);
		let mut consumer = Consumer::new(consumer_track, Container::Legacy).with_latency(Duration::from_millis(500));

		let mut group = track.create_group(moq_net::group::Info { sequence: 0 }).unwrap();
		Container::Legacy
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

	// ---- Group Ordering ----

	#[tokio::test]
	async fn groups_delivered_in_sequence_order() {
		tokio::time::pause();
		let mut track = track_producer("test", hang::container::track_info());
		let consumer_track = track.subscribe(None);
		let mut consumer = Consumer::new(consumer_track, Container::Legacy).with_latency(Duration::from_millis(500));

		let mut group0 = track.create_group(moq_net::group::Info { sequence: 0 }).unwrap();
		Container::Legacy
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
		let mut track = track_producer("test", hang::container::track_info());
		let consumer_track = track.subscribe(None);
		let mut consumer = Consumer::new(consumer_track, Container::Legacy).with_latency(Duration::from_millis(500));

		write_group(&mut track, 0, &[ts(0)]);
		write_group(&mut track, 1, &[ts(30_000)]);
		track.finish().unwrap();

		let frames = read_all(&mut consumer).await.unwrap();
		assert_eq!(frames.len(), 2);
		assert_eq!(frames[0].timestamp, ts(0));
		assert_eq!(frames[1].timestamp, ts(30_000));
	}

	// ---- B-frames ----

	#[tokio::test]
	async fn bframes_within_group() {
		let mut track = track_producer("test", hang::container::track_info());
		let consumer_track = track.subscribe(None);
		let mut consumer = Consumer::new(consumer_track, Container::Legacy).with_latency(Duration::from_millis(500));

		write_group(&mut track, 0, &[ts(0), ts(66_000), ts(33_000)]);
		track.finish().unwrap();

		let frames = read_all(&mut consumer).await.unwrap();
		assert_eq!(frames.len(), 3);
		assert_eq!(frames[0].timestamp, ts(0));
		assert_eq!(frames[1].timestamp, ts(66_000));
		assert_eq!(frames[2].timestamp, ts(33_000));
	}

	// ---- Track Lifecycle ----

	#[tokio::test]
	async fn empty_track_returns_none() {
		tokio::time::pause();
		let mut track = track_producer("test", hang::container::track_info());
		let consumer_track = track.subscribe(None);
		let mut consumer = Consumer::new(consumer_track, Container::Legacy).with_latency(Duration::from_millis(500));

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
		let mut track = track_producer("test", hang::container::track_info());
		let consumer_track = track.subscribe(None);
		let mut consumer = Consumer::new(consumer_track, Container::Legacy).with_latency(Duration::from_millis(500));

		write_group(&mut track, 0, &[ts(0)]);
		track.abort(moq_net::Error::Cancel).unwrap();

		let result = tokio::time::timeout(Duration::from_millis(500), async {
			let mut frames = Vec::new();
			while let Ok(Some(frame)) = consumer.read().await {
				frames.push(frame);
			}
			frames
		})
		.await;

		assert!(result.is_ok(), "Consumer should not hang after track error");
	}

	// ---- Gap Recovery ----

	#[tokio::test]
	async fn gap_in_group_sequence_recovery() {
		let mut track = track_producer("test", hang::container::track_info());
		let consumer_track = track.subscribe(None);
		let mut consumer = Consumer::new(consumer_track, Container::Legacy).with_latency(Duration::from_millis(100));

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
		let mut track = track_producer("test", hang::container::track_info());
		let consumer_track = track.subscribe(None);
		let mut consumer = Consumer::new(consumer_track, Container::Legacy).with_latency(Duration::from_millis(80));

		write_group(&mut track, 5, &[ts(0), ts(20_000)]);
		write_group(&mut track, 7, &[ts(80_000), ts(100_000)]);
		write_group(&mut track, 8, &[ts(120_000), ts(140_000)]);
		write_group(&mut track, 9, &[ts(160_000), ts(180_000)]);
		track.finish().unwrap();

		let frames = read_all(&mut consumer).await.unwrap();
		assert!(frames.len() >= 4, "Expected >= 4 frames, got {}", frames.len());
	}

	// ---- Eviction recovery (pause/resume) ----

	/// A group that aged out of the relay cache (aborted with `Error::Old`) while the
	/// consumer was parked on it must not hang the consumer: reading it errors, and
	/// the consumer skips the gap to the next live group even though the track is NOT
	/// finished. This is the resume-from-pause path (the recorder stops reading, the
	/// group + the sequences after it evict, then it resumes).
	#[tokio::test]
	async fn evicted_group_with_gap_skips_to_live() {
		let mut track = track_producer("test", hang::container::track_info());
		let consumer_track = track.subscribe(None);
		let mut consumer = Consumer::new(consumer_track, Container::Legacy).with_latency(Duration::from_millis(100));

		// Group 0: a frame the consumer reads, positioning it there.
		let mut group0 = track.create_group(moq_net::group::Info { sequence: 0 }).unwrap();
		Container::Legacy
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
		let first = consumer.read().await.unwrap().unwrap();
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
		assert_eq!(next.timestamp, ts(150_000), "skipped the evicted gap to the live group");
	}

	/// A missing (evicted) sequence with a newer group buffered must be skipped even
	/// while the track is still LIVE -- not only once it's finished. This is the
	/// recorder resume stall: `current` points at a sequence the cache dropped, a
	/// higher group is buffered, and the track never finishes.
	#[tokio::test]
	async fn missing_sequence_skips_on_live_track() {
		let mut track = track_producer("test", hang::container::track_info());
		let consumer_track = track.subscribe(None);
		let mut consumer = Consumer::new(consumer_track, Container::Legacy).with_latency(Duration::from_millis(100));

		// Group 0, then group 2 -- sequence 1 is missing (evicted) and never arrives.
		// The track is NOT finished (live), the case that used to hang.
		write_group(&mut track, 0, &[ts(0), ts(20_000)]);
		write_group(&mut track, 2, &[ts(200_000)]);

		// Reading must reach group 2 across the gap instead of waiting forever for 1.
		let reached = tokio::time::timeout(Duration::from_secs(1), async {
			loop {
				let frame = consumer.read().await.unwrap().unwrap();
				if frame.timestamp == ts(200_000) {
					return;
				}
			}
		})
		.await;
		assert!(reached.is_ok(), "consumer hung on a missing sequence on a live track");
	}

	// ---- Decode errors ----

	/// A container that decodes each frame's payload as an 8-byte LE microsecond
	/// timestamp, but treats a `FAIL` payload as a malformed frame. Lets a test put a
	/// decodable frame first (so startup selects the group) and a decode failure after.
	struct FailingDecode;

	impl ContainerTrait for FailingDecode {
		type Error = crate::Error;

		fn write(&self, group: &mut moq_net::group::Producer, frames: &[Frame]) -> Result<(), Self::Error> {
			for frame in frames {
				group.write_frame(moq_net::Timestamp::ZERO, frame.payload.clone())?;
			}
			Ok(())
		}

		fn poll_read(
			&self,
			group: &mut moq_net::group::Consumer,
			waiter: &kio::Waiter,
		) -> Poll<Result<Option<Vec<Frame>>, Self::Error>> {
			use bytes::Buf;

			let Some(mut data) = ready!(group.poll_read_frame(waiter)?).map(|f| f.payload) else {
				return Poll::Ready(Ok(None));
			};
			if data.as_ref() == b"FAIL" {
				return Poll::Ready(Err(crate::Error::UnknownFormat("malformed payload".into())));
			}
			Poll::Ready(Ok(Some(vec![Frame {
				timestamp: ts(data.get_u64_le()),
				payload: Bytes::new(),
				keyframe: false,
				duration: None,
			}])))
		}
	}

	/// A decode error on a cleanly-finished group must propagate to the caller, not be
	/// mistaken for a relay eviction and silently skipped. Eviction-skip only fires when
	/// the group's stream was actually aborted.
	#[tokio::test]
	async fn decode_error_propagates() {
		tokio::time::pause();
		let mut track = track_producer("test", None);
		let consumer_track = track.subscribe(None);
		let mut consumer = Consumer::new(consumer_track, FailingDecode);

		// A decodable frame first (so startup selects the group), then a malformed one.
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
		let first = consumer.read().await;
		assert!(matches!(first, Ok(Some(_))), "first frame should decode, got {first:?}");

		let second = tokio::time::timeout(Duration::from_millis(200), consumer.read())
			.await
			.expect("consumer hung on a decode error");
		assert!(
			matches!(second, Err(crate::Error::UnknownFormat(_))),
			"decode error must propagate, got {second:?}"
		);
	}

	// ---- Frame Decoding ----

	#[tokio::test]
	async fn frame_timestamp_and_index_decoding() {
		let mut track = track_producer("test", hang::container::track_info());
		let consumer_track = track.subscribe(None);
		let mut consumer = Consumer::new(consumer_track, Container::Legacy).with_latency(Duration::from_millis(500));

		write_group(&mut track, 0, &[ts(0), ts(33_333), ts(66_666)]);
		track.finish().unwrap();

		let frames = read_all(&mut consumer).await.unwrap();
		assert_eq!(frames.len(), 3);

		assert_eq!(frames[0].timestamp, ts(0));
		assert!(frames[0].keyframe);

		assert_eq!(frames[1].timestamp, ts(33_333));

		assert_eq!(frames[2].timestamp, ts(66_666));
	}

	#[tokio::test]
	async fn frame_payload_preserved() {
		let mut track = track_producer("test", hang::container::track_info());
		let consumer_track = track.subscribe(None);
		let mut consumer = Consumer::new(consumer_track, Container::Legacy).with_latency(Duration::from_millis(500));

		let payload_bytes = vec![0x01, 0x02, 0x03, 0x04, 0x05];
		let mut group = track.create_group(moq_net::group::Info { sequence: 0 }).unwrap();
		Container::Legacy
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
		let mut track = track_producer("test", hang::container::track_info());
		let consumer_track = track.subscribe(None);
		let mut consumer = Consumer::new(consumer_track, Container::Legacy).with_latency(Duration::from_secs(10));

		let mut group0 = track.create_group(moq_net::group::Info { sequence: 0 }).unwrap();
		Container::Legacy
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
			while let Some(frame) = consumer.read().await.unwrap() {
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
		let mut track = track_producer("test", hang::container::track_info());
		let consumer_track = track.subscribe(None);
		let mut consumer = Consumer::new(consumer_track, Container::Legacy).with_latency(Duration::from_secs(3700));

		let one_hour = 3_600_000_000u64;
		write_group(&mut track, 0, &[ts(one_hour)]);
		track.finish().unwrap();

		let frames = read_all(&mut consumer).await.unwrap();
		assert_eq!(frames.len(), 1);
		assert_eq!(frames[0].timestamp, ts(one_hour));
		assert_eq!(frames[0].timestamp.as_micros(), one_hour as u128);
	}

	#[tokio::test]
	async fn set_latency_changes_behavior() {
		let mut track = track_producer("test", hang::container::track_info());
		let consumer_track = track.subscribe(None);
		let mut consumer = Consumer::new(consumer_track, Container::Legacy).with_latency(Duration::from_secs(10));

		write_group(&mut track, 0, &[ts(0)]);
		track.finish().unwrap();

		let frame = consumer.read().await.unwrap().unwrap();
		assert_eq!(frame.timestamp, ts(0));

		consumer.set_latency(Duration::from_millis(100));

		assert!(consumer.read().await.unwrap().is_none());
	}

	#[tokio::test]
	async fn max_timestamp_tracks_through_bframes() {
		tokio::time::pause();
		let mut track = track_producer("test", hang::container::track_info());
		let consumer_track = track.subscribe(None);
		// latency must exceed (group1_max - group0_min) = 100ms - 0ms = 100ms
		// to avoid the latency skip and test B-frame timestamp tracking.
		let mut consumer = Consumer::new(consumer_track, Container::Legacy).with_latency(Duration::from_millis(110));

		let mut group0 = track.create_group(moq_net::group::Info { sequence: 0 }).unwrap();
		for &timestamp in &[ts(0), ts(66_000), ts(33_000)] {
			Container::Legacy
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
			while let Some(frame) = consumer.read().await.unwrap() {
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
		let mut track = track_producer("test", hang::container::track_info());
		let consumer_track = track.subscribe(None);
		let mut consumer = Consumer::new(consumer_track, Container::Legacy).with_latency(Duration::from_millis(100));

		write_group(&mut track, 3, &[ts(0)]);
		write_group(&mut track, 5, &[ts(150_000)]);

		let mut group7 = track.create_group(moq_net::group::Info { sequence: 7 }).unwrap();
		Container::Legacy
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
			Container::Legacy
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
			while let Some(frame) = consumer.read().await.unwrap() {
				frames.push(frame);
			}
			frames
		})
		.await
		.expect("should not hang");

		finisher.await.unwrap();
	}

	#[tokio::test]
	async fn startup_skips_groups_without_data() {
		tokio::time::pause();
		let mut track = track_producer("test", hang::container::track_info());
		let consumer_track = track.subscribe(None);
		let mut consumer = Consumer::new(consumer_track, Container::Legacy).with_latency(Duration::from_millis(500));

		let _group5 = track.create_group(moq_net::group::Info { sequence: 5 }).unwrap();
		write_group(&mut track, 7, &[ts(210_000)]);
		track.finish().unwrap();

		let frames = tokio::time::timeout(Duration::from_millis(500), async {
			let mut frames = Vec::new();
			while let Some(frame) = consumer.read().await.unwrap() {
				frames.push(frame);
			}
			frames
		})
		.await
		.expect("should not hang");

		assert!(!frames.is_empty());
	}

	#[tokio::test]
	async fn startup_single_group_mid_stream() {
		let mut track = track_producer("test", hang::container::track_info());
		let consumer_track = track.subscribe(None);
		let mut consumer = Consumer::new(consumer_track, Container::Legacy).with_latency(Duration::from_millis(500));

		write_group(&mut track, 100, &[ts(3_000_000)]);
		track.finish().unwrap();

		let frames = read_all(&mut consumer).await.unwrap();
		assert_eq!(frames.len(), 1);
	}

	#[tokio::test]
	async fn multiple_sequential_latency_skips() {
		tokio::time::pause();
		let mut track = track_producer("test", hang::container::track_info());
		let consumer_track = track.subscribe(None);
		let mut consumer = Consumer::new(consumer_track, Container::Legacy).with_latency(Duration::from_millis(50));

		let mut group0 = track.create_group(moq_net::group::Info { sequence: 0 }).unwrap();
		Container::Legacy
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
		let mut track = track_producer("test", hang::container::track_info());
		let consumer_track = track.subscribe(None);
		let mut consumer = Consumer::new(consumer_track, Container::Legacy).with_latency(Duration::from_millis(100));

		let mut group0 = track.create_group(moq_net::group::Info { sequence: 0 }).unwrap();
		Container::Legacy
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
	/// a latency skip when the timestamp difference exceeds latency.
	/// Previously, the span was computed across newer groups only (zero for one
	/// group), so the skip never fired.
	#[tokio::test]
	async fn single_newer_group_triggers_skip() {
		tokio::time::pause();
		let mut track = track_producer("test", hang::container::track_info());
		let consumer_track = track.subscribe(None);
		let mut consumer = Consumer::new(consumer_track, Container::Legacy).with_latency(Duration::from_millis(100));

		// Group 0: stalled at ts=0, NOT finished
		let mut group0 = track.create_group(moq_net::group::Info { sequence: 0 }).unwrap();
		Container::Legacy
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

		// Group 1: finished, 200ms ahead (well beyond 100ms latency)
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
		let mut track = track_producer("test", hang::container::track_info());
		let consumer_track = track.subscribe(None);
		let mut consumer = Consumer::new(consumer_track, Container::Legacy).with_latency(Duration::from_millis(100));

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
		let mut track = track_producer("test", hang::container::track_info());
		let consumer_track = track.subscribe(None);
		let mut consumer = Consumer::new(consumer_track, Container::Legacy).with_latency(Duration::from_millis(500));

		let mut group0 = track.create_group(moq_net::group::Info { sequence: 0 }).unwrap();
		group0.abort(moq_net::Error::Cancel).unwrap();

		write_group(&mut track, 1, &[ts(30_000)]);
		track.finish().unwrap();

		let frames = read_all(&mut consumer).await.unwrap();
		assert_eq!(frames.len(), 1);
	}

	#[tokio::test]
	async fn track_finishes_while_reading() {
		tokio::time::pause();
		let mut track = track_producer("test", hang::container::track_info());
		let consumer_track = track.subscribe(None);
		let mut consumer = Consumer::new(consumer_track, Container::Legacy).with_latency(Duration::from_millis(500));

		write_group(&mut track, 0, &[ts(0)]);

		let finisher = tokio::spawn(async move {
			tokio::time::sleep(Duration::from_millis(20)).await;
			write_group(&mut track, 1, &[ts(30_000)]);
			tokio::time::sleep(Duration::from_millis(20)).await;
			track.finish().unwrap();
		});

		let frames = tokio::time::timeout(Duration::from_secs(2), async {
			let mut frames = Vec::new();
			while let Some(frame) = consumer.read().await.unwrap() {
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
		let mut track = track_producer("test", hang::container::track_info());
		let consumer_track = track.subscribe(None);
		let mut consumer = Consumer::new(consumer_track, Container::Legacy).with_latency(Duration::from_millis(500));

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

		let mut track = track_producer("video", hang::container::track_info());
		let consumer_track = track.subscribe(None);
		let mut consumer = Consumer::new(consumer_track, Container::Legacy).with_latency(Duration::from_millis(500));

		// Write frames using Container::Legacy encoding
		let mut group = track.create_group(moq_net::group::Info { sequence: 0 }).unwrap();
		for i in 0..3u64 {
			let frame = Frame {
				timestamp: ts(i * 33_333),
				payload: Bytes::from_static(&[0xDE, 0xAD]),
				keyframe: false,
				duration: None,
			};
			Container::Legacy.write(&mut group, &[frame]).unwrap();
		}
		group.finish().unwrap();
		track.finish().unwrap();

		let mut frames = Vec::new();
		while let Some(frame) = consumer.read().await.unwrap() {
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
	/// immediately, even with a latency budget far larger than the gap. Without
	/// duration support the consumer would block on the unfinished group forever.
	#[tokio::test]
	async fn duration_skip_advances_to_next_group() {
		tokio::time::pause();
		// DurationWire is a test-only container that doesn't stamp moq_net frame
		// timestamps; leave the track untimed so model-layer validation matches.
		let mut track = track_producer("test", None);
		let consumer_track = track.subscribe(None);
		// Latency dwarfs the gap, so only duration coverage can trigger the skip.
		let mut consumer = Consumer::new(consumer_track, DurationWire).with_latency(Duration::from_secs(10));

		// Group 0: one frame at ts=0 lasting 33ms, never finished.
		let mut group0 = track.create_group(moq_net::group::Info { sequence: 0 }).unwrap();
		write_duration_frame(&mut group0, ts(0), ts(33_000));

		// Group 1: finished, starts exactly where group 0's frame ends.
		let mut group1 = track.create_group(moq_net::group::Info { sequence: 1 }).unwrap();
		write_duration_frame(&mut group1, ts(33_000), ts(33_000));
		group1.finish().unwrap();

		track.finish().unwrap();

		let frames = tokio::time::timeout(Duration::from_secs(2), async {
			let mut frames = Vec::new();
			while let Some(frame) = consumer.read().await.unwrap() {
				frames.push(frame);
			}
			frames
		})
		.await
		.expect("consumer hung — duration skip regression");

		assert_eq!(frames.len(), 2);
		assert_eq!(frames[0].timestamp, ts(0));
		assert_eq!(frames[1].timestamp, ts(33_000));

		// group0 is intentionally never finished.
		drop(group0);
	}

	/// When the current group's frame ends before the next group begins, there is
	/// still a gap to cover, so we don't skip early: a late-arriving frame on the
	/// slow group is delivered rather than dropped.
	#[tokio::test]
	async fn duration_below_gap_does_not_skip() {
		tokio::time::pause();
		// DurationWire is untimed at the moq_net frame layer.
		let mut track = track_producer("test", None);
		let consumer_track = track.subscribe(None);
		let mut consumer = Consumer::new(consumer_track, DurationWire).with_latency(Duration::from_secs(10));

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
			while let Some(frame) = consumer.read().await.unwrap() {
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
}
