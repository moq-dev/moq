use std::sync::Arc;

use bytes::Buf;

use crate::error::MoqError;
use crate::ffi::Task;
use crate::media::*;
use crate::origin::MoqRoute;
use crate::producer::MoqTrackInfo;

fn timestamp_us(timestamp: moq_net::Timestamp) -> Result<u64, MoqError> {
	timestamp
		.as_micros()
		.try_into()
		.map_err(|_| MoqError::TimeOverflow(moq_net::TimeOverflow))
}

fn raw_frame(frame: moq_net::frame::Frame) -> Result<MoqFrame, MoqError> {
	let timestamp_us = timestamp_us(frame.timestamp)?;
	Ok(MoqFrame {
		payload: frame.payload.to_vec(),
		timestamp_us,
	})
}

fn media_frame(mut frame: moq_mux::container::Frame) -> Result<MoqMediaFrame, MoqError> {
	let timestamp_us = timestamp_us(frame.timestamp)?;
	let payload = frame.payload.copy_to_bytes(frame.payload.remaining()).to_vec();

	Ok(MoqMediaFrame {
		payload,
		timestamp_us,
		keyframe: frame.keyframe,
	})
}

fn media_container(container: MoqContainer) -> Result<moq_mux::catalog::hang::Container, MoqError> {
	let container: hang::catalog::Container = container.into();
	(&container)
		.try_into()
		.map_err(|e| MoqError::Codec(format!("invalid container: {e}")))
}

/// Subscriber-side delivery preferences, mirroring [`moq_net::track::Subscription`].
///
/// Construct with the fields you care about; the rest default to moq-net's defaults
/// (priority 0, unordered, no staleness tolerance, full group range).
#[derive(Clone, uniffi::Record)]
pub struct MoqSubscription {
	/// Delivery priority; higher values preempt lower ones under bandwidth contention.
	#[uniffi(default = 0)]
	pub priority: u8,
	/// Whether groups are prioritized in sequence order. Groups may always arrive
	/// out-of-order (or not at all) over the network. Defaults to `false`; the
	/// aggregate is ordered only when every subscriber asks for it.
	#[uniffi(default = false)]
	pub ordered: bool,
	/// Maximum age of a non-latest group before it is skipped, in milliseconds.
	/// `0` skips immediately; a larger value tolerates that much reordering.
	///
	/// Enforced both by the publisher's cache (sent on the wire) and by any local
	/// buffering, such as `subscribe_media`'s jitter buffer.
	#[uniffi(default = 0)]
	pub max_age_ms: u64,
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
		moq_net::group::Fetch::default().with_priority(options.priority)
	}
}

impl From<MoqSubscription> for moq_net::track::Subscription {
	fn from(s: MoqSubscription) -> Self {
		moq_net::track::Subscription::default()
			.with_priority(s.priority)
			.with_ordered(s.ordered)
			.with_max_age(std::time::Duration::from_millis(s.max_age_ms))
			.with_start(s.group_start.map(moq_net::track::Position::group))
			.with_end(s.group_end.and_then(moq_net::track::Position::after_group))
	}
}

#[derive(Clone, uniffi::Object)]
pub struct MoqBroadcastConsumer {
	inner: moq_net::broadcast::Consumer,
	/// The origin this broadcast was resolved through, if any. A catalog rendition may name a
	/// sibling broadcast, and only an origin can fetch one; a standalone broadcast (a local
	/// producer's `consume`) has none, so such a rendition is unresolvable rather than wrong.
	origin: Option<moq_net::origin::Consumer>,
}

impl MoqBroadcastConsumer {
	/// Wrap a standalone broadcast, with no origin to resolve cross-broadcast references against.
	pub(crate) fn new(inner: moq_net::broadcast::Consumer) -> Self {
		Self { inner, origin: None }
	}

	/// Wrap a broadcast the origin handed out, so a rendition referencing a sibling broadcast
	/// resolves through the same origin (which deduplicates the shared subscription).
	///
	/// `origin` must be the cursor that named `inner`: the broadcast's path is stamped per
	/// cursor, and a reference resolves against that path, so an origin rooted elsewhere would
	/// read a legal reference as escaping (or worse, resolve it to the wrong broadcast).
	pub(crate) fn routed(inner: moq_net::broadcast::Consumer, origin: moq_net::origin::Consumer) -> Self {
		Self {
			inner,
			origin: Some(origin),
		}
	}

