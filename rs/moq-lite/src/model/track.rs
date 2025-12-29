//! A track is a collection of semi-reliable and semi-ordered streams, split into a [TrackProducer] and [TrackConsumer] handle.
//!
//! A [TrackProducer] creates streams with a sequence number and priority.
//! The sequest number is used to determine the order of streams, while the priority is used to determine which stream to transmit first.
//! This may seem counter-intuitive, but is designed for live streaming where the newest streams may be higher priority.
//! A cloned [Producer] can be used to create streams in parallel, but will error if a duplicate sequence number is used.
//!
//! A [TrackConsumer] may not receive all streams in order or at all.
//! These streams are meant to be transmitted over congested networks and the key to MoQ Tranport is to not block on them.
//! streams will be cached for a potentially limited duration added to the unreliable nature.
//! A cloned [Consumer] will receive a copy of all new stream going forward (fanout).
//!
//! The track is closed with [Error] when all writers or readers are dropped.

use tokio::sync::watch;

use super::{Group, GroupConsumer, GroupProducer, Time};
use crate::{Error, ExpiresProducer, Produce, Result, TrackMeta, TrackMetaConsumer, TrackMetaProducer};

use std::{collections::VecDeque, fmt, future::Future, ops::Deref, sync::Arc};

#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Track {
	/// The name of the track.
	pub name: String,

	/// Higher priority tracks will be served first during congestion.
	pub priority: u8,

	/// Groups will be dropped if they are this much older than the latest group.
	pub max_latency: Time,
}

impl Track {
	pub fn new(name: &str) -> Self {
		Self {
			name: name.to_string(),
			priority: 0,
			max_latency: Time::ZERO,
		}
	}

	pub fn produce(self) -> Produce<TrackProducer, TrackConsumer> {
		let producer = TrackProducer::new(self.clone());
		Produce {
			consumer: producer.consume(),
			producer,
		}
	}
}

impl<T: AsRef<str>> From<T> for Track {
	fn from(name: T) -> Self {
		Self::new(name.as_ref())
	}
}

/// Static information about a track
///
/// Only used to make accessing the name easy/fast.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrackInfo {
	pub name: Arc<String>,
}

#[derive(Debug, Default)]
struct State {
	// Groups in order of arrival.
	// If None, the group has expired but was not in the front of the queue.
	groups: VecDeque<Option<Produce<GroupProducer, GroupConsumer>>>,

	// +1 every time we remove a group from the front.
	offset: usize,

	// Some if the track is closed
	closed: Option<Result<()>>,

	// The highest sequence number received.
	max: Option<u64>,
}

impl State {
	fn create_group(&mut self, info: Group, expires: ExpiresProducer) -> Result<GroupProducer> {
		if let Some(closed) = self.closed.clone() {
			return Err(closed.err().unwrap_or(Error::Cancel));
		}

		let group = GroupProducer::new_expires(info.clone(), expires);

		// As a sanity check, make sure this is not a duplicate.
		if self
			.groups
			.iter()
			.filter_map(|g| g.as_ref())
			.any(|g| g.producer.sequence == group.sequence)
		{
			return Err(Error::Duplicate);
		}

		self.max = Some(self.max.unwrap_or_default().max(group.sequence));

		self.groups.push_back(Some(Produce {
			consumer: group.consume(),
			producer: group.clone(),
		}));

		Ok(group)
	}

	fn append_group(&mut self, expires: ExpiresProducer) -> Result<GroupProducer> {
		if let Some(closed) = self.closed.clone() {
			return Err(closed.err().unwrap_or(Error::Cancel));
		}

		let sequence = match self.max {
			Some(sequence) => sequence + 1,
			None => 0,
		};
		self.max = Some(sequence);

		let group = GroupProducer::new_expires(Group { sequence }, expires);

		self.groups.push_back(Some(Produce {
			consumer: group.consume(),
			producer: group.clone(),
		}));

		Ok(group)
	}

	fn abort(&mut self, err: Error) -> Result<()> {
		if let Some(Err(err)) = self.closed.clone() {
			return Err(err);
		}

		self.closed = Some(Err(err));

		Ok(())
	}

	fn close(&mut self) -> Result<()> {
		if let Some(closed) = self.closed.clone() {
			return Err(closed.err().unwrap_or(Error::Cancel));
		}

		self.closed = Some(Ok(()));

		Ok(())
	}
}

/// A producer for a track, used to create new groups.
#[derive(Clone)]
pub struct TrackProducer {
	info: TrackInfo,
	state: watch::Sender<State>,
	meta: Produce<TrackMetaProducer, TrackMetaConsumer>,
	expires: ExpiresProducer,
}

impl fmt::Debug for TrackProducer {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("TrackProducer")
			.field("info", &self.info)
			.field("state", &self.state.borrow().deref())
			.field("meta", &self.meta.consumer)
			.finish()
	}
}

