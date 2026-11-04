//! Datagram tracks: live, unreliable payloads addressed by their own sequence.
//!
//! A datagram track is fixed as one when it is created, independent of any stream track of
//! the same name. Each datagram is one self-contained payload carried in a single QUIC
//! datagram: best-effort, unordered, and never retransmitted, cached, or fetched. It has a
//! sequence from the track's own counter and a timestamp, and no group.
//!
//! A broadcast creates a [`Producer`] with
//! [`broadcast::Producer::create_datagrams`](crate::broadcast::Producer::create_datagrams),
//! and a reader asks for a [`Consumer`] with
//! [`broadcast::Consumer::datagrams`](crate::broadcast::Consumer::datagrams). A consumer
//! is live from when it is created: it never receives a datagram written before it.
//!
//! Delivery is best-effort per hop: a session drops (with a debug log) any datagram whose
//! encoded body exceeds the transport's datagram size, and sessions that can't carry
//! datagrams at all (moq-lite before 05, moq-transport for now, or stream-only transports
//! like WebSocket) never deliver them.
//!
//! Moq-lite 07 asks for a datagram track on a request of its own. Moq-lite 05 and 06 carry a
//! name's groups and datagrams on one subscription, as moq-transport does: a session serves
//! an old subscription with both of the name's tracks, and a subscriber keeps only the kind
//! it asked for, sharing one subscription with any reader of the other kind. Two limits
//! follow on those versions:
//!
//! - A subscription carries the datagram track that exists when it starts, so create a
//!   name's datagram track before, or with, its stream track.
//! - The wire never says a name has no datagram track, so a datagram reader of a name with
//!   only a stream track receives nothing until that track ends.

use std::{
	collections::VecDeque,
	sync::Arc,
	task::{Poll, ready},
};

use bytes::Bytes;

use crate::{Error, Result, Timescale, Timestamp, stats};

/// Hard ceiling on a datagram payload, matching the QUIC DATAGRAM frame limit.
///
/// This only bounds buffering; the real limit is per hop. Each session drops a datagram whose
/// encoded body exceeds the transport's current datagram size (roughly the path MTU minus QUIC
/// and MoQ header overhead), so callers should keep payloads well below the minimum path MTU
/// of 1200 bytes (e.g. a single audio frame).
pub(crate) const MAX_PAYLOAD: usize = u16::MAX as usize;

/// How many datagrams a track holds for consumers that have not read them yet.
///
/// A consumer more than this far behind skips to the oldest one still held, so a stalled
/// reader costs the track at most this many payloads.
const MAX_BUFFERED: usize = 64;

/// How many payload bytes a track holds for consumers that have not read them yet, beyond
/// which it drops the oldest, though never the newest. Datagrams that fit a path are well
/// under this; it bounds a local writer of large payloads.
const MAX_BUFFERED_BYTES: usize = 64 * 1024;

/// The default subscriber priority of a [`Consumer`].
const DEFAULT_PRIORITY: i8 = 0;

/// A single unreliable payload: its sequence, its timestamp, and the bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Datagram {
	/// The datagram track's own sequence number.
	pub sequence: u64,
	/// Presentation timestamp in the track's timescale, or `None` on a track without one.
	pub timestamp: Option<Timestamp>,
	/// The datagram payload.
	pub payload: Bytes,
}

/// Publisher-side properties of a datagram track, fixed when it is created.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Info {
	/// Units per second of each datagram's timestamp, or `None` for a track without one.
	///
	/// A track is all timed or all untimed: a write that doesn't match is refused with
	/// [`Error::TimestampMismatch`].
	pub timescale: Option<Timescale>,
	/// The publisher's priority for this track, higher first, defaulting to 0. It breaks
	/// ties between subscriptions of equal subscriber priority.
	pub priority: i8,
}

impl Info {
	/// Set the timestamp scale, or `None` for an untimed track, returning `self` for chaining.
	pub fn with_timescale(mut self, timescale: impl Into<Option<Timescale>>) -> Self {
		self.timescale = timescale.into();
		self
	}

