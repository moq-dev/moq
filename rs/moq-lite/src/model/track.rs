//! A track is a collection of semi-reliable and semi-ordered streams, split into a [TrackProducer] and [TrackConsumer] handle.
//!
//! A [TrackProducer] creates streams with a sequence number and priority.
//! The sequest number is used to determine the order of streams, while the priority is used to determine which stream to transmit first.
//! This may seem counter-intuitive, but is designed for live streaming where the newest streams may be higher priority.
//! A cloned [TrackProducer] can be used to create streams in parallel, but will error if a duplicate sequence number is used.
//!
//! A [TrackConsumer] may not receive all streams in order or at all.
//! These streams are meant to be transmitted over congested networks and the key to MoQ Tranport is to not block on them.
//! streams will be cached for a potentially limited duration added to the unreliable nature.
//! A cloned [TrackConsumer] will receive a copy of all new stream going forward (fanout).
//!
//! The track is closed with [Error] when all writers or readers are dropped.

use crate::{Error, Result};

use super::state::{Consumer, Producer, Weak};
use super::waiter::waiter_fn;
use super::{Group, GroupConsumer, GroupProducer};

use std::{
	collections::{HashSet, VecDeque},
	task::Poll,
};

/// A track is a collection of groups, delivered out-of-order until expired.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Track {
	pub name: String,
	pub priority: u8,
}

impl Track {
	pub fn new<T: Into<String>>(name: T) -> Self {
		Self {
			name: name.into(),
			priority: 0,
		}
	}

	pub fn produce(self) -> TrackProducer {
		TrackProducer::new(self)
	}
}

#[derive(Default)]
struct State {
	groups: VecDeque<GroupProducer>,
	duplicates: HashSet<u64>,
	offset: usize,
	max_sequence: Option<u64>,
	fin: bool,
}

impl State {
	fn poll_next_group(&self, index: usize) -> Poll<Option<GroupProducer>> {
		let relative = index.saturating_sub(self.offset);
		if let Some(group) = self.groups.get(relative) {
			Poll::Ready(Some(group.clone()))
		} else if self.fin {
			Poll::Ready(None)
		} else {
			Poll::Pending
		}
	}

	fn poll_get_group(&self, sequence: u64) -> Poll<Option<GroupProducer>> {
		// Search for the group with the matching sequence.
		if let Some(group) = self.groups.iter().find(|g| g.info.sequence == sequence) {
			return Poll::Ready(Some(group.clone()));
		}

		// If we've already seen a newer sequence, the group is gone.
		if let Some(max) = self.max_sequence
			&& max >= sequence
		{
			return Poll::Ready(None);
		}

		if self.fin {
			return Poll::Ready(None);
		}

		Poll::Pending
	}
}

/// A producer for a track, used to create new groups.
pub struct TrackProducer {
	pub info: Track,
	state: Producer<State>,
}

impl TrackProducer {
	pub fn new(info: Track) -> Self {
		Self {
			info,
			state: Producer::default(),
		}
	}

	/// Create a new group with the given sequence number.
	pub fn create_group(&mut self, info: Group) -> Result<GroupProducer> {
		let group = info.produce();

		let mut state = self.state.modify()?;
		if state.fin && group.info.sequence >= state.max_sequence.unwrap_or(0) {
			return Err(Error::Closed);
		}

		if !state.duplicates.insert(group.info.sequence) {
			return Err(Error::Duplicate);
		}

		state.max_sequence = Some(state.max_sequence.unwrap_or(0).max(group.info.sequence));
		state.groups.push_back(group.clone());

		Ok(group)
	}

	/// Create a new group with the next sequence number.
	pub fn append_group(&mut self) -> Result<GroupProducer> {
		let mut state = self.state.modify()?;
		if state.fin {
			return Err(Error::Closed);
		}

		let sequence = state.max_sequence.map_or(0, |s| s + 1);
		let group = Group { sequence }.produce();

		state.duplicates.insert(sequence);
		state.max_sequence = Some(sequence);
		state.groups.push_back(group.clone());

		Ok(group)
	}

	/// Create a group with a single frame.
	pub fn write_frame<B: Into<bytes::Bytes>>(&mut self, frame: B) -> Result<()> {
		let mut group = self.append_group()?;
		group.write_frame(frame.into())?;
		group.finish()?;
		Ok(())
	}

	/// Mark the last group of the track.
	///
	/// NOTE: The track is not closed yet; old groups can still arrive.
	pub fn finish(&mut self) -> Result<()> {
		let mut state = self.state.modify()?;
		state.fin = true;
		Ok(())
	}