impl TrackProducer {
	pub fn new<T: Into<Track>>(info: T) -> Self {
		let info = info.into();

		let expires = ExpiresProducer::new(info.max_latency);

		let meta = TrackMetaProducer::new_expires(
			TrackMeta {
				priority: info.priority,
				max_latency: info.max_latency,
			},
			expires.clone(),
		);

		Self {
			state: Default::default(),
			meta: Produce {
				consumer: meta.consume(),
				producer: meta,
			},
			info: TrackInfo {
				name: Arc::new(info.name),
			},
			expires,
		}
	}

	pub fn info(&self) -> TrackInfo {
		self.info.clone()
	}

	// Information about all of the consumers of this track.
	pub fn meta(&self) -> TrackMetaConsumer {
		self.meta.consumer.clone()
	}

	/// Create a new [GroupProducer] with the given info.
	///
	/// Returns an error if the track is closed.
	pub fn create_group<T: Into<Group>>(&mut self, info: T) -> Result<GroupProducer> {
		let mut result = Err(Error::Cancel);

		self.state.send_if_modified(|state| {
			result = state.create_group(info.into(), self.expires.clone());
			result.is_ok()
		});

		result
	}

	/// Create a new group with the next sequence number.
	pub fn append_group(&mut self) -> Result<GroupProducer> {
		let mut result = Err(Error::Cancel);

		self.state.send_if_modified(|state| {
			result = state.append_group(self.expires.clone());
			result.is_ok()
		});

		result
	}

	pub fn close(&mut self) -> Result<()> {
		let mut result = Ok(());

		self.state.send_if_modified(|state| {
			result = state.close();
			result.is_ok()
		});

		result
	}

	pub fn abort(&mut self, err: Error) -> Result<()> {
		let mut result = Ok(());

		self.state.send_if_modified(|state| {
			result = state.abort(err);
			result.is_ok()
		});

		result
	}

	/// Create a new consumer for the track.
	pub fn consume(&self) -> TrackConsumer {
		TrackConsumer {
			info: self.info.clone(),
			state: self.state.subscribe(),
			meta: self.meta.producer.clone(),
			index: 0,
		}
	}

	/// Block until there are no active consumers.
	// We don't use the `async` keyword so we don't borrow &self across the await.
	pub fn unused(&self) -> impl Future<Output = ()> {
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

impl Deref for TrackProducer {
	type Target = TrackInfo;

	fn deref(&self) -> &Self::Target {
		&self.info
	}
}

/// A consumer for a track, used to read groups.
///
/// If the consumer is cloned, it will receive a copy of all unread groups.
#[derive(Clone)]
pub struct TrackConsumer {
	info: TrackInfo,

	state: watch::Receiver<State>,
	meta: TrackMetaProducer,

	// We last returned this group, factoring in offset
	index: usize,
}

impl fmt::Debug for TrackConsumer {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("TrackConsumer")
			.field("name", &self.name)
			.field("state", &self.state.borrow().deref())
			.field("meta", &self.meta)
			.field("index", &self.index)
			.finish()
	}
}

impl TrackConsumer {
	pub fn info(&self) -> TrackInfo {
		self.info.clone()
	}

	/// Return the next group in order.
	///
	/// NOTE: This can have gaps if the reader is too slow or there were network slowdowns.
	pub async fn next_group(&mut self) -> Result<Option<GroupConsumer>> {
		loop {
			// Wait until there's a new latest group or the track is closed.
			let state = self
				.state
				.wait_for(|state| state.closed.is_some() || self.index < state.offset + state.groups.len())
				.await
				.map_err(|_| Error::Cancel)?;

			for i in self.index.saturating_sub(state.offset)..state.groups.len() {
				// If None, the group has expired out of order.
				if let Some(group) = &state.groups[i] {
					self.index = state.offset + i + 1;
					return Ok(Some(group.consumer.clone()));
				}
			}

			match &state.closed {
				Some(Ok(_)) => return Ok(None),
				Some(Err(err)) => return Err(err.clone()),
				// There must have been a new None group
				// This can happen if an immediately expired group is received, or just a race.
				_ => {}
			}
		}
	}

	/// Block until the track is closed.
	pub fn closed(&self) -> impl Future<Output = Result<()>> {
		let mut state = self.state.clone();

		async move {
			match state.wait_for(|state| state.closed.is_some()).await {
				Ok(state) => state.closed.clone().unwrap(),
				Err(_) => Err(Error::Cancel),
			}
		}
	}

	pub fn is_clone(&self, other: &Self) -> bool {
		self.state.same_channel(&other.state)
	}

	pub fn meta(&mut self) -> TrackMetaProducer {
		self.meta.clone()
	}
}

impl Deref for TrackConsumer {
	type Target = TrackInfo;

	fn deref(&self) -> &Self::Target {
		&self.info
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