	/// Set the publisher's priority, returning `self` for chaining.
	pub fn with_priority(mut self, priority: i8) -> Self {
		self.priority = priority;
		self
	}

	/// `timestamp` at this track's [`timescale`](Self::timescale).
	///
	/// A track is all timed or all untimed: a timestamp that doesn't match it, or one the
	/// track's scale can't hold, is [`Error::TimestampMismatch`]. A writer that changes
	/// state before its datagram reaches the track, such as encrypting it, rescales first so
	/// a refused timestamp changes nothing.
	pub fn rescale(&self, timestamp: Option<Timestamp>) -> Result<Option<Timestamp>> {
		match (timestamp, self.timescale) {
			(Some(timestamp), Some(timescale)) => Ok(Some(
				timestamp.convert(timescale).map_err(|_| Error::TimestampMismatch)?,
			)),
			(None, None) => Ok(None),
			_ => Err(Error::TimestampMismatch),
		}
	}

	/// Produce a standalone datagram track with this info, outside any broadcast.
	pub fn produce(self, name: impl Into<Arc<str>>) -> Producer {
		Producer::new(name.into(), Some(self))
	}
}

#[derive(Default)]
struct State {
	// `None` while a request waits for a handler to accept it.
	info: Option<Info>,
	buffer: VecDeque<Datagram>,
	// The payload bytes `buffer` holds.
	bytes: usize,
	// The absolute index of `buffer[0]`, so consumers keep a cursor past what was dropped.
	offset: u64,
	// The sequence `append` assigns next: one past the highest written, or `None` once
	// `u64::MAX` was written and no sequence follows it.
	next: Option<u64>,
	// Set once the track ends: `Ok` for a finish.
	end: Option<Result<()>>,
	// Live producer-side handles: producers and an unanswered request.
	producers: usize,
	// Each live consumer's subscriber priority.
	priorities: slab::Slab<i8>,
}

impl State {
	fn closed(&self) -> Result<()> {
		match &self.end {
			None => Ok(()),
			Some(Ok(())) => Err(Error::Closed),
			Some(Err(err)) => Err(err.clone()),
		}
	}

	fn push(&mut self, datagram: Datagram) {
		self.bytes += datagram.payload.len();
		while self.buffer.len() >= MAX_BUFFERED || (!self.buffer.is_empty() && self.bytes > MAX_BUFFERED_BYTES) {
			let dropped = self.buffer.pop_front().expect("not empty");
			self.bytes -= dropped.payload.len();
			self.offset += 1;
		}
		self.next = match (self.next, datagram.sequence.checked_add(1)) {
			(Some(next), Some(after)) => Some(next.max(after)),
			_ => None,
		};
		self.buffer.push_back(datagram);
	}

	fn end(&mut self, end: Result<()>) {
		if self.end.is_some() {
			return;
		}
		// A finish leaves what is buffered for the consumers already reading; an error
		// ends the track at once, so nobody reads what is left.
		let unread = end.is_err() || self.priorities.is_empty();
		self.end = Some(end);
		if unread {
			self.release();
		}
	}

	/// Drop what is buffered: nobody can read it any more. A consumer joining later starts
	/// past it, so this holds for an ended track once its last consumer leaves.
	fn release(&mut self) {
		self.offset += self.buffer.len() as u64;
		self.buffer.clear();
		self.bytes = 0;
	}

	/// Whether a consumer at `cursor` has a datagram to read: past what was dropped, it reads
	/// the oldest one still held.
	fn has_next(&self, cursor: u64) -> bool {
		!self.buffer.is_empty() && self.offset + (self.buffer.len() as u64) > cursor
	}

	fn priority(&self) -> Option<i8> {
		self.priorities.iter().map(|(_, priority)| *priority).max()
	}
}

/// The broadcast's handle on a datagram track, which keeps nothing alive.
#[derive(Clone)]
pub(super) struct Weak {
	name: Arc<str>,
	state: kio::Shared<State>,
}

impl Weak {
	/// Join the track as a new consumer, unless it already ended.
	pub(super) fn try_consume(&self, stats: stats::Scope) -> Option<Consumer> {
		Consumer::join(self.name.clone(), &self.state, stats)
	}
}

