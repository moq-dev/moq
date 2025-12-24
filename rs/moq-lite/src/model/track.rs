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

use futures::StreamExt;
use tokio::{sync::watch, time::Instant};
use web_async::FuturesExt;

use crate::{Error, Result};

use super::{Group, GroupConsumer, GroupProducer};

use std::{collections::VecDeque, fmt, future::Future, ops::Deref, sync::Arc};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Track {
	/// The name of the track.
	pub name: String,

	/// Higher priority tracks will be served first during congestion.
	pub priority: u8,

	/// Groups will be dropped if they are this much older than the latest group.
	pub max_latency: std::time::Duration,
}

impl Track {
	pub fn new(name: &str) -> Self {
		Self {
			name: name.to_string(),
			priority: 0,
			max_latency: std::time::Duration::ZERO,
		}
	}
}

#[derive(Debug, Default, Clone, Copy)]
pub struct TrackMeta {
	/// Higher priority tracks will be served first during congestion.
	pub priority: u8,

	/// Groups will be dropped if they are this much older than the latest group.
	pub max_latency: std::time::Duration,
}

#[derive(Debug, Default)]
struct TrackState {
	// Metadata about the track.
	meta: TrackMeta,

	// Groups in order of arrival.
	// If None, the group has expired but was not in the front of the queue.
	groups: VecDeque<Option<GroupState>>,

	// +1 every time we remove a group from the front.
	offset: usize,

	// The highest sequence number received, and when.
	max: Option<(u64, Instant)>,

	// Some if the track is closed
	closed: Option<Result<()>>,
}

impl TrackState {
	pub fn new(track: &Track) -> Self {
		Self {
			meta: TrackMeta {
				priority: track.priority,
				max_latency: track.max_latency,
			},
			groups: VecDeque::new(),
			offset: 0,
			max: None,
			closed: None,
		}
	}
}

#[derive(Debug)]
struct GroupState {
	// We need a producer in order to abort on expired/close.
	producer: GroupProducer,

	// If we didn't hold a consumer, `unused()` would be true.
	consumer: GroupConsumer,

	// TODO We should use timestamps on a per-track basis, instead of wall clock time.
	when: Instant,
}

/// A producer for a track, used to create new groups.
#[derive(Clone)]
pub struct TrackProducer {
	name: Arc<String>, // can't change, cheap to clone
	state: watch::Sender<TrackState>,
}

impl TrackProducer {
	pub fn new(info: Track) -> Self {
		let this = Self {
			state: watch::Sender::new(TrackState::new(&info)),
			name: Arc::new(info.name),
		};
		web_async::spawn(this.clone().run_expires());
		this
	}

	pub fn name(&self) -> &str {
		&self.name
	}

	/// Create a new group with the given info.
	///
	/// Returns an error if the track is closed.
	pub fn create_group(&mut self, info: Group) -> Result<GroupProducer> {
		let group = GroupProducer::new(info);
		let mut result = Err(Error::Cancel);

		self.state.send_if_modified(|state| {
			if let Some(closed) = state.closed.clone() {
				result = Err(closed.err().unwrap_or(Error::Cancel));
				return false;
			}

			// As a sanity check, make sure this is not a duplicate.
			if state
				.groups
				.iter()
				.filter_map(|g| g.as_ref())
				.any(|g| g.producer.sequence == group.sequence)
			{
				result = Err(Error::Duplicate);
				return false;
			}

			let now = if group.sequence >= state.max.map(|m| m.0).unwrap_or(0) {
				let now = Instant::now();
				state.max = Some((group.sequence, now));
				now
			} else if state.meta.max_latency.is_zero() {
				// Guaranteed to expire, we don't even need to call Instant::now
				result = Err(Error::Expired);
				return false;
			} else {
				// Optimization: Check if this group should have expired by now.
				// We avoid inserting and creating groups that would be instantly expired.
				let max = state.max.expect("impossible").1;

				let now = Instant::now();
				if now - max > state.meta.max_latency {
					result = Err(Error::Expired);
					return false;
				}

				now
			};

			result = Ok(group.clone());

			let group = GroupState {
				consumer: group.consume(),
				producer: group,
				when: now,
			};

			state.groups.push_back(Some(group));

			true
		});

		result
	}

	/// Create a new group with the next sequence number.
	pub fn append_group(&mut self) -> Result<GroupProducer> {
		let mut result = Err(Error::Cancel);

		self.state.send_if_modified(|state| {
			if let Some(closed) = state.closed.clone() {
				result = Err(closed.err().unwrap_or(Error::Cancel));
				return false;
			}

			let sequence = match state.max {
				Some((sequence, _)) => sequence + 1,
				None => 0,
			};

			let group = GroupProducer::new(Group { sequence });
			result = Ok(group.clone());

			let now = Instant::now();

			let group = GroupState {
				consumer: group.consume(),
				producer: group,
				when: now,
			};

			state.max = Some((sequence, now));
			state.groups.push_back(Some(group));

			true
		});

		result
	}

