//! Splice multiple per-session tracks into one logical track, switching at group
//! boundaries, so a subscription survives route and connection changes.
//!
//! A [`Producer`] holds an ordered list of segments, each a [`track::Consumer`]
//! bounded to a half-open range of group sequences. [`Producer::switch`] appends a
//! segment starting at group `N` and caps the previous one at `N - 1`, so the
//! segments always partition the sequence space. A [`Subscriber`] reads across the
//! segments as if they were one track: bounds are enforced on the read side (a
//! route delivering outside its range is silently filtered), a segment dying
//! stalls the subscriber instead of erroring (the next [`Producer::switch`]
//! resumes it), and demand is forwarded to each underlying track intersected with
//! its segment bounds, so a session serving a segment just sees an ordinary
//! subscription that happens to start or end at a boundary.

use std::task::{Poll, ready};

use crate::{Datagram, Error, Result, group, track};

use super::subscription::Subscription;

/// One spliced source: a track bounded to a range of group sequences.
#[derive(Clone)]
struct Segment {
	/// Monotonic id, used by subscribers to reconcile their cursor set.
	id: u64,
	/// First group this segment serves, or `None` for no lower bound (the
	/// initial segment, which may start at the live edge).
	start: Option<u64>,
	/// Last group this segment serves (inclusive), or `None` while it is the
	/// newest segment.
	end: Option<u64>,
	/// The underlying per-session track.
	track: track::Consumer,
}

/// The demand to register on an underlying track: the subscriber's own
/// preferences intersected with a segment's bounds.
fn slice(prefs: &Subscription, start: Option<u64>, end: Option<u64>) -> Subscription {
	let mut sub = prefs.clone();
	sub.group_start = match (prefs.group_start, start) {
		(Some(a), Some(b)) => Some(a.max(b)),
		(Some(a), None) => Some(a),
		(None, bound) => bound,
	};
	sub.group_end = match (prefs.group_end, end) {
		(Some(a), Some(b)) => Some(a.min(b)),
		(Some(a), None) | (None, Some(a)) => Some(a),
		(None, None) => None,
	};
	sub
}

struct ResumeState {
	/// Segments in switch order; ranges are disjoint and ascending.
	segments: Vec<Segment>,
	/// Bumped on every mutation so subscribers know to reconcile.
	epoch: u64,
	/// No more switches will happen; the logical track ends with its last segment.
	finished: bool,
	/// The logical track was aborted; surfaced to every subscriber.
	abort: Option<Error>,
}

impl Default for ResumeState {
	fn default() -> Self {
		Self {
			segments: Vec::new(),
			epoch: 1,
			finished: false,
			abort: None,
		}
	}
}

/// Splices tracks into one logical track by switching at group boundaries.
///
/// Created with [`Self::new`]; hand out read access via [`Self::consume`]. Call
/// [`Self::switch`] whenever the serving route changes; subscribers migrate
/// transparently. The producer only manages boundaries: the actual groups are
/// written by whoever owns each underlying [`track::Producer`].
#[derive(Clone, Default)]
pub struct Producer {
	state: kio::Producer<ResumeState>,
}

impl Producer {
	/// Create a logical track with no segments; subscribers stall until the first
	/// [`Self::switch`].
	pub fn new() -> Self {
		Self::default()
	}

	/// Splice in a track serving groups from `start` onward, capping the previous
	/// segment at `start - 1`.
	///
	/// The first switch may pass `None` to leave the segment unbounded (it serves
	/// whatever the subscriber asks for, typically the live edge). Every later
	/// switch must pass `Some(start)` with `start` above the previous segment's
	/// start, so the ranges stay disjoint and ascending; otherwise this fails with
	/// [`Error::BoundsExceeded`] and the segment list is unchanged.
	///
	/// Bounds are enforced when reading: a previous segment's session may keep
	/// delivering past its new cap (the switch races the network) and those groups
	/// are simply never surfaced.
	pub fn switch(
		&mut self,
		track: impl super::origin_impl::Consume<track::Consumer>,
		start: impl Into<Option<u64>>,
	) -> Result<()> {
		let track = track.consume();
		let start = start.into();
		let mut state = self.state.write().map_err(|_| Error::Dropped)?;
		if state.finished || state.abort.is_some() {
			return Err(Error::Closed);
		}

		if let Some(prev) = state.segments.last_mut() {
			// A boundary is required (and must move forward) once a segment exists.
			let Some(start) = start else {
				return Err(crate::coding::BoundsExceeded.into());
			};
			if start <= prev.start.unwrap_or(0) && prev.start.is_some() {
				return Err(crate::coding::BoundsExceeded.into());
			}
			let Some(end) = start.checked_sub(1) else {
				return Err(crate::coding::BoundsExceeded.into());
			};
			prev.end = Some(end);
		}

		let id = state.epoch;
		state.segments.push(Segment {
			id,
			start,
			end: None,
			track,
		});
		state.epoch += 1;
		Ok(())
	}