impl super::WeakEntry for Weak {
	fn is_closed(&self) -> bool {
		self.state.read().end.is_some()
	}

	fn same_channel(&self, other: &Self) -> bool {
		self.state.same_channel(&other.state)
	}
}

/// Writes datagrams to a datagram track.
///
/// Clones share the track and its sequence counter. The track ends on [`Self::finish`],
/// [`Self::abort`], or the last producer dropping, which consumers see as
/// [`Error::Dropped`].
pub struct Producer {
	name: Arc<str>,
	info: Info,
	state: kio::Shared<State>,
	stats: stats::Scope,
}

impl Producer {
	pub(super) fn new(name: Arc<str>, info: Option<Info>) -> Self {
		let state = State {
			info,
			next: Some(0),
			producers: 1,
			..Default::default()
		};
		Self {
			name,
			info: info.unwrap_or_default(),
			state: kio::Shared::new(state),
			stats: stats::Scope::default(),
		}
	}

	pub(super) fn with_stats(mut self, scope: stats::Scope) -> Self {
		self.stats = scope;
		self
	}

	pub(super) fn weak(&self) -> Weak {
		Weak {
			name: self.name.clone(),
			state: self.state.clone(),
		}
	}

	/// The track's name within its broadcast.
	pub fn name(&self) -> &str {
		&self.name
	}

	/// The track's properties.
	pub fn info(&self) -> &Info {
		&self.info
	}

	/// Append a datagram with the next sequence number, returning that sequence.
	///
	/// Pass `None` on an untimed track; a timestamp that doesn't match the track is
	/// [`Error::TimestampMismatch`]. Keep payloads well under the 1200-byte minimum path
	/// MTU: a hop drops a datagram that does not fit.
	pub fn append<B: crate::IntoBytes>(&mut self, timestamp: impl Into<Option<Timestamp>>, payload: B) -> Result<u64> {
		let mut state = self.state.lock();
		let sequence = state.next.ok_or(crate::coding::BoundsExceeded)?;
		self.push(
			&mut state,
			Datagram {
				sequence,
				timestamp: timestamp.into(),
				payload: payload.into_bytes(),
			},
		)?;
		Ok(sequence)
	}

	/// Insert a datagram with an explicit sequence, as a relay forwarding one does.
	///
	/// [`Self::append`] continues past the highest sequence inserted. Nothing checks the
	/// sequence otherwise: datagrams are unordered, so a repeat or an older one is delivered.
	pub fn insert(&mut self, datagram: Datagram) -> Result<()> {
		let mut state = self.state.lock();
		self.push(&mut state, datagram)
	}

	fn push(&self, state: &mut State, mut datagram: Datagram) -> Result<()> {
		if datagram.payload.len() > MAX_PAYLOAD {
			return Err(Error::FrameTooLarge);
		}
		state.closed()?;
		datagram.timestamp = self.info.rescale(datagram.timestamp)?;
		self.stats.meter().datagram(datagram.payload.len() as u64);
		state.push(datagram);
		Ok(())
	}

	/// A new consumer of the track, starting with the next datagram written.
	pub fn consume(&self) -> Consumer {
		Consumer::join(self.name.clone(), &self.state, stats::Scope::default())
			.unwrap_or_else(|| Consumer::ended(self.name.clone(), &self.state))
	}

	/// End the track cleanly: consumers read `None` once they have read what is buffered.
	pub fn finish(self) {
		self.state.lock().end(Ok(()));
	}

	/// End the track with an error.
	pub fn abort(self, err: Error) {
		self.state.lock().end(Err(err));
	}

	/// The highest subscriber priority among the track's consumers, or `None` when it has none.
	pub fn priority(&self) -> Option<i8> {
		self.state.read().priority()
	}

	/// Resolves to [`Self::priority`] once it differs from `last`.
	pub fn poll_priority(&self, waiter: &kio::Waiter, last: Option<i8>) -> Poll<Option<i8>> {
		let state = ready!(self.state.poll(waiter, |state| match state.priority() == last {
			true => Poll::Pending,
			false => Poll::Ready(()),
		}));
		Poll::Ready(state.priority())
	}