	/// Access the underlying `moq_net::broadcast::Consumer` for sibling
	/// modules (e.g. `audio`) that need to subscribe a typed track.
	pub(crate) fn inner(&self) -> &moq_net::broadcast::Consumer {
		&self.inner
	}

	/// Resolve a catalog rendition's `broadcast` reference to the broadcast serving its track.
	pub(crate) async fn resolve_inner(
		&self,
		reference: Option<&str>,
	) -> Result<moq_net::broadcast::Consumer, MoqError> {
		// Normalize before testing emptiness: this is a caller-supplied string, and one made only
		// of slashes normalizes to the empty reference, which names this broadcast.
		let reference = reference.map(moq_net::PathRelative::new);

		// An absent or empty reference names the catalog's own broadcast, which we already hold.
		// Short-circuiting also keeps a standalone broadcast usable: the common case needs no origin.
		let Some(reference) = reference.filter(|reference| !reference.is_empty()) else {
			return Ok(self.inner.clone());
		};

		let origin = self
			.origin
			.clone()
			.ok_or_else(|| MoqError::UnresolvableBroadcast(reference.as_str().to_string()))?;

		let source = moq_mux::Source::new(origin, &self.inner.info().path);
		Ok(source.resolve(Some(&reference)).await?)
	}
}

/// A watch over a broadcast's route. Created by `MoqBroadcastConsumer::route_updates`.
#[derive(uniffi::Object)]
pub struct MoqRouteWatch {
	task: Task<RouteWatch>,
}

struct RouteWatch {
	inner: moq_net::broadcast::Consumer,
}

impl RouteWatch {
	async fn next(&mut self) -> Result<Option<MoqRoute>, MoqError> {
		match self.inner.route_changed().await {
			Ok(route) => Ok(Some(route.into())),
			// A broadcast has no abort; Dropped (every producer gone) is its clean end.
			Err(moq_net::Error::Dropped) => Ok(None),
			Err(e) => Err(e.into()),
		}
	}
}

#[uniffi::export]
impl MoqRouteWatch {
	/// Wait for the next route: the current one on the first call, then each change.
	///
	/// Returns `None` once the broadcast ends (every producer gone).
	pub async fn next(&self) -> Result<Option<MoqRoute>, MoqError> {
		self.task.run(|mut state| async move { state.next().await }).await
	}

	/// Cancel all current and future `next()` calls.
	///
	/// Terminal: the subscription is released here, not when the handle is.
	pub fn cancel(&self) {
		self.task.cancel();
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
	async fn next(&mut self) -> Result<Option<MoqMediaFrame>, MoqError> {
		self.inner.read().await?.map(media_frame).transpose()
	}
}

// ---- Broadcast ----

#[uniffi::export]
impl MoqBroadcastConsumer {
	/// The route the broadcast currently takes to reach this origin.
	pub fn route(&self) -> MoqRoute {
		self.inner.route().into()
	}

	/// Watch the broadcast's route for changes.
	///
	/// The returned watch yields the current route first, then every update
	/// (e.g. an upstream failover), so a loop observes the full history from now.
	pub fn route_updates(&self) -> Arc<MoqRouteWatch> {
		Arc::new(MoqRouteWatch {
			task: Task::new(RouteWatch {
				inner: self.inner.clone(),
			}),
		})
	}

	/// Resolve a catalog rendition's `broadcast` reference to the broadcast serving its track.
	///
	/// `reference` is [`MoqVideo::broadcast`] / [`MoqAudio::broadcast`]: absent or empty names
	/// this broadcast, anything else names a sibling relative to it (e.g. `./source`). Call it on a
	/// rendition that carries one before [`Self::subscribe_media`], [`Self::subscribe_track`],
	/// [`Self::fetch_group`], or [`Self::fetch_media_group`], which take a track name rather than a
	/// rendition; `decode_video` and `decode_audio` resolve it themselves.
	///
	/// Errors if this broadcast came from a local producer rather than an origin, since a
	/// standalone broadcast has no sibling to name, and reports a sibling that exists but is not
	/// announced yet as unroutable rather than waiting for it (see
	/// [`MoqOriginConsumer::request_broadcast`](crate::origin::MoqOriginConsumer::request_broadcast)).
	pub async fn resolve(&self, reference: Option<String>) -> Result<Arc<MoqBroadcastConsumer>, MoqError> {
		let broadcast = self.resolve_inner(reference.as_deref()).await?;
		Ok(Arc::new(match self.origin.clone() {
			Some(origin) => Self::routed(broadcast, origin),
			None => Self::new(broadcast),
		}))
	}

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