	pub fn close(&mut self) -> Result<()> {
		let mut result = Ok(());

		self.state.send_if_modified(|state| {
			if let Some(closed) = state.closed.clone() {
				result = Err(closed.err().unwrap_or(Error::Cancel));
				return false;
			}

			state.closed = Some(Ok(()));
			true
		});

		result
	}

	pub fn abort(&mut self, err: Error) -> Result<()> {
		let mut result = Ok(());

		self.state.send_if_modified(|state| {
			if let Some(Err(closed)) = state.closed.clone() {
				result = Err(closed);
				return false;
			}

			state.closed = Some(Err(err));

			true
		});

		result
	}

	/// Create a new consumer for the track.
	pub fn consume(&self) -> TrackConsumer {
		TrackConsumer {
			name: self.name.clone(),
			state: self.state.subscribe(),
			index: 0,
		}
	}

	/// Block until there are no active consumers.
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

	async fn run_expires(self) {
		let mut receiver = self.state.subscribe();

		loop {
			let (expires, closed) = {
				let state = receiver.borrow();

				// This monster of an iterator:
				// - only looks at existing groups older than the max sequence
				// - finds the group with the minimum arrival timestamp
				// - returns the index and arrival timestamp of the group to expire
				//
				// We sleep by that amount, and if we wake up, remove that index.
				let next = state
					.groups
					.iter()
					.enumerate()
					.filter_map(|(index, group)| {
						group
							.as_ref()
							.map(|g| (index, g.producer.sequence, g.when + state.meta.max_latency))
					})
					.filter(|(_index, sequence, _expires)| *sequence < state.max.map(|m| m.0).unwrap_or(0))
					.min_by_key(|(_index, _sequence, expires)| *expires);

				let expires = if let Some((index, _sequence, expires)) = next {
					if state.meta.max_latency.is_zero() || !expires.elapsed().is_zero() {
						self.state.send_if_modified(|state| {
							let mut group = state
								.groups
								.get_mut(index)
								.unwrap()
								.take()
								.expect("group must have been Some");
							group.producer.abort(Error::Expired).ok();

							while state.groups.front().is_none() {
								state.groups.pop_front();
								state.offset += 1;
							}

							// Don't notify anybody; we're just cleaning up.
							false
						});
						continue;
					}

					Some(expires)
				} else {
					None
				};

				(expires, state.closed.is_some())
			};

			if closed && expires.is_none() {
				// No more groups to expire, only the last one is left.
				break;
			}

			tokio::select! {
				// Sleep until the next group expires.
				Some(()) = async { Some(tokio::time::sleep_until(expires?).await) } => {},
				_ = receiver.changed() => {},
			};
		}
	}

	pub fn meta(&self) -> TrackMeta {
		self.state.borrow().meta
	}

	pub fn set_meta(&mut self, meta: TrackMeta) {
		self.state.send_modify(|state| {
			state.meta = meta;
		});
	}
}

impl From<Track> for TrackProducer {
	fn from(info: Track) -> Self {
		TrackProducer::new(info)
	}
}

impl Drop for TrackProducer {
	fn drop(&mut self) {
		// +1 because of run_expires
		if self.state.sender_count() > 2 {
			return;
		}

		self.state.send_if_modified(|state| {
			if state.closed.is_some() {
				return false;
			}

			state.closed = Some(Err(Error::Cancel));
			true
		});
	}
}

/// A consumer for a track, used to read groups.
///
/// If the consumer is cloned, it will receive a copy of all unread groups.
#[derive(Clone)]
pub struct TrackConsumer {
	name: Arc<String>,

	state: watch::Receiver<TrackState>,

	// We last returned this group, factoring in offset
	index: usize,
}

impl fmt::Debug for TrackConsumer {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("TrackConsumer")
			.field("name", &self.name)
			.field("state", &self.state.borrow().deref())
			.field("index", &self.index)
			.finish()
	}
}

impl TrackConsumer {
	pub fn name(&self) -> &str {
		&self.name
	}

	pub fn meta(&self) -> TrackMeta {
		self.state.borrow().meta
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
	pub async fn closed(&self) -> Result<()> {
		match self.state.clone().wait_for(|state| state.closed.is_some()).await {
			Ok(state) => state.closed.clone().unwrap(),
			Err(_) => Err(Error::Cancel),
		}
	}

	pub fn is_clone(&self, other: &Self) -> bool {
		self.state.same_channel(&other.state)
	}

	/// Proxy all groups and errors to the given producer.
	///
	/// Returns an error on any unexpected close, which can happen if the [TrackProducer] is cloned.
	pub async fn proxy(mut self, mut dst: TrackProducer) -> Result<()> {
		let mut tasks = futures::stream::FuturesUnordered::new();

		loop {
			tokio::select! {
				biased;
				Some(res) = self.next_group().transpose() => match res {
					Ok(group) => {
						let dst = dst.create_group(group.info().clone())?;
						tasks.push(group.proxy(dst));
					}
					Err(err) => return dst.abort(err),
				},
				// Wait until all groups have finished being proxied.
				Some(res) = tasks.next() => {
					if let Err(err) = res {
						tracing::warn!(?err, "proxy track");
					}
				},
				// We're done with the proxy.
				else => return Ok(()),
			}
		}
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