	/// Mark the logical track as complete: no further switches. Subscribers see a
	/// clean end once the final segment's track finishes.
	pub fn finish(&mut self) -> Result<()> {
		let mut state = self.state.write().map_err(|_| Error::Dropped)?;
		if state.finished || state.abort.is_some() {
			return Err(Error::Closed);
		}
		state.finished = true;
		state.epoch += 1;
		Ok(())
	}

	/// Abort the logical track, releasing every subscriber with `err`.
	pub fn abort(&mut self, err: Error) -> Result<()> {
		let mut state = self.state.write().map_err(|_| Error::Dropped)?;
		if state.abort.is_some() {
			return Err(Error::Closed);
		}
		state.abort = Some(err);
		state.epoch += 1;
		Ok(())
	}

	/// Create a read handle for the logical track.
	pub fn consume(&self) -> Consumer {
		Consumer {
			state: self.state.consume(),
		}
	}

	/// Block until there are no consumers or subscribers left.
	pub async fn unused(&self) -> Result<()> {
		self.state.unused().await.map_err(|_| Error::Dropped)
	}
}

/// A cheap, cloneable read handle for a spliced logical track.
#[derive(Clone)]
pub struct Consumer {
	state: kio::Consumer<ResumeState>,
}

impl Consumer {
	/// Open a live subscription across every segment.
	///
	/// The subscription's preferences are forwarded to each underlying track
	/// intersected with its segment bounds, so each serving session sees plain
	/// demand for its own range. Pass `None` for [`Subscription::default`].
	pub fn subscribe(&self, subscription: impl Into<Option<Subscription>>) -> Subscriber {
		Subscriber {
			state: self.state.clone(),
			prefs: subscription.into().unwrap_or_default(),
			epoch: 0,
			finished: false,
			abort: None,
			segments: Vec::new(),
			next_sequence: 0,
		}
	}

	/// Fetch a single past group without a live subscription.
	///
	/// Routed to the most recent segment's track: old segments' sessions are
	/// usually gone by the time history is fetched, and a live route can serve
	/// groups outside its subscription bounds (bounds slice demand, not access).
	/// In-flight fetches on older segments are unaffected. Fails with
	/// [`Error::NotFound`] when no segment exists yet.
	pub fn fetch_group(&self, sequence: u64, options: impl Into<Option<group::Fetch>>) -> Result<kio::Pending<track::Fetching>> {
		let track = {
			let state = self.state.read();
			if let Some(err) = &state.abort {
				return Err(err.clone());
			}
			state.segments.last().map(|s| s.track.clone()).ok_or(Error::NotFound)?
		};
		Ok(track.fetch_group(sequence, options))
	}

	/// Return a cached group by sequence without blocking, or `None` if no segment
	/// has it cached. Newer segments are preferred.
	pub fn get_group(&self, sequence: u64) -> Option<group::Consumer> {
		let state = self.state.read();
		state.segments.iter().rev().find_map(|s| s.track.get_group(sequence))
	}

	/// The latest group sequence across the segments, clamped to their bounds.
	pub fn latest(&self) -> Option<u64> {
		let state = self.state.read();
		state
			.segments
			.iter()
			.filter_map(|s| {
				let latest = s.track.latest()?;
				Some(match s.end {
					Some(end) => latest.min(end),
					None => latest,
				})
			})
			.max()
	}
}

/// A subscriber's cursor over one segment.
struct SegmentSub {
	id: u64,
	start: Option<u64>,
	end: Option<u64>,
	sub: SubState,
}

enum SubState {
	/// Waiting for the underlying track's info (it may not be accepted yet).
	Pending(kio::Pending<track::Subscribing>),
	/// Live cursor over the underlying track.
	Active(track::Subscriber),
	/// The underlying track ended (finished, aborted, or dropped); nothing more
	/// will come from this segment. An abort is deliberately not surfaced: a dead
	/// route stalls the logical track until the next switch replaces it.
	Done,
}

/// A live subscription spliced across every segment of a logical track.
///
/// Reads switch between the underlying [`track::Subscriber`]s at the segment
/// boundaries. A segment's session failing does not error the subscription; it
/// stalls until [`Producer::switch`] provides a replacement, or ends cleanly once
/// the producer [`finish`](Producer::finish)es and the final segment completes.
pub struct Subscriber {
	state: kio::Consumer<ResumeState>,
	prefs: Subscription,

