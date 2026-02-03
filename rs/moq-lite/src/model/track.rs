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

use tokio::sync::watch;

use crate::{Error, Result};

use super::{Group, GroupConsumer, GroupProducer};

use std::{collections::VecDeque, future::Future};

const MAX_CACHE: std::time::Duration = std::time::Duration::from_secs(30);

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
struct TrackState {
	groups: VecDeque<(tokio::time::Instant, GroupConsumer)>,
	closed: Option<Result<()>>,
	offset: usize,

	max_sequence: Option<u64>,

	// The largest sequence number that has been dropped.
	drop_sequence: Option<u64>,
}

impl TrackState {
	fn trim(&mut self, now: tokio::time::Instant) {
		while let Some((timestamp, _)) = self.groups.front() {
			if now.saturating_duration_since(*timestamp) > MAX_CACHE {
				let (_, group) = self.groups.pop_front().unwrap();
				self.drop_sequence = Some(self.drop_sequence.unwrap_or(0).max(group.info.sequence));
				self.offset += 1;
			} else {
				break;
			}
		}
	}
}

/// A producer for a track, used to create new groups.
#[derive(Clone)]
pub struct TrackProducer {
	pub info: Track,
	state: watch::Sender<TrackState>,
}

impl TrackProducer {
	pub fn new(info: Track) -> Self {
		Self {
			info,
			state: Default::default(),
		}
	}

	/// Insert a group into the track, returning true if this is the latest group.
	pub fn insert_group(&mut self, group: GroupConsumer) -> bool {
		self.state.send_if_modified(|state| {
			assert!(state.closed.is_none());
			let now = tokio::time::Instant::now();
			state.trim(now);
			state.groups.push_back((now, group.clone()));
			state.max_sequence = Some(state.max_sequence.unwrap_or(0).max(group.info.sequence));
			true
		})
	}

	/// Create a new group with the given sequence number.
	///
	/// If the sequence number is not the latest, this method will return None.
	pub fn create_group(&mut self, info: Group) -> Option<GroupProducer> {
		let group = info.produce();
		self.insert_group(group.consume()).then_some(group)
	}

	/// Create a new group with the next sequence number.
	pub fn append_group(&mut self) -> GroupProducer {
		let mut producer = None;

		self.state.send_if_modified(|state| {
			assert!(state.closed.is_none());

			let now = tokio::time::Instant::now();
			state.trim(now);

			let sequence = state.max_sequence.map_or(0, |sequence| sequence + 1);
			let group = Group { sequence }.produce();
			state.groups.push_back((now, group.consume()));
			state.max_sequence = Some(sequence);

			producer = Some(group);
			true
		});

		producer.unwrap()
	}

	/// Create a group with a single frame.
	pub fn write_frame<B: Into<bytes::Bytes>>(&mut self, frame: B) {
		let mut group = self.append_group();
		group.write_frame(frame.into());
		group.close();
	}

	pub fn close(self) {
		self.state.send_modify(|state| state.closed = Some(Ok(())));
	}

	pub fn abort(self, err: Error) {
		self.state.send_modify(|state| state.closed = Some(Err(err)));
	}

	/// Create a new consumer for the track.
	pub fn consume(&self) -> TrackConsumer {
		let state = self.state.borrow();
		TrackConsumer {
			info: self.info.clone(),
			state: self.state.subscribe(),
			// Start at the latest group
			index: state.offset + state.groups.len().saturating_sub(1),
		}
	}

	/// Block until there are no active consumers.
	pub fn unused(&self) -> impl Future<Output = ()> + use<> {
		let state = self.state.clone();
		async move {
			state.closed().await;
		}
	}

	/// Return true if this is the same track.
	pub fn is_clone(&self, other: &Self) -> bool {
		self.state.same_channel(&other.state)
	}
}

impl From<Track> for TrackProducer {
	fn from(info: Track) -> Self {
		TrackProducer::new(info)
	}
}

/// A consumer for a track, used to read groups.
#[derive(Clone)]
pub struct TrackConsumer {
	pub info: Track,
	state: watch::Receiver<TrackState>,
	index: usize,
}

impl TrackConsumer {
	/// Return the next group in order.
	///
	/// NOTE: This can have gaps if the reader is too slow or there were network slowdowns.
	pub async fn next_group(&mut self) -> Result<Option<GroupConsumer>> {
		// Wait until there's a new latest group or the track is closed.
		let Ok(state) = self
			.state
			.wait_for(|state| {
				let index = self.index.saturating_sub(state.offset);
				state.groups.get(index).is_some() || state.closed.is_some()
			})
			.await
		else {
			return Err(Error::Cancel);
		};

		let index = self.index.saturating_sub(state.offset);
		if let Some(group) = state.groups.get(index) {
			self.index = state.offset + index + 1;
			return Ok(Some(group.1.clone()));
		}

		match &state.closed {
			Some(Ok(_)) => Ok(None),
			Some(Err(err)) => Err(err.clone()),
			_ => unreachable!(),
		}
	}

	/// Block until the group is available.
	///
	/// NOTE: This can block indefinitely if the requested group is dropped.
	pub async fn get_group(&self, sequence: u64) -> Result<Option<GroupConsumer>> {
		let mut state = self.state.clone();

		let Ok(state) = state
			.wait_for(|state| {
				if state.closed.is_some() {
					return true;
				}

				if let Some(drop_sequence) = state.drop_sequence
					&& drop_sequence >= sequence
				{
					return true;
				}

				state.groups.iter().any(|(_, group)| group.info.sequence == sequence)
			})
			.await
		else {
			return Err(Error::Cancel);
		};

		if let Some((_, group)) = state.groups.iter().find(|(_, group)| group.info.sequence == sequence) {
			return Ok(Some(group.clone()));
		}

		match &state.closed {
			Some(Ok(_)) => Ok(None), // end of stream
			Some(Err(err)) => Err(err.clone()),
			None => Ok(None), // Dropped
		}
	}

	/// Block until the track is closed.
	pub async fn closed(&self) -> Result<()> {
		match self.state.clone().wait_for(|state| state.closed.is_some()).await {
			Ok(state) => state.closed.clone().unwrap(),
			Err(_) => Err(Error::Cancel),
		}
	}

	pub fn is_clone(&self, other: &Self) -> bool {
		self.state.same_channel(&other.state)
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