	/// Subscribe to a track by name, the same pattern as moq-boy's command/status tracks.
	///
	/// Frames are returned as plain byte payloads with no codec or container parsing.
	/// `subscription` tunes delivery priority, group ordering priority, and group range; omit for defaults.
	pub async fn subscribe_track(
		&self,
		name: String,
		subscription: Option<MoqSubscription>,
	) -> Result<Arc<MoqTrackConsumer>, MoqError> {
		let subscription = subscription.map(moq_net::track::Subscription::from);
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

	/// Fetch one group and decode its track container into media frames.
	///
	/// Unlike [`Self::subscribe_media`], this does not create a live subscription or apply
	/// age-based group skipping. The returned consumer reads exactly the requested group
	/// until [`MoqMediaGroupConsumer::next`] returns `None`.
	pub async fn fetch_media_group(
		&self,
		name: String,
		sequence: u64,
		container: MoqContainer,
		options: Option<MoqFetchGroupOptions>,
	) -> Result<Arc<MoqMediaGroupConsumer>, MoqError> {
		// Parse the container before fetching so invalid CMAF init data does not leave a
		// dynamic group request waiting for a consumer that can never read it.
		let media = media_container(container)?;
		let options = options.map(moq_net::group::Fetch::from);
		let track = self.inner.track(&name).map_err(map_fetch_error)?;
		let group = track.fetch_group(sequence, options).await.map_err(map_fetch_error)?;
		Ok(Arc::new(MoqMediaGroupConsumer::new(group, media)))
	}

	/// Subscribe to a track by name, delivering frames in decode order.
	///
	/// `container` is the track container from the catalog.
	/// `subscription` tunes delivery priority, group ordering priority, and group range; omit for defaults.
	///
	/// [`MoqSubscription::max_age_ms`] bounds the local jitter buffer as well as
	/// the publisher's cache, so both ends skip a stalled group on the same budget.
	pub async fn subscribe_media(
		&self,
		name: String,
		container: MoqContainer,
		subscription: Option<MoqSubscription>,
	) -> Result<Arc<MoqMediaConsumer>, MoqError> {
		// Parse the container before subscribing so we don't leave a dangling
		// subscription if init parsing fails.
		let media = media_container(container)?;
		let subscription = subscription.map(moq_net::track::Subscription::from).unwrap_or_default();
		let track = self.inner.track(&name)?.subscribe(subscription).await?;
		let consumer = moq_mux::container::Consumer::new(track, media);
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

	async fn read_frame(&mut self) -> Result<Option<MoqFrame>, MoqError> {
		self.track.read_frame().await?.map(raw_frame).transpose()
	}

	async fn recv_datagram(&mut self) -> Result<Option<MoqDatagram>, MoqError> {
		let Some(datagram) = self.track.recv_datagram().await? else {
			return Ok(None);
		};
		let timestamp_us = datagram
			.timestamp
			.as_micros()
			.try_into()
			.map_err(|_| MoqError::Codec("timestamp overflow".into()))?;
		Ok(Some(MoqDatagram {
			sequence: datagram.sequence,
			timestamp_us,
			payload: datagram.payload.to_vec(),
		}))
	}
}

#[derive(uniffi::Object)]
pub struct MoqTrackConsumer {
	task: Task<TrackInner>,
	control: moq_net::track::SubscriberControl,
	info: moq_net::track::Info,
}

impl MoqTrackConsumer {
	pub(crate) fn new(track: moq_net::track::Subscriber) -> Self {
		let control = track.control();
		let info = track.info().clone();
		Self {
			task: Task::new(TrackInner { track }),
			control,
			info,
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

	/// Read the first frame of the next group, including its timestamp.
	///
	/// Convenience for tracks using one-frame-per-group (like moq-boy's
	/// status/command tracks). Returns `None` when the track ends.
	pub async fn read_frame(&self) -> Result<Option<MoqFrame>, MoqError> {
		self.task.run(|mut state| async move { state.read_frame().await }).await
	}

	/// Receive the next best-effort datagram in arrival order.
	///
	/// Returns `None` when the track ends. Datagram delivery is unavailable over
	/// IETF moq-transport, pre-lite-05 moq-lite, and stream-only transports.
	pub async fn recv_datagram(&self) -> Result<Option<MoqDatagram>, MoqError> {
		self.task
			.run(|mut state| async move { state.recv_datagram().await })
			.await
	}

	/// Return the publisher-side track properties learned during subscription.
	pub fn info(&self) -> Result<MoqTrackInfo, MoqError> {
		MoqTrackInfo::try_from(&self.info)
	}

	/// Change this subscriber's delivery preferences.
	///
	/// Silently ignored if the track already ended; the update is meaningless at
	/// that point.
	pub fn update(&self, subscription: MoqSubscription) {
		let _ = self.control.update(subscription.into());
	}

	/// Cancel all current and future reads.
	///
	/// Terminal: the subscription is released here, not when the handle is.
	pub fn cancel(&self) {
		self.task.cancel();
	}
}

struct GroupInner {
	group: moq_net::group::Consumer,
}

impl GroupInner {
	async fn read_frame(&mut self) -> Result<Option<MoqFrame>, MoqError> {
		self.group.read_frame().await?.map(raw_frame).transpose()
	}
}

#[derive(uniffi::Object)]
pub struct MoqGroupConsumer {
	sequence: u64,
	task: Task<GroupInner>,
}

struct MediaGroupInner {
	inner: moq_mux::container::GroupConsumer<moq_mux::catalog::hang::Container>,
}

impl MediaGroupInner {
	async fn next(&mut self) -> Result<Option<MoqMediaFrame>, MoqError> {
		self.inner.read().await?.map(media_frame).transpose()
	}
}

/// A finite, container-decoded media group returned by
/// [`MoqBroadcastConsumer::fetch_media_group`].
#[derive(uniffi::Object)]
pub struct MoqMediaGroupConsumer {
	sequence: u64,
	task: Task<MediaGroupInner>,
}

impl MoqMediaGroupConsumer {
	fn new(group: moq_net::group::Consumer, container: moq_mux::catalog::hang::Container) -> Self {
		let inner = moq_mux::container::GroupConsumer::new(group, container);
		Self {
			sequence: inner.sequence(),
			task: Task::new(MediaGroupInner { inner }),
		}
	}
}

#[uniffi::export]
impl MoqMediaGroupConsumer {
	/// The sequence number of this group within the track.
	pub fn sequence(&self) -> u64 {
		self.sequence
	}

	/// Read the next decoded media frame, or `None` when the group ends.
	pub async fn next(&self) -> Result<Option<MoqMediaFrame>, MoqError> {
		self.task.run(|mut state| async move { state.next().await }).await
	}

	/// Cancel all current and future `next()` calls.
	///
	/// Terminal: the subscription is released here, not when the handle is.
	pub fn cancel(&self) {
		self.task.cancel();
	}
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

	/// Read the next frame in this group, including its timestamp.
	///
	/// Returns `None` when the group ends.
	pub async fn read_frame(&self) -> Result<Option<MoqFrame>, MoqError> {
		self.task.run(|mut state| async move { state.read_frame().await }).await
	}

	/// Cancel all current and future `read_frame()` calls.
	///
	/// Terminal: the group and whatever it still buffers are released here, not when the handle is.
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
	///
	/// Terminal: the subscription is released here, not when the handle is.
	pub fn cancel(&self) {
		self.task.cancel();
	}
}

// ---- Media Consumer ----

#[uniffi::export]
impl MoqMediaConsumer {
	/// Get the next frame. Returns `None` when the track ends or is closed.
	pub async fn next(&self) -> Result<Option<MoqMediaFrame>, MoqError> {
		self.task.run(|mut state| async move { state.next().await }).await
	}

	/// Cancel all current and future `next()` calls.
	///
	/// Terminal: the subscription is released here, not when the handle is.
	pub fn cancel(&self) {
		self.task.cancel();
	}
}