	/// Last observed producer epoch; a mismatch triggers a reconcile.
	epoch: u64,
	finished: bool,
	abort: Option<Error>,

	/// Cursors over the segments, in segment order.
	segments: Vec<SegmentSub>,

	/// One past the highest sequence returned by [`Self::next_group`].
	next_sequence: u64,
}

impl Subscriber {
	/// Sync with the producer: pick up new segments, apply moved boundaries, and
	/// register the waiter for the next change.
	fn poll_sync(&mut self, waiter: &kio::Waiter) {
		loop {
			let epoch = self.epoch;
			// Snapshot inside the predicate: `kio::Consumer::poll` yields the
			// predicate's value on change, or the final state once closed. Inline
			// the poll so its state borrow ends with this statement.
			let (snapshot, closed) = match self.state.poll(waiter, |state| {
				if state.epoch != epoch {
					Poll::Ready((state.epoch, state.finished, state.abort.clone(), state.segments.clone()))
				} else {
					Poll::Pending
				}
			}) {
				Poll::Ready(Ok(snapshot)) => (Some(snapshot), false),
				// The producer is gone; the state is frozen, so reconcile one last
				// time and stop watching (existing segments can still drain).
				Poll::Ready(Err(state)) => {
					let snapshot = (state.epoch != epoch)
						.then(|| (state.epoch, state.finished, state.abort.clone(), state.segments.clone()));
					(snapshot, true)
				}
				// Unchanged, and the waiter is now registered for the next switch.
				Poll::Pending => return,
			};

			if let Some(snapshot) = snapshot {
				self.apply(snapshot);
			}
			if closed {
				return;
			}
			// Loop: re-poll so the waiter is registered for the next change.
		}
	}

	/// Apply a producer snapshot: move boundaries on known segments and subscribe
	/// to new ones.
	fn apply(&mut self, snapshot: (u64, bool, Option<Error>, Vec<Segment>)) {
		let (epoch, finished, abort, segments) = snapshot;
		self.epoch = epoch;
		self.finished = finished;
		self.abort = abort;

		for segment in segments {
			match self.segments.iter_mut().find(|s| s.id == segment.id) {
				Some(existing) => {
					if existing.end != segment.end {
						existing.end = segment.end;
						if let SubState::Active(sub) = &mut existing.sub {
							sub.end_at(segment.end);
							// Also shrink the demand so the session can cap upstream.
							let _ = sub.update(slice(&self.prefs, segment.start, segment.end));
						}
					}
				}
				None => {
					let sub = segment.track.subscribe(slice(&self.prefs, segment.start, segment.end));
					self.segments.push(SegmentSub {
						id: segment.id,
						start: segment.start,
						end: segment.end,
						sub: SubState::Pending(sub),
					});
				}
			}
		}
	}

	/// Drive one segment cursor: resolve a pending subscription, then poll for an
	/// in-bounds group. Out-of-bounds groups (a route racing its cap) are skipped.
	fn poll_segment(seg: &mut SegmentSub, waiter: &kio::Waiter) -> Poll<Option<group::Consumer>> {
		loop {
			match &mut seg.sub {
				SubState::Pending(pending) => match pending.poll_ok(waiter) {
					Poll::Ready(Ok(mut sub)) => {
						// Enforce the bounds on the read cursor; demand bounds were
						// already applied at subscribe time.
						sub.start_at(seg.start.unwrap_or(0));
						sub.end_at(seg.end);
						seg.sub = SubState::Active(sub);
					}
					// The underlying track was rejected or closed: stall, not error.
					Poll::Ready(Err(_)) => {
						seg.sub = SubState::Done;
						return Poll::Ready(None);
					}
					Poll::Pending => return Poll::Pending,
				},
				SubState::Active(sub) => match sub.poll_recv_group(waiter) {
					Poll::Ready(Ok(Some(group))) => {
						// `start_at` already floors the cursor; enforce the cap here since
						// arrival-order reads don't honor `end_at`.
						if let Some(end) = seg.end
							&& group.sequence > end
						{
							continue;
						}
						return Poll::Ready(Some(group));
					}
					Poll::Ready(Ok(None)) => {
						seg.sub = SubState::Done;
						return Poll::Ready(None);
					}
					// A dead segment stalls the logical track rather than erroring;
					// the next switch resumes it.
					Poll::Ready(Err(_)) => {
						seg.sub = SubState::Done;
						return Poll::Ready(None);
					}
					Poll::Pending => return Poll::Pending,
				},
				SubState::Done => return Poll::Ready(None),
			}
		}
	}