	/// Abort the track with the given error.
	pub fn close(&mut self, err: Error) -> Result<()> {
		let mut state = self.state.modify()?;

		// Abort all groups still in progress.
		for group in state.groups.iter_mut() {
			// Ignore errors, we don't care if the group was already closed.
			group.close(err.clone()).ok();
		}

		state.close(err);
		Ok(())
	}

	/// Create a new consumer for the track, starting at the latest group.
	pub fn consume(&self) -> TrackConsumer {
		let state = self.state.borrow();
		let index = state.offset + state.groups.len().saturating_sub(1);

		TrackConsumer {
			info: self.info.clone(),
			state: self.state.consume(),
			index,
		}
	}

	/// Block until there are no active consumers.
	pub async fn unused(&self) -> Result<()> {
		self.state.unused().await
	}

	/// Return true if the track has been closed.
	pub fn is_closed(&self) -> bool {
		self.state.borrow().is_closed()
	}

	/// Return true if this is the same track.
	pub fn is_clone(&self, other: &Self) -> bool {
		self.state.is_clone(&other.state)
	}

	/// Create a weak reference that doesn't prevent auto-close.
	pub(crate) fn weak(&self) -> TrackWeak {
		TrackWeak {
			info: self.info.clone(),
			state: self.state.weak(),
		}
	}
}

impl Clone for TrackProducer {
	fn clone(&self) -> Self {
		Self {
			info: self.info.clone(),
			state: self.state.clone(),
		}
	}
}

impl From<Track> for TrackProducer {
	fn from(info: Track) -> Self {
		TrackProducer::new(info)
	}
}

/// A weak reference to a track that doesn't prevent auto-close.
#[derive(Clone)]
pub(crate) struct TrackWeak {
	pub info: Track,
	state: Weak<State>,
}

impl TrackWeak {
	pub fn is_closed(&self) -> bool {
		self.state.is_closed()
	}

	pub fn consume(&self) -> TrackConsumer {
		let state = self.state.borrow();
		let index = state.offset + state.groups.len().saturating_sub(1);

		TrackConsumer {
			info: self.info.clone(),
			state: self.state.consume(),
			index,
		}
	}

	pub async fn unused(&self) -> crate::Result<()> {
		self.state.unused().await
	}

	pub fn is_clone(&self, other: &Self) -> bool {
		self.state.is_clone(&other.state)
	}
}

/// A consumer for a track, used to read groups.
#[derive(Clone)]
pub struct TrackConsumer {
	pub info: Track,
	state: Consumer<State>,
	index: usize,
}

impl TrackConsumer {
	/// Return the next group in order.
	///
	/// NOTE: This can have gaps if the reader is too slow or there were network slowdowns.
	pub async fn next_group(&mut self) -> Result<Option<GroupConsumer>> {
		let index = self.index;
		let res = waiter_fn(|waiter| self.state.poll(waiter, |state| state.poll_next_group(index))).await?;
		let consumer = res.map(|producer| {
			self.index += 1;
			producer.consume()
		});
		Ok(consumer)
	}

	/// Block until the group with the given sequence is available.
	///
	/// Returns None if the group is not in the cache and a newer group exists.
	pub async fn get_group(&self, sequence: u64) -> Result<Option<GroupConsumer>> {
		let res = waiter_fn(|waiter| self.state.poll(waiter, |state| state.poll_get_group(sequence))).await?;
		Ok(res.map(|producer| producer.consume()))
	}

	/// Block until the track is closed.
	pub async fn closed(&self) -> Result<()> {
		let err = self.state.closed().await;
		match err {
			Error::Closed | Error::Dropped => Ok(()),
			err => Err(err),
		}
	}

	pub fn is_clone(&self, other: &Self) -> bool {
		self.state.is_clone(&other.state)
	}
}

#[cfg(test)]
use futures::FutureExt;

#[cfg(test)]
impl TrackConsumer {
	pub fn assert_group(&mut self) -> GroupConsumer {
		self.next_group()
			.now_or_never()
			.expect("group would have blocked")
			.expect("would have errored")
			.expect("track was closed")
	}

	pub fn assert_no_group(&mut self) {
		assert!(
			self.next_group().now_or_never().is_none(),
			"next group would not have blocked"
		);
	}

	pub fn assert_not_closed(&self) {
		assert!(self.closed().now_or_never().is_none(), "should not be closed");
	}

	pub fn assert_closed(&self) {
		assert!(self.closed().now_or_never().is_some(), "should be closed");
	}

	// TODO assert specific errors after implementing PartialEq
	pub fn assert_error(&self) {
		assert!(
			self.closed().now_or_never().expect("should not block").is_err(),
			"should be error"
		);
	}

	pub fn assert_is_clone(&self, other: &Self) {
		assert!(self.is_clone(other), "should be clone");
	}

	pub fn assert_not_clone(&self, other: &Self) {
		assert!(!self.is_clone(other), "should not be clone");
	}
}
