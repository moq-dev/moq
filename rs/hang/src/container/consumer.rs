use std::collections::VecDeque;

use buf_list::BufList;
use futures::{StreamExt, stream::FuturesUnordered};
use moq_lite::coding::Decode;

use super::{Frame, Timestamp};
use crate::Error;

/// A consumer for hang-formatted media tracks with timestamp reordering.
///
/// This wraps a `moq_lite::TrackConsumer` and adds hang-specific functionality
/// like timestamp decoding, latency management, and frame buffering.
///
/// ## Latency Management
///
/// The consumer can skip groups that are too far behind to maintain low latency.
/// Configure the maximum acceptable delay through the consumer's latency settings.
pub struct OrderedConsumer {
	pub track: moq_lite::TrackConsumer,

	// The current group that we are reading from.
	current: Option<GroupReader>,

	// Future groups that we are monitoring, deciding based on [latency] whether to skip.
	pending: VecDeque<GroupReader>,

	// The maximum timestamp seen thus far, or zero because that's easier than None.
	max_timestamp: Timestamp,

	// The maximum buffer size before skipping a group.
	max_latency: std::time::Duration,
}

impl OrderedConsumer {
	/// Create a new OrderedConsumer wrapping the given moq-lite consumer.
	pub fn new(track: moq_lite::TrackConsumer, max_latency: std::time::Duration) -> Self {
		Self {
			track,
			current: None,
			pending: VecDeque::new(),
			max_timestamp: Timestamp::default(),
			max_latency,
		}
	}

	/// Read the next frame from the track.
	///
	/// This method handles timestamp decoding, group ordering, and latency management
	/// automatically. It will skip groups that are too far behind to maintain the
	/// configured latency target.
	///
	/// Returns `None` when the track has ended.
	pub async fn read(&mut self) -> Result<Option<Frame>, Error> {
		let latency = self.max_latency.try_into()?;
		loop {
			let cutoff = self.max_timestamp.checked_add(latency)?;

			// Keep track of all pending groups, buffering until we detect a timestamp far enough in the future.
			// This is a race; only the first group will succeed.
			// TODO is there a way to do this without FuturesUnordered?
			let mut buffering = FuturesUnordered::new();
			for (index, pending) in self.pending.iter_mut().enumerate() {
				buffering.push(async move { (index, pending.buffer_until(cutoff).await) })
			}

			tokio::select! {
				biased;
				Some(res) = async { Some(self.current.as_mut()?.read().await) } => {
					drop(buffering);

					match res {
						// Got the next frame.
						Ok(Some(frame)) => {
							tracing::trace!(?frame, "read frame");
							self.max_timestamp = frame.timestamp;
							return Ok(Some(frame));
						}
						Ok(None) | Err(_) => {
							// Group ended, instantly move to the next group.
							// We don't care about errors, which will happen if the group is closed early.
							self.current = self.pending.pop_front();
							continue;
						}
					};
				},
				Some(res) = async { self.track.next_group().await.transpose() } => {
					let group = GroupReader::new(res?);
					drop(buffering);

					match self.current.as_ref() {
						Some(current) if group.info.sequence < current.info.sequence => {
							// Ignore old groups
							tracing::debug!(old = ?group.info.sequence, current = ?current.info.sequence, "skipping old group");
						},
						Some(_) => {
							// Insert into pending based on the sequence number ascending.
							let index = self.pending.partition_point(|g| g.info.sequence < group.info.sequence);
							self.pending.insert(index, group);
						},
						None => self.current = Some(group),
					};
				},
				Some((index, timestamp)) = buffering.next() => {
					if self.current.is_some() {
						tracing::debug!(old = ?self.max_timestamp, new = ?timestamp, buffer = ?self.max_latency, "skipping slow group");
					}

					drop(buffering);

					if index > 0 {
						self.pending.drain(0..index);
						tracing::debug!(count = index, "skipping additional groups");
					}

					self.current = self.pending.pop_front();
				}
				else => return Ok(None),
			}
		}
	}

	/// Set the maximum latency tolerance for this consumer.
	///
	/// Groups with timestamps older than `max_timestamp - max_latency` will be skipped.
	pub fn set_max_latency(&mut self, max: std::time::Duration) {
		self.max_latency = max;
	}

	/// Wait until the track is closed.
	pub async fn closed(&self) -> Result<(), Error> {
		Ok(self.track.closed().await?)
	}
}

impl From<OrderedConsumer> for moq_lite::TrackConsumer {
	fn from(inner: OrderedConsumer) -> Self {
		inner.track
	}
}

impl std::ops::Deref for OrderedConsumer {
	type Target = moq_lite::TrackConsumer;

	fn deref(&self) -> &Self::Target {
		&self.track
	}
}

/// Internal reader for a group of frames.
struct GroupReader {
	// The group.
	group: moq_lite::GroupConsumer,

	// The current frame index
	index: usize,

	// The any buffered frames in the group.
	buffered: VecDeque<Frame>,

	// The max timestamp in the group
	max_timestamp: Option<Timestamp>,
}

impl GroupReader {
	fn new(group: moq_lite::GroupConsumer) -> Self {
		Self {
			group,
			index: 0,
			buffered: VecDeque::new(),
			max_timestamp: None,
		}
	}

	async fn read(&mut self) -> Result<Option<Frame>, Error> {
		if let Some(frame) = self.buffered.pop_front() {
			Ok(Some(frame))
		} else {
			self.read_unbuffered().await
		}
	}

	async fn read_unbuffered(&mut self) -> Result<Option<Frame>, Error> {
		let Some(mut frame) = self.group.next_frame().await? else {
			return Ok(None);
		};
		let payload = frame.read_chunks().await?;

		let mut payload = BufList::from_iter(payload);

		let timestamp = Timestamp::decode(&mut payload, ())?;

		let frame = Frame {
			keyframe: (self.index == 0),
			timestamp,
			payload,
		};

		self.index += 1;
		self.max_timestamp = Some(self.max_timestamp.unwrap_or_default().max(timestamp));

		Ok(Some(frame))
	}

	// Keep reading and buffering new frames, returning when `max` is larger than or equal to the cutoff.
	// This will BLOCK FOREVER if the group has ended early; it's intended to be used within select!
	async fn buffer_until(&mut self, cutoff: Timestamp) -> Timestamp {
		loop {
			match self.max_timestamp {
				Some(timestamp) if timestamp >= cutoff => return timestamp,
				_ => (),
			}

			match self.read().await {
				Ok(Some(frame)) => self.buffered.push_back(frame),
				// Otherwise block forever so we don't return from FuturesUnordered
				_ => std::future::pending().await,
			}
		}
	}
}

impl std::ops::Deref for GroupReader {
	type Target = moq_lite::GroupConsumer;

	fn deref(&self) -> &Self::Target {
		&self.group
	}
}
