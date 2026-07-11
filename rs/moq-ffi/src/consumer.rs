use std::sync::Arc;

use bytes::Buf;

use crate::error::MoqError;
use crate::ffi::Task;
use crate::media::*;

/// Subscriber-side delivery preferences, mirroring [`moq_net::Subscription`].
///
/// Construct with the fields you care about; the rest default to moq-net's defaults
/// (priority 0, ordered, no staleness tolerance, full group range).
#[derive(Clone, uniffi::Record)]
pub struct MoqSubscription {
	/// Delivery priority; higher values preempt lower ones under bandwidth contention.
	#[uniffi(default = 0)]
	pub priority: u8,
	/// Deliver groups in sequence order.
	#[uniffi(default = true)]
	pub ordered: bool,
	/// How long to wait for an older group once a newer one has arrived before
	/// skipping it, in milliseconds. `0` skips immediately.
	#[uniffi(default = 0)]
	pub stale_ms: u64,
	/// First group to deliver, or null to start at the latest group.
	#[uniffi(default = None)]
	pub group_start: Option<u64>,
	/// Last group to deliver (inclusive), or null for no end.
	#[uniffi(default = None)]
	pub group_end: Option<u64>,
}

/// Options for fetching one past group by sequence.
#[derive(Clone, uniffi::Record)]
pub struct MoqFetchGroupOptions {
	/// Delivery priority for the fetch stream; higher values preempt lower ones.
	#[uniffi(default = 0)]
	pub priority: u8,
}

impl From<MoqFetchGroupOptions> for moq_net::group::Fetch {
	fn from(options: MoqFetchGroupOptions) -> Self {
		Self {
			priority: options.priority,
		}
	}
}

impl From<MoqSubscription> for moq_net::Subscription {
	fn from(s: MoqSubscription) -> Self {
		moq_net::Subscription::default()
			.with_priority(s.priority)
			.with_ordered(s.ordered)
			.with_stale(std::time::Duration::from_millis(s.stale_ms))
			.with_group_start(s.group_start)
			.with_group_end(s.group_end)
	}
}

#[derive(Clone, uniffi::Object)]
pub struct MoqBroadcastConsumer {
	inner: moq_net::broadcast::Consumer,
}

impl MoqBroadcastConsumer {
	pub(crate) fn new(inner: moq_net::broadcast::Consumer) -> Self {
		Self { inner }
	}

	/// Access the underlying `moq_net::broadcast::Consumer` for sibling
	/// modules (e.g. `audio`) that need to subscribe a typed track.
	pub(crate) fn inner(&self) -> &moq_net::broadcast::Consumer {
		&self.inner
	}
}

#[derive(uniffi::Object)]
pub struct MoqCatalogConsumer {
	task: Task<Catalog>,
}

struct Catalog {
	// Consume with the untyped `Extra` extension so application sections survive into
	// `MoqCatalog.sections` instead of being dropped.
	inner: moq_mux::catalog::hang::Consumer<moq_mux::catalog::hang::Extra>,
}

impl Catalog {
	async fn next(&mut self) -> Result<Option<MoqCatalog>, MoqError> {
		match self.inner.next().await {
			Ok(Some(catalog)) => Ok(Some(convert_catalog(&catalog))),
			Ok(None) => Ok(None),
			Err(e) => Err(e.into()),
		}
	}
}

#[derive(uniffi::Object)]
pub struct MoqMediaConsumer {
	task: Task<Media>,
}

struct Media {
	inner: moq_mux::container::Consumer<moq_mux::catalog::hang::Container>,
}

impl Media {
	async fn next(&mut self) -> Result<Option<MoqFrame>, MoqError> {
		let frame = self.inner.read().await?;

		let Some(frame) = frame else {
			return Ok(None);
		};

		let timestamp_us: u64 = frame
			.timestamp
			.as_micros()
			.try_into()
			.map_err(|_| MoqError::Codec("timestamp overflow".into()))?;

		let mut buf = frame.payload;
		let payload = buf.copy_to_bytes(buf.remaining()).to_vec();

		Ok(Some(MoqFrame {
			payload,
			timestamp_us,
			keyframe: frame.keyframe,
		}))
	}
}