	/// Ready once the track has no consumer; see [`Self::close_unused`].
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn poll_unused(&self, waiter: &kio::Waiter) -> Poll<()> {
		let _state = ready!(self.state.poll(waiter, |state| match state.priorities.is_empty() {
			true => Poll::Ready(()),
			false => Poll::Pending,
		}));
		Poll::Ready(())
	}

	/// End the track with [`Error::Cancel`] if it has no consumer, returning whether it ended.
	///
	/// Atomic with a consumer joining, so a session letting an unread track go never strands
	/// a reader that arrived meanwhile: that reader either keeps the track or finds it ended
	/// and asks for it again.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn close_unused(&self) -> bool {
		let mut state = self.state.lock();
		if !state.priorities.is_empty() {
			return false;
		}
		state.end(Err(Error::Cancel));
		true
	}

	/// Whether this is the same track as `other`.
	pub fn is_clone(&self, other: &Self) -> bool {
		self.state.same_channel(&other.state)
	}
}

impl Clone for Producer {
	fn clone(&self) -> Self {
		self.state.lock().producers += 1;
		Self {
			name: self.name.clone(),
			info: self.info,
			state: self.state.clone(),
			stats: self.stats.clone(),
		}
	}
}

impl Drop for Producer {
	fn drop(&mut self) {
		let mut state = self.state.lock();
		state.producers -= 1;
		if state.producers == 0 {
			state.end(Err(Error::Dropped));
		}
	}
}

/// A datagram track a consumer asked for that a handler has yet to serve.
///
/// Yielded by [`broadcast::DatagramRequests::poll_requested`](crate::broadcast::DatagramRequests::poll_requested).
/// [`Self::accept`] it with the track's [`Info`] to start producing, or [`Self::reject`] it.
/// Dropping it rejects it with [`Error::Dropped`].
pub struct Request {
	producer: Producer,
}

impl Request {
	pub(super) fn new(name: Arc<str>) -> Self {
		Self {
			producer: Producer::new(name, None),
		}
	}

	pub(super) fn with_stats(mut self, scope: stats::Scope) -> Self {
		self.producer = self.producer.with_stats(scope);
		self
	}

	pub(super) fn weak(&self) -> Weak {
		self.producer.weak()
	}

	/// The requested track's name.
	pub fn name(&self) -> &str {
		self.producer.name()
	}

	/// Whether the request ended, such as by its last waiting consumer leaving.
	pub(super) fn is_closed(&self) -> bool {
		self.producer.state.read().end.is_some()
	}

	/// The highest subscriber priority among the consumers waiting, or `None` once they all left.
	pub fn priority(&self) -> Option<i8> {
		self.producer.priority()
	}

	/// [`Producer::poll_unused`] for a request.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn poll_unused(&self, waiter: &kio::Waiter) -> Poll<()> {
		self.producer.poll_unused(waiter)
	}

	/// [`Producer::close_unused`] for a request nobody waits for any more.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn close_unused(&self) -> bool {
		self.producer.close_unused()
	}

	/// Serve the track with `info`, returning its producer.
	pub fn accept(self, info: Info) -> Producer {
		let mut producer = self.producer;
		producer.state.lock().info = Some(info);
		producer.info = info;
		producer
	}

	/// Refuse the track: its consumers read `err`.
	pub fn reject(self, err: Error) {
		self.producer.abort(err);
	}
}

/// Reads a datagram track's datagrams, in arrival order, from when it was created.
///
/// Each consumer holds a subscriber priority that the track's publisher sees aggregated as
/// the highest among its consumers. A clone keeps the original's priority and position.
pub struct Consumer {
	name: Arc<str>,
	state: kio::Shared<State>,
	// This consumer's entry in `State::priorities`, or `None` for one created on an ended track.
	slot: Option<usize>,
	// The absolute index of the next datagram to read.
	cursor: u64,
	stats: stats::Scope,
}