	/// Poll for the next group in arrival order across the segments.
	///
	/// Returns `Poll::Ready(Ok(None))` once the producer finished and every
	/// segment completed, and `Poll::Ready(Err(_))` only if the producer aborted.
	pub fn poll_recv_group(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<group::Consumer>>> {
		self.poll_sync(waiter);

		let mut all_done = true;
		for seg in &mut self.segments {
			match Self::poll_segment(seg, waiter) {
				Poll::Ready(Some(group)) => {
					self.next_sequence = self.next_sequence.max(group.sequence.saturating_add(1));
					return Poll::Ready(Ok(Some(group)));
				}
				Poll::Ready(None) => {}
				Poll::Pending => all_done = false,
			}
		}

		if let Some(err) = &self.abort {
			return Poll::Ready(Err(err.clone()));
		}
		if self.finished && all_done {
			return Poll::Ready(Ok(None));
		}
		Poll::Pending
	}

	/// Receive the next group in arrival order across the segments.
	pub async fn recv_group(&mut self) -> Result<Option<group::Consumer>> {
		kio::wait(|waiter| self.poll_recv_group(waiter)).await
	}

	/// Poll for the next group with a higher sequence than any previously
	/// returned, skipping late arrivals, across the segments.
	pub fn poll_next_group(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<group::Consumer>>> {
		loop {
			match ready!(self.poll_recv_group(waiter))? {
				Some(group) if group.sequence.saturating_add(1) < self.next_sequence => continue,
				res => return Poll::Ready(Ok(res)),
			}
		}
	}

	/// Return the next group with a higher sequence than any previously returned.
	pub async fn next_group(&mut self) -> Result<Option<group::Consumer>> {
		kio::wait(|waiter| self.poll_next_group(waiter)).await
	}

	/// Poll for the next datagram, from the newest segment only (datagrams are a
	/// live best-effort channel; there is nothing to resume from older segments).
	pub fn poll_recv_datagram(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<Datagram>>> {
		self.poll_sync(waiter);

		if let Some(seg) = self.segments.last_mut()
			&& let SubState::Active(sub) = &mut seg.sub
		{
			match sub.poll_recv_datagram(waiter) {
				Poll::Ready(Ok(Some(datagram))) => return Poll::Ready(Ok(Some(datagram))),
				// Terminal states fall through to the logical checks below.
				Poll::Ready(_) => {}
				Poll::Pending => return Poll::Pending,
			}
		}

		if let Some(err) = &self.abort {
			return Poll::Ready(Err(err.clone()));
		}
		if self.finished {
			return Poll::Ready(Ok(None));
		}
		Poll::Pending
	}

	/// Receive the next datagram from the newest segment.
	pub async fn recv_datagram(&mut self) -> Result<Option<Datagram>> {
		kio::wait(|waiter| self.poll_recv_datagram(waiter)).await
	}

	/// Replace this subscriber's preferences, re-deriving each segment's demand.
	pub fn update(&mut self, subscription: Subscription) {
		self.prefs = subscription;
		for seg in &mut self.segments {
			if let SubState::Active(sub) = &mut seg.sub {
				let _ = sub.update(slice(&self.prefs, seg.start, seg.end));
			}
		}
	}
}

#[cfg(test)]
mod test {
	use super::*;
	use crate::{Timestamp, broadcast};
	use futures::FutureExt;
	use std::sync::Arc;

	fn track_pair(name: &str) -> (track::Producer, track::Consumer) {
		let producer = track::Producer::new(Arc::new(broadcast::Info::default()), name, None);
		let consumer = producer.consume();
		(producer, consumer)
	}

	fn write_group(producer: &mut track::Producer, sequence: u64, payload: &str) {
		let mut group = producer.create_group(group::Info { sequence }).unwrap();
		group
			.write_frame(Timestamp::ZERO, payload.as_bytes().to_vec())
			.unwrap();
		group.finish().unwrap();
	}

	fn recv(sub: &mut Subscriber) -> u64 {
		sub.recv_group()
			.now_or_never()
			.expect("should not block")
			.expect("should not error")
			.expect("should not be finished")
			.sequence
	}

	fn recv_pending(sub: &mut Subscriber) {
		assert!(sub.recv_group().now_or_never().is_none(), "should have blocked");
	}

	#[tokio::test]
	async fn switch_splices_groups() {
		let (mut track_a, consumer_a) = track_pair("a");
		let (mut track_b, consumer_b) = track_pair("b");

		let mut producer = Producer::new();
		producer.switch(&consumer_a, None).unwrap();

		let mut sub = producer.consume().subscribe(None);

		write_group(&mut track_a, 0, "a0");
		write_group(&mut track_a, 1, "a1");
		assert_eq!(recv(&mut sub), 0);
		assert_eq!(recv(&mut sub), 1);

		// Switch to B at group 2. A racing past its cap is filtered.
		producer.switch(&consumer_b, 2).unwrap();
		write_group(&mut track_a, 2, "a2-over-cap");
		write_group(&mut track_b, 2, "b2");
		write_group(&mut track_b, 3, "b3");

		assert_eq!(recv(&mut sub), 2);
		assert_eq!(recv(&mut sub), 3);
		recv_pending(&mut sub);
	}

	#[tokio::test]
	async fn demand_reflects_boundaries() {
		let (track_a, consumer_a) = track_pair("a");
		let (track_b, consumer_b) = track_pair("b");

		let mut producer = Producer::new();
		producer.switch(&consumer_a, None).unwrap();

		let mut sub = producer.consume().subscribe(Subscription::default().with_group_start(0));
		// Poll once so the subscriber registers on segment A.
		recv_pending(&mut sub);
		assert_eq!(track_a.subscription().unwrap().group_end, None);

		producer.switch(&consumer_b, 5).unwrap();
		recv_pending(&mut sub);

		// The old session sees its demand capped; the new one starts at the boundary.
		assert_eq!(track_a.subscription().unwrap().group_end, Some(4));
		assert_eq!(track_b.subscription().unwrap().group_start, Some(5));
	}

	#[tokio::test]
	async fn dead_segment_stalls_until_switch() {
		let (mut track_a, consumer_a) = track_pair("a");
		let (mut track_b, consumer_b) = track_pair("b");

		let mut producer = Producer::new();
		producer.switch(&consumer_a, None).unwrap();
		let mut sub = producer.consume().subscribe(None);

		write_group(&mut track_a, 0, "a0");
		assert_eq!(recv(&mut sub), 0);

		// The route dies: the subscriber stalls, it does not error.
		track_a.abort(Error::Dropped).unwrap();
		recv_pending(&mut sub);

		// A replacement resumes exactly where the old route left off.
		producer.switch(&consumer_b, 1).unwrap();
		write_group(&mut track_b, 1, "b1");
		assert_eq!(recv(&mut sub), 1);
	}

	#[tokio::test]
	async fn finish_ends_after_final_segment() {
		let (mut track_a, consumer_a) = track_pair("a");

		let mut producer = Producer::new();
		producer.switch(&consumer_a, None).unwrap();
		let mut sub = producer.consume().subscribe(None);

		write_group(&mut track_a, 0, "a0");
		assert_eq!(recv(&mut sub), 0);

		// Finishing the logical track alone isn't the end; the segment must drain.
		producer.finish().unwrap();
		recv_pending(&mut sub);

		track_a.finish().unwrap();
		assert!(
			sub.recv_group()
				.now_or_never()
				.expect("should not block")
				.expect("should not error")
				.is_none(),
			"should be finished"
		);
	}

	#[tokio::test]
	async fn fetch_routes_to_newest_segment() {
		let (track_a, consumer_a) = track_pair("a");
		let (mut track_b, consumer_b) = track_pair("b");

		let mut producer = Producer::new();
		producer.switch(&consumer_a, None).unwrap();
		producer.switch(&consumer_b, 10).unwrap();

		// A cached group on the newest segment resolves immediately, even below
		// its subscribe boundary: bounds slice demand, not access.
		write_group(&mut track_b, 3, "b3");
		let consumer = producer.consume();
		let group = consumer
			.fetch_group(3, None)
			.unwrap()
			.now_or_never()
			.expect("cached fetch should resolve")
			.unwrap();
		assert_eq!(group.sequence, 3);

		// Fetches never touch the old segment.
		drop(track_a);
	}

	#[tokio::test]
	async fn switch_validates_boundaries() {
		let (_track_a, consumer_a) = track_pair("a");
		let (_track_b, consumer_b) = track_pair("b");

		let mut producer = Producer::new();
		producer.switch(&consumer_a, None).unwrap();

		// A later switch requires an explicit, advancing boundary.
		assert!(producer.switch(&consumer_b, None).is_err());
		assert!(producer.switch(&consumer_b, 0).is_err());
		producer.switch(&consumer_b, 1).unwrap();
	}
}