// ---- Broadcast ----

#[uniffi::export]
impl MoqBroadcastConsumer {
	/// Subscribe to the catalog for this broadcast.
	pub async fn subscribe_catalog(&self) -> Result<Arc<MoqCatalogConsumer>, MoqError> {
		let track = self
			.inner
			.track(hang::catalog::Catalog::DEFAULT_NAME)?
			.subscribe(hang::catalog::Catalog::default_subscription())
			.await?;
		let consumer = moq_mux::catalog::hang::Consumer::from(track);
		Ok(Arc::new(MoqCatalogConsumer {
			task: Task::new(Catalog { inner: consumer }),
		}))
	}

	/// Subscribe to a track by name — same pattern as moq-boy's command/status tracks.
	///
	/// Frames are returned as plain byte payloads with no codec or container parsing.
	/// `subscription` tunes delivery (priority, ordering, group range); omit for defaults.
	pub async fn subscribe_track(
		&self,
		name: String,
		subscription: Option<MoqSubscription>,
	) -> Result<Arc<MoqTrackConsumer>, MoqError> {
		let subscription = subscription.map(moq_net::Subscription::from);
		let track = self.inner.track(&name)?.subscribe(subscription).await?;
		Ok(Arc::new(MoqTrackConsumer::new(track)))
	}

	/// Fetch one complete group by track name and group sequence.
	///
	/// This does not create a live subscription. A retained group resolves immediately;
	/// otherwise the request waits for a dynamic producer to serve it. The returned
	/// group may still be in progress, so read frames until `read_frame()` returns `None`.
	pub async fn fetch_group(
		&self,
		name: String,
		sequence: u64,
		options: Option<MoqFetchGroupOptions>,
	) -> Result<Arc<MoqGroupConsumer>, MoqError> {
		let options = options.map(moq_net::group::Fetch::from);
		let track = self.inner.track(&name).map_err(map_fetch_error)?;
		let group = track.fetch_group(sequence, options).await.map_err(map_fetch_error)?;
		Ok(Arc::new(MoqGroupConsumer::new(group)))
	}

	/// Subscribe to a track by name, delivering frames in decode order.
	///
	/// `container` is the track container from the catalog.
	/// `max_latency_ms` controls the maximum buffering before skipping a GoP.
	/// `subscription` tunes delivery (priority, ordering, group range); omit for defaults.
	pub async fn subscribe_media(
		&self,
		name: String,
		container: Container,
		max_latency_ms: u64,
		subscription: Option<MoqSubscription>,
	) -> Result<Arc<MoqMediaConsumer>, MoqError> {
		// Parse the container before subscribing so we don't leave a dangling
		// subscription if init parsing fails.
		let container: hang::catalog::Container = container.into();
		let media: moq_mux::catalog::hang::Container = (&container)
			.try_into()
			.map_err(|e| MoqError::Codec(format!("invalid container: {e}")))?;
		let subscription = subscription.map(moq_net::Subscription::from);
		let track = self.inner.track(&name)?.subscribe(subscription).await?;
		let latency = std::time::Duration::from_millis(max_latency_ms);
		let consumer = moq_mux::container::Consumer::new(track, media).with_latency(latency);
		Ok(Arc::new(MoqMediaConsumer {
			task: Task::new(Media { inner: consumer }),
		}))
	}
}

fn map_fetch_error(err: moq_net::Error) -> MoqError {
	match err {
		moq_net::Error::NotFound => MoqError::NotFound,
		moq_net::Error::Unsupported | moq_net::Error::Version => MoqError::Unsupported,
		err => err.into(),
	}
}

// ---- Track Consumer ----

struct TrackInner {
	track: moq_net::track::Subscriber,
}

impl TrackInner {
	async fn recv_group(&mut self) -> Result<Option<moq_net::group::Consumer>, MoqError> {
		Ok(self.track.recv_group().await?)
	}

	async fn next_group(&mut self) -> Result<Option<moq_net::group::Consumer>, MoqError> {
		Ok(self.track.next_group().await?)
	}