impl Consumer {
	/// Join a live track, or `None` once it ended.
	fn join(name: Arc<str>, state: &kio::Shared<State>, stats: stats::Scope) -> Option<Self> {
		let mut guard = state.lock();
		if guard.end.is_some() {
			return None;
		}
		let slot = guard.priorities.insert(DEFAULT_PRIORITY);
		let cursor = guard.offset + guard.buffer.len() as u64;
		drop(guard);
		Some(Self {
			name,
			state: state.clone(),
			slot: Some(slot),
			cursor,
			stats,
		})
	}

	fn ended(name: Arc<str>, state: &kio::Shared<State>) -> Self {
		// Past what a finish left buffered, which is for the consumers already reading.
		let cursor = {
			let state = state.read();
			state.offset + state.buffer.len() as u64
		};
		Self {
			name,
			state: state.clone(),
			slot: None,
			cursor,
			stats: stats::Scope::default(),
		}
	}

	pub(super) fn with_stats(mut self, scope: stats::Scope) -> Self {
		self.stats = scope;
		self
	}

	/// The track's name within its broadcast.
	pub fn name(&self) -> &str {
		&self.name
	}

	/// The track's properties, once its publisher accepted it.
	#[cfg_attr(feature = "async", kio::wrapper)]
	pub fn poll_info(&self, waiter: &kio::Waiter) -> Poll<Result<Info>> {
		let state = ready!(
			self.state
				.poll(waiter, |state| match state.info.is_some() || state.end.is_some() {
					true => Poll::Ready(()),
					false => Poll::Pending,
				})
		);
		match (state.info, &state.end) {
			(Some(info), _) => Poll::Ready(Ok(info)),
			(None, Some(Err(err))) => Poll::Ready(Err(err.clone())),
			(None, _) => Poll::Ready(Err(Error::Closed)),
		}
	}

	/// The next datagram, or `None` once the track finished and this consumer read what it
	/// buffered. An aborted track errors at once.
	///
	/// A consumer that fell behind skips the datagrams the track no longer holds.
	#[cfg_attr(feature = "async", kio::wrapper)]
	pub fn poll_recv(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<Datagram>>> {
		let cursor = self.cursor;
		let state = ready!(self.state.poll(waiter, |state| {
			match state.end.is_some() || state.has_next(cursor) {
				true => Poll::Ready(()),
				false => Poll::Pending,
			}
		}));
		// Buffered datagrams first: a finished track still delivers them.
		if !state.has_next(cursor) {
			let end = state.end.clone().expect("ready only once ended or buffered");
			return Poll::Ready(end.map(|()| None));
		}
		let index = self.cursor.max(state.offset);
		let datagram = state.buffer[(index - state.offset) as usize].clone();
		drop(state);
		self.cursor = index + 1;
		self.stats.meter().datagram(datagram.payload.len() as u64);
		Poll::Ready(Ok(Some(datagram)))
	}

	/// This consumer's subscriber priority.
	pub fn priority(&self) -> i8 {
		self.slot
			.and_then(|slot| self.state.read().priorities.get(slot).copied())
			.unwrap_or(DEFAULT_PRIORITY)
	}

	/// Set this consumer's subscriber priority, higher first.
	pub fn set_priority(&mut self, priority: i8) {
		if let Some(slot) = self.slot {
			self.state.lock().priorities[slot] = priority;
		}
	}

	/// Whether this reads the same track as `other`.
	pub fn is_clone(&self, other: &Self) -> bool {
		self.state.same_channel(&other.state)
	}
}

impl Clone for Consumer {
	fn clone(&self) -> Self {
		let slot = self.slot.map(|slot| {
			let mut state = self.state.lock();
			let priority = state.priorities[slot];
			state.priorities.insert(priority)
		});
		Self {
			name: self.name.clone(),
			state: self.state.clone(),
			slot,
			cursor: self.cursor,
			stats: self.stats.clone(),
		}
	}
}

impl Drop for Consumer {
	fn drop(&mut self) {
		if let Some(slot) = self.slot {
			let mut state = self.state.lock();
			state.priorities.remove(slot);
			// Nobody waits for a request no handler accepted yet: withdraw it, so a handler
			// that stops polling never holds requests nobody reads.
			if state.priorities.is_empty() && state.info.is_none() {
				state.end(Err(Error::Cancel));
			}
			// The last reader of a finished track leaves what it never read.
			if state.priorities.is_empty() && state.end.is_some() {
				state.release();
			}
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn recv(consumer: &mut Consumer) -> Poll<Result<Option<Datagram>>> {
		consumer.poll_recv(&kio::Waiter::noop())
	}

	fn sequence(consumer: &mut Consumer) -> u64 {
		match recv(consumer) {
			Poll::Ready(Ok(Some(datagram))) => datagram.sequence,
			other => panic!("expected a datagram, got {other:?}"),
		}
	}

	#[test]
	fn append_counts_its_own_sequence_past_inserts() {
		let mut producer = Info::default().produce("dg");
		let mut consumer = producer.consume();
		assert_eq!(producer.append(None, "a").unwrap(), 0);
		producer
			.insert(Datagram {
				sequence: 10,
				timestamp: None,
				payload: Bytes::from_static(b"b"),
			})
			.unwrap();
		// An older one is still delivered, and moves nothing back.
		producer
			.insert(Datagram {
				sequence: 3,
				timestamp: None,
				payload: Bytes::from_static(b"c"),
			})
			.unwrap();
		assert_eq!(producer.append(None, "d").unwrap(), 11);
		let got: Vec<u64> = (0..4).map(|_| sequence(&mut consumer)).collect();
		assert_eq!(got, [0, 10, 3, 11]);
		assert!(recv(&mut consumer).is_pending());
	}

	/// Past `u64::MAX` no sequence remains, so `append` refuses instead of repeating it.
	#[test]
	fn append_refuses_once_the_sequence_is_spent() {
		let mut producer = Info::default().produce("dg");
		producer
			.insert(Datagram {
				sequence: u64::MAX,
				timestamp: None,
				payload: Bytes::from_static(b"last"),
			})
			.unwrap();
		assert!(matches!(producer.append(None, "x"), Err(Error::BoundsExceeded(_))));
	}

	#[test]
	fn a_consumer_starts_live() {
		let mut producer = Info::default().produce("dg");
		producer.append(None, "old").unwrap();
		let mut consumer = producer.consume();
		assert!(recv(&mut consumer).is_pending());
		producer.append(None, "new").unwrap();
		assert_eq!(sequence(&mut consumer), 1);
	}

	#[test]
	fn a_slow_consumer_skips_to_the_oldest_held() {
		let mut producer = Info::default().produce("dg");
		let mut consumer = producer.consume();
		for _ in 0..MAX_BUFFERED + 5 {
			producer.append(None, "x").unwrap();
		}
		assert_eq!(sequence(&mut consumer), 5);
	}

	#[test]
	fn a_slow_consumer_holds_a_bounded_number_of_bytes() {
		let mut producer = Info::default().produce("dg");
		let mut consumer = producer.consume();
		let payload = Bytes::from(vec![0; MAX_BUFFERED_BYTES / 4]);
		for _ in 0..8 {
			producer.append(None, payload.clone()).unwrap();
		}
		// Four fit, so the oldest four went.
		assert_eq!(sequence(&mut consumer), 4);
		// The newest is always held, even alone over the budget.
		let mut producer = Info::default().produce("dg");
		let mut consumer = producer.consume();
		for _ in 0..2 {
			producer.append(None, vec![0; MAX_PAYLOAD]).unwrap();
		}
		assert_eq!(sequence(&mut consumer), 1);
	}

	#[test]
	fn timestamps_must_match_the_track() {
		let mut timed = Info::default().with_timescale(Timescale::MILLI).produce("dg");
		let mut consumer = timed.consume();
		assert!(matches!(timed.append(None, "x"), Err(Error::TimestampMismatch)));
		timed.append(Timestamp::from_secs(1).unwrap(), "x").unwrap();
		let Poll::Ready(Ok(Some(datagram))) = recv(&mut consumer) else {
			panic!("expected a datagram");
		};
		// Converted into the track's scale.
		assert_eq!(datagram.timestamp, Some(Timestamp::from_millis(1000).unwrap()));
		let mut untimed = Info::default().produce("dg");
		assert!(matches!(
			untimed.append(Timestamp::from_secs(1).unwrap(), "x"),
			Err(Error::TimestampMismatch)
		));
	}

	#[test]
	fn the_end_reaches_consumers() {
		let mut producer = Info::default().produce("dg");
		let mut finished = producer.consume();
		producer.clone().finish();
		assert!(matches!(recv(&mut finished), Poll::Ready(Ok(None))));
		assert!(matches!(producer.append(None, "x"), Err(Error::Closed)));

		let producer = Info::default().produce("dg");
		let mut dropped = producer.consume();
		drop(producer);
		assert!(matches!(recv(&mut dropped), Poll::Ready(Err(Error::Dropped))));
	}

	#[test]
	fn a_finish_delivers_what_is_buffered() {
		let mut producer = Info::default().produce("dg");
		let mut consumer = producer.consume();
		producer.append(None, "a").unwrap();
		producer.append(None, "b").unwrap();
		producer.finish();
		assert_eq!(sequence(&mut consumer), 0);
		assert_eq!(sequence(&mut consumer), 1);
		assert!(matches!(recv(&mut consumer), Poll::Ready(Ok(None))));

		// A consumer arriving after the finish never reads them.
		let mut producer = Info::default().produce("dg");
		let _reading = producer.consume();
		producer.append(None, "a").unwrap();
		producer.clone().finish();
		let mut late = producer.consume();
		assert!(matches!(recv(&mut late), Poll::Ready(Ok(None))));

		// Nobody left to read them, they go: on a finish with no consumer, or with the last
		// consumer of a finished track.
		let mut producer = Info::default().produce("dg");
		producer.append(None, "a").unwrap();
		producer.clone().finish();
		assert!(producer.state.read().buffer.is_empty());
		let mut producer = Info::default().produce("dg");
		let reading = producer.consume();
		producer.append(None, "a").unwrap();
		producer.clone().finish();
		assert_eq!(producer.state.read().buffer.len(), 1);
		drop(reading);
		assert!(producer.state.read().buffer.is_empty());
		assert_eq!(producer.state.read().bytes, 0);

		// An abort drops them.
		let mut producer = Info::default().produce("dg");
		let mut consumer = producer.consume();
		producer.append(None, "a").unwrap();
		producer.abort(Error::Cancel);
		assert!(matches!(recv(&mut consumer), Poll::Ready(Err(Error::Cancel))));
	}

	#[test]
	fn the_publisher_sees_the_highest_priority() {
		let producer = Info::default().produce("dg");
		assert_eq!(producer.priority(), None);
		let mut low = producer.consume();
		let mut high = low.clone();
		high.set_priority(5);
		low.set_priority(-3);
		assert_eq!(producer.priority(), Some(5));
		assert!(producer.poll_priority(&kio::Waiter::noop(), Some(5)).is_pending());
		drop(high);
		assert_eq!(
			producer.poll_priority(&kio::Waiter::noop(), Some(5)),
			Poll::Ready(Some(-3))
		);
		assert!(!producer.close_unused());
		drop(low);
		assert!(producer.close_unused());
	}

	#[test]
	fn a_request_resolves_its_consumers() {
		let request = Request::new("dg".into());
		let consumer = request.weak().try_consume(stats::Scope::default()).unwrap();
		assert!(consumer.poll_info(&kio::Waiter::noop()).is_pending());
		let info = Info::default().with_priority(3);
		let _producer = request.accept(info);
		assert!(matches!(consumer.poll_info(&kio::Waiter::noop()), Poll::Ready(Ok(got)) if got == info));

		let request = Request::new("dg".into());
		let mut consumer = request.weak().try_consume(stats::Scope::default()).unwrap();
		request.reject(Error::NotFound);
		assert!(matches!(recv(&mut consumer), Poll::Ready(Err(Error::NotFound))));
		assert!(matches!(
			consumer.poll_info(&kio::Waiter::noop()),
			Poll::Ready(Err(Error::NotFound))
		));
	}
}