	async fn read_frame(&mut self) -> Result<Option<Vec<u8>>, MoqError> {
		Ok(self.track.read_frame().await?.map(|b| b.to_vec()))
	}
}

#[derive(uniffi::Object)]
pub struct MoqTrackConsumer {
	task: Task<TrackInner>,
}

impl MoqTrackConsumer {
	pub(crate) fn new(track: moq_net::track::Subscriber) -> Self {
		Self {
			task: Task::new(TrackInner { track }),
		}
	}
}

#[uniffi::export]
impl MoqTrackConsumer {
	/// Return the next group in arrival order. Returns `None` when the track ends.
	///
	/// Groups are returned as they arrive on the wire, which may be out of sequence
	/// order (e.g. if a later group lands before an earlier one on a separate stream).
	pub async fn recv_group(&self) -> Result<Option<Arc<MoqGroupConsumer>>, MoqError> {
		self.task
			.run(|mut state| async move {
				Ok(state.recv_group().await?.map(|group| {
					Arc::new(MoqGroupConsumer {
						sequence: group.sequence,
						task: Task::new(GroupInner { group }),
					})
				}))
			})
			.await
	}

	/// Return the next group in sequence order, skipping forward if the reader
	/// has fallen behind. Returns `None` when the track ends.
	pub async fn next_group(&self) -> Result<Option<Arc<MoqGroupConsumer>>, MoqError> {
		self.task
			.run(|mut state| async move {
				Ok(state.next_group().await?.map(|group| {
					Arc::new(MoqGroupConsumer {
						sequence: group.sequence,
						task: Task::new(GroupInner { group }),
					})
				}))
			})
			.await
	}

	/// Read the first frame of the next group.
	///
	/// Convenience for tracks using one-frame-per-group (like moq-boy's
	/// status/command tracks). Returns `None` when the track ends.
	pub async fn read_frame(&self) -> Result<Option<Vec<u8>>, MoqError> {
		self.task.run(|mut state| async move { state.read_frame().await }).await
	}

	pub fn cancel(&self) {
		self.task.cancel();
	}
}

struct GroupInner {
	group: moq_net::group::Consumer,
}

impl GroupInner {
	async fn read_frame(&mut self) -> Result<Option<Vec<u8>>, MoqError> {
		Ok(self.group.read_frame().await?.map(|b| b.to_vec()))
	}
}

#[derive(uniffi::Object)]
pub struct MoqGroupConsumer {
	sequence: u64,
	task: Task<GroupInner>,
}

impl MoqGroupConsumer {
	pub(crate) fn new(group: moq_net::group::Consumer) -> Self {
		Self {
			sequence: group.sequence,
			task: Task::new(GroupInner { group }),
		}
	}
}

#[uniffi::export]
impl MoqGroupConsumer {
	/// The sequence number of this group within the track.
	pub fn sequence(&self) -> u64 {
		self.sequence
	}

	/// Read the next frame in this group. Returns `None` when the group ends.
	pub async fn read_frame(&self) -> Result<Option<Vec<u8>>, MoqError> {
		self.task.run(|mut state| async move { state.read_frame().await }).await
	}

	pub fn cancel(&self) {
		self.task.cancel();
	}
}

// ---- Catalog Consumer ----

#[uniffi::export]
impl MoqCatalogConsumer {
	/// Get the next catalog update. Returns `None` when the track ends or is closed.
	pub async fn next(&self) -> Result<Option<MoqCatalog>, MoqError> {
		self.task.run(|mut state| async move { state.next().await }).await
	}

	/// Cancel all current and future `next()` calls.
	pub fn cancel(&self) {
		self.task.cancel();
	}
}

// ---- Media Consumer ----

#[uniffi::export]
impl MoqMediaConsumer {
	/// Get the next frame. Returns `None` when the track ends or is closed.
	pub async fn next(&self) -> Result<Option<MoqFrame>, MoqError> {
		self.task.run(|mut state| async move { state.next().await }).await
	}

	/// Cancel all current and future `next()` calls.
	pub fn cancel(&self) {
		self.task.cancel();
	}
}
