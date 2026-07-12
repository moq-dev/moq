use std::{ffi::c_char, future::Future, pin::Pin, task::Poll};
use tokio::sync::{mpsc, oneshot};

use crate::ffi::OnStatus;
use crate::{
	Error, Id, NonZeroSlab, State, moq_audio_config, moq_datagram, moq_frame, moq_section, moq_string, moq_video_config,
};

struct ConsumeCatalog {
	broadcast: moq_net::broadcast::Consumer,

	// Carries the untyped `Extra` extension so application catalog sections survive
	// into `moq_catalog_section_*` instead of being dropped on parse.
	catalog: moq_mux::catalog::hang::Catalog<moq_mux::catalog::hang::Extra>,

	/// We need to store the codec information on the heap unfortunately.
	audio_codec: Vec<String>,
	video_codec: Vec<String>,

	/// Section names and their JSON, serialized on the heap so the section iterator
	/// and direct-lookup APIs can hand C borrowed pointers into stable storage.
	sections: Vec<(String, String)>,
}

/// A spawned task entry: `close` signals shutdown, `callback` delivers status.
///
/// `close` is an `Option` so `*_close` can drop just the sender (signalling
/// shutdown) without removing the entry or revoking the callback. The task
/// removes its own entry only after delivering one final terminal callback,
/// so `user_data` stays valid until that callback fires.
struct TaskEntry {
	close: Option<oneshot::Sender<()>>,
	callback: OnStatus,
}

/// A raw track task also accepts subscription updates while it is running.
struct RawTaskEntry {
	close: Option<oneshot::Sender<()>>,
	update: mpsc::UnboundedSender<Option<moq_net::Subscription>>,
	callback: OnStatus,
}

/// Outcome of polling a raw track source alongside its control channels.
enum RawStep<T> {
	/// A delivered value: the next group, or the next frame within a group.
	Item(T),
	/// The source is exhausted: the track finished, or the group was fully read.
	End,
	/// The consumer was closed; the task must unwind without re-polling `close`.
	Stop,
}

#[derive(Default)]
pub struct Consume {
	/// Active broadcast consumers.
	broadcast: NonZeroSlab<moq_net::broadcast::Consumer>,

	/// Active catalog consumers and their broadcast references.
	catalog: NonZeroSlab<ConsumeCatalog>,

	/// Catalog consumer tasks. Close signals shutdown; the task delivers a final callback, then removes itself.
	catalog_task: NonZeroSlab<Option<TaskEntry>>,

	/// Track consumer tasks (video and audio).
	track_task: NonZeroSlab<Option<TaskEntry>>,

	/// Buffered frames ready for consumption.
	frame: NonZeroSlab<moq_mux::container::Frame>,

	/// Raw track consumer tasks (no media/container framing).
	raw_task: NonZeroSlab<Option<RawTaskEntry>>,

	/// Buffered raw frames ready for consumption.
	raw_frame: NonZeroSlab<bytes::Bytes>,

	/// Raw track datagram consumer tasks (best-effort, parallel to `raw_task`).
	datagram_task: NonZeroSlab<Option<TaskEntry>>,

	/// Buffered datagrams ready for consumption.
	datagram: NonZeroSlab<moq_net::Datagram>,
}

impl Consume {
	pub fn start(&mut self, broadcast: moq_net::broadcast::Consumer) -> Result<Id, Error> {
		self.broadcast.insert(broadcast)
	}

	pub fn catalog(&mut self, broadcast: Id, on_catalog: OnStatus) -> Result<Id, Error> {
		let broadcast = self.broadcast.get(broadcast).ok_or(Error::BroadcastNotFound)?.clone();

		let channel = oneshot::channel();
		let entry = TaskEntry {
			close: Some(channel.0),
			callback: on_catalog,
		};
		let id = self.catalog_task.insert(Some(entry))?;

		// `subscribe` blocks on SUBSCRIBE_OK, so run it inside the task
		// to keep this entrypoint non-blocking.
		tokio::spawn(async move {
			let res = async move {
				let catalog = broadcast
					.track(hang::catalog::Catalog::DEFAULT_NAME)?
					.subscribe(hang::catalog::Catalog::default_subscription())
					.await?;
				Self::run_catalog(on_catalog, broadcast.clone(), catalog.into(), channel.1).await
			}
			.await;

			// Deliver one final terminal callback (code <= 0), then drop the entry.
			// Pull it out from under the lock so the callback never runs while held.
			let entry = State::lock().consume.catalog_task.remove(id).flatten();
			if let Some(entry) = entry {
				entry.callback.call(res);
			}
		});

		Ok(id)
	}

	async fn run_catalog(
		callback: OnStatus,
		broadcast: moq_net::broadcast::Consumer,
		mut catalog: moq_mux::catalog::hang::Consumer<moq_mux::catalog::hang::Extra>,
		mut close: oneshot::Receiver<()>,
	) -> Result<(), Error> {
		loop {
			// `biased` so a pending close always wins over a ready update: a hot
			// stream must not be able to starve the close signal, and we must not
			// deliver another update once close has been requested.
			let update = tokio::select! {
				biased;
				_ = &mut close => return Ok(()),
				next = catalog.next() => match next? {
					Some(update) => update,
					None => return Ok(()),
				},
			};

			// Unfortunately we need to store the codec information on the heap.
			let audio_codec = update
				.audio
				.renditions
				.values()
				.map(|config| config.codec.to_string())
				.collect();

			let video_codec = update
				.video
				.renditions
				.values()
				.map(|config| config.codec.to_string())
				.collect();

			// Serialize the untyped application sections to owned strings so the
			// C section APIs can borrow stable pointers from the snapshot.
			let sections = update
				.sections()
				.map(|(name, value)| (name.clone(), value.to_string()))
				.collect();

			let snapshot = ConsumeCatalog {
				broadcast: broadcast.clone(),
				catalog: update,
				audio_codec,
				video_codec,
				sections,
			};

			// Hold the lock only to buffer the snapshot; release it before the callback.
			let snapshot_id = State::lock().consume.catalog.insert(snapshot)?;
			callback.call(Ok(snapshot_id));
		}
	}

	pub fn video_config(&mut self, catalog: Id, index: usize, dst: &mut moq_video_config) -> Result<(), Error> {
		let consume = self.catalog.get(catalog).ok_or(Error::CatalogNotFound)?;

		let (rendition, config) = consume
			.catalog
			.video
			.renditions
			.iter()
			.nth(index)
			.ok_or(Error::NoIndex)?;
		let codec = consume.video_codec.get(index).ok_or(Error::NoIndex)?;

		*dst = moq_video_config {
			name: rendition.as_str().as_ptr() as *const c_char,
			name_len: rendition.len(),
			codec: codec.as_str().as_ptr() as *const c_char,
			codec_len: codec.len(),
			description: config
				.description
				.as_ref()
				.map(|desc| desc.as_ptr())
				.unwrap_or(std::ptr::null()),
			description_len: config.description.as_ref().map(|desc| desc.len()).unwrap_or(0),
			coded_width: config
				.coded_width
				.as_ref()
				.map(|width| width as *const u32)
				.unwrap_or(std::ptr::null()),
			coded_height: config
				.coded_height
				.as_ref()
				.map(|height| height as *const u32)
				.unwrap_or(std::ptr::null()),
		};

		Ok(())
	}

	pub fn audio_config(&mut self, catalog: Id, index: usize, dst: &mut moq_audio_config) -> Result<(), Error> {
		let consume = self.catalog.get(catalog).ok_or(Error::CatalogNotFound)?;

		let (rendition, config) = consume
			.catalog
			.audio
			.renditions
			.iter()
			.nth(index)
			.ok_or(Error::NoIndex)?;
		let codec = consume.audio_codec.get(index).ok_or(Error::NoIndex)?;

		*dst = moq_audio_config {
			name: rendition.as_str().as_ptr() as *const c_char,
			name_len: rendition.len(),
			codec: codec.as_str().as_ptr() as *const c_char,
			codec_len: codec.len(),
			description: config
				.description
				.as_ref()
				.map(|desc| desc.as_ptr())
				.unwrap_or(std::ptr::null()),
			description_len: config.description.as_ref().map(|desc| desc.len()).unwrap_or(0),
			sample_rate: config.sample_rate,
			channel_count: config.channel_count,
		};

		Ok(())
	}

	/// Number of untyped application catalog sections in this snapshot.
	pub fn catalog_section_count(&self, catalog: Id) -> Result<usize, Error> {
		let consume = self.catalog.get(catalog).ok_or(Error::CatalogNotFound)?;
		Ok(consume.sections.len())
	}

	/// Fill `dst` with the section at `index` (name + JSON value). The pointers
	/// borrow the snapshot and stay valid until it is freed.
	pub fn catalog_section_at(&self, catalog: Id, index: usize, dst: &mut moq_section) -> Result<(), Error> {
		let consume = self.catalog.get(catalog).ok_or(Error::CatalogNotFound)?;
		let (name, json) = consume.sections.get(index).ok_or(Error::NoIndex)?;

		*dst = moq_section {
			name: name.as_ptr() as *const c_char,
			name_len: name.len(),
			json: json.as_ptr() as *const c_char,
			json_len: json.len(),
		};

		Ok(())
	}

	/// Fill `dst` with the JSON value of the section named `name`. The pointer
	/// borrows the snapshot and stays valid until it is freed. Errors with
	/// [`Error::NotFound`] if no section with that name exists.
	pub fn catalog_section_get(&self, catalog: Id, name: &str, dst: &mut moq_string) -> Result<(), Error> {
		let consume = self.catalog.get(catalog).ok_or(Error::CatalogNotFound)?;
		let (_, json) = consume
			.sections
			.iter()
			.find(|(section, _)| section == name)
			.ok_or(Error::NotFound)?;

		*dst = moq_string {
			data: json.as_ptr() as *const c_char,
			len: json.len(),
		};

		Ok(())
	}

	pub fn catalog_close(&mut self, catalog: Id) -> Result<(), Error> {
		// Signal shutdown by dropping the sender. The task still delivers one
		// final callback and then removes itself, so this neither revokes the
		// callback nor frees user_data. Errors if already closed.
		self.catalog_task
			.get_mut(catalog)
			.and_then(|entry| entry.as_mut())
			.ok_or(Error::CatalogNotFound)?
			.close
			.take()
			.ok_or(Error::CatalogNotFound)?;
		Ok(())
	}

	pub fn catalog_free(&mut self, catalog: Id) -> Result<(), Error> {
		self.catalog.remove(catalog).ok_or(Error::CatalogNotFound)?;
		Ok(())
	}

	pub fn video_ordered(
		&mut self,
		catalog: Id,
		index: usize,
		latency: std::time::Duration,
		on_frame: OnStatus,
	) -> Result<Id, Error> {
		let consume = self.catalog.get(catalog).ok_or(Error::CatalogNotFound)?;
		let (name, config) = consume
			.catalog
			.video
			.renditions
			.iter()
			.nth(index)
			.ok_or(Error::NoIndex)?;
		let name = name.clone();
		// Consume with the container the catalog actually advertises (Legacy / Cmaf / Loc)
		// instead of assuming Legacy, otherwise CMAF/fMP4 sources (e.g. ffmpeg moqenc,
		// browser @moq/publish) are misread as raw frames.
		let container = moq_mux::catalog::hang::Container::try_from(&config.container)?;
		let broadcast = consume.broadcast.clone();

		let channel = oneshot::channel();
		let entry = TaskEntry {
			close: Some(channel.0),
			callback: on_frame,
		};
		let id = self.track_task.insert(Some(entry))?;

		// `subscribe` blocks on SUBSCRIBE_OK, so run it inside the task.
		tokio::spawn(async move {
			let res = async move {
				let track = broadcast
					.track(&name)?
					.subscribe(moq_net::Subscription {
						priority: 1,
						..Default::default()
					})
					.await?;
				let track = moq_mux::container::Consumer::new(track, container).with_latency(latency);
				Self::run_track(on_frame, track, channel.1).await
			}
			.await;

			// Deliver one final terminal callback (code <= 0), then drop the entry.
			// Pull it out from under the lock so the callback never runs while held.
			let entry = State::lock().consume.track_task.remove(id).flatten();
			if let Some(entry) = entry {
				entry.callback.call(res);
			}
		});

		Ok(id)
	}

	pub fn audio_ordered(
		&mut self,
		catalog: Id,
		index: usize,
		latency: std::time::Duration,
		on_frame: OnStatus,
	) -> Result<Id, Error> {
		let consume = self.catalog.get(catalog).ok_or(Error::CatalogNotFound)?;
		let (name, config) = consume
			.catalog
			.audio
			.renditions
			.iter()
			.nth(index)
			.ok_or(Error::NoIndex)?;
		let name = name.clone();
		let container = moq_mux::catalog::hang::Container::try_from(&config.container)?;
		let broadcast = consume.broadcast.clone();

		let channel = oneshot::channel();
		let entry = TaskEntry {
			close: Some(channel.0),
			callback: on_frame,
		};
		let id = self.track_task.insert(Some(entry))?;

		// `subscribe` blocks on SUBSCRIBE_OK, so run it inside the task.
		tokio::spawn(async move {
			let res = async move {
				let track = broadcast
					.track(&name)?
					.subscribe(moq_net::Subscription {
						priority: 2,
						..Default::default()
					})
					.await?;
				let track = moq_mux::container::Consumer::new(track, container).with_latency(latency);
				Self::run_track(on_frame, track, channel.1).await
			}
			.await;

			// Deliver one final terminal callback (code <= 0), then drop the entry.
			// Pull it out from under the lock so the callback never runs while held.
			let entry = State::lock().consume.track_task.remove(id).flatten();
			if let Some(entry) = entry {
				entry.callback.call(res);
			}
		});

		Ok(id)
	}

	async fn run_track(
		callback: OnStatus,
		mut track: moq_mux::container::Consumer<moq_mux::catalog::hang::Container>,
		mut close: oneshot::Receiver<()>,
	) -> Result<(), Error> {
		loop {
			// `biased` so a pending close always wins over a ready frame.
			let frame = tokio::select! {
				biased;
				_ = &mut close => return Ok(()),
				frame = track.read() => match frame? {
					Some(frame) => frame,
					None => return Ok(()),
				},
			};

			// Hold the lock only to buffer the frame; release it before the callback.
			let frame_id = State::lock().consume.frame.insert(frame)?;
			callback.call(Ok(frame_id));
		}
	}

	pub fn track_close(&mut self, track: Id) -> Result<(), Error> {
		// Signal shutdown; the task delivers a final callback and removes itself.
		self.track_task
			.get_mut(track)
			.and_then(|entry| entry.as_mut())
			.ok_or(Error::TrackNotFound)?
			.close
			.take()
			.ok_or(Error::TrackNotFound)?;
		Ok(())
	}

	/// Read the payload of a frame as a single contiguous slice.
	///
	/// Frames are not chunked. The payload pointer is valid until the frame is closed
	/// via [`Self::frame_close`].
	pub fn frame(&self, frame: Id, dst: &mut moq_frame) -> Result<(), Error> {
		let f = self.frame.get(frame).ok_or(Error::FrameNotFound)?;

		let timestamp_us = f.timestamp.as_micros().try_into().map_err(|_| moq_net::TimeOverflow)?;

		*dst = moq_frame {
			payload: f.payload.as_ptr(),
			payload_size: f.payload.len(),
			timestamp_us,
			keyframe: f.keyframe,
		};

		Ok(())
	}

	pub fn frame_close(&mut self, frame: Id) -> Result<(), Error> {
		self.frame.remove(frame).ok_or(Error::FrameNotFound)?;
		Ok(())
	}

	pub fn close(&mut self, consume: Id) -> Result<(), Error> {
		self.broadcast.remove(consume).ok_or(Error::BroadcastNotFound)?;
		Ok(())
	}

	/// Subscribe to a raw track by name, delivering each frame's payload as-is.
	///
	/// No catalog lookup or container parsing. This is the moq-net primitive for
	/// non-media tracks. `on_frame` is called with a raw frame ID for each frame,
	/// in sequence order. Frames must be released with [`Self::raw_frame_close`].
	pub fn raw_track(
		&mut self,
		broadcast: Id,
		name: &str,
		subscription: Option<moq_net::Subscription>,
		on_frame: OnStatus,
	) -> Result<Id, Error> {
		let broadcast = self.broadcast.get(broadcast).ok_or(Error::BroadcastNotFound)?.clone();
		let name = name.to_string();

		let channel = oneshot::channel();
		let (update, updates) = mpsc::unbounded_channel();
		let entry = RawTaskEntry {
			close: Some(channel.0),
			update,
			callback: on_frame,
		};
		let id = self.raw_task.insert(Some(entry))?;

		// `subscribe` blocks on SUBSCRIBE_OK, so run it inside the task.
		tokio::spawn(async move {
			let res = async move {
				let mut track = broadcast.track(&name)?.subscribe(subscription.clone()).await?;
				Self::apply_raw_subscription(&mut track, subscription);
				Self::run_raw(on_frame, track, channel.1, updates).await
			}
			.await;

			// Deliver one final terminal callback (code <= 0), then drop the entry.
			// Pull it out from under the lock so the callback never runs while held.
			let entry = State::lock().consume.raw_task.remove(id).flatten();
			if let Some(entry) = entry {
				entry.callback.call(res);
			}
		});

		Ok(id)
	}

	fn apply_raw_subscription(track: &mut moq_net::track::Subscriber, subscription: Option<moq_net::Subscription>) {
		let subscription = subscription.unwrap_or_default();
		if let Some(start) = subscription.group_start.or_else(|| track.latest()) {
			track.start_at(start);
		}
		track.end_at(subscription.group_end);
		track.update(subscription);
	}

	async fn run_raw(
		callback: OnStatus,
		mut track: moq_net::track::Subscriber,
		mut close: oneshot::Receiver<()>,
		mut updates: mpsc::UnboundedReceiver<Option<moq_net::Subscription>>,
	) -> Result<(), Error> {
		// Deliver every frame in sequence order, reading all frames within each
		// group rather than the one-frame-per-group convenience. This is the
		// "raw track contents" model: the consumer sees exactly what the
		// producer wrote, regardless of how it was grouped.
		//
		// `close` is a oneshot that panics if polled after completion, so a `Stop`
		// must unwind the whole task rather than fall through to the outer loop.
		loop {
			let mut group =
				match moq_net::kio::wait(|waiter| -> Poll<Result<RawStep<moq_net::group::Consumer>, Error>> {
					if Self::poll_raw_control(&mut close, &mut updates, &mut track, waiter) {
						return Poll::Ready(Ok(RawStep::Stop));
					}
					match track.poll_next_group(waiter) {
						Poll::Ready(Ok(Some(group))) => Poll::Ready(Ok(RawStep::Item(group))),
						Poll::Ready(Ok(None)) => Poll::Ready(Ok(RawStep::End)),
						Poll::Ready(Err(err)) => Poll::Ready(Err(err.into())),
						Poll::Pending => Poll::Pending,
					}
				})
				.await?
				{
					RawStep::Item(group) => group,
					// Track finished or the consumer was closed: nothing left to deliver.
					RawStep::End | RawStep::Stop => return Ok(()),
				};

			loop {
				let payload = match moq_net::kio::wait(|waiter| -> Poll<Result<RawStep<bytes::Bytes>, Error>> {
					if Self::poll_raw_control(&mut close, &mut updates, &mut track, waiter) {
						return Poll::Ready(Ok(RawStep::Stop));
					}
					match group.poll_read_frame(waiter) {
						Poll::Ready(Ok(Some(payload))) => Poll::Ready(Ok(RawStep::Item(payload))),
						Poll::Ready(Ok(None)) => Poll::Ready(Ok(RawStep::End)),
						Poll::Ready(Err(err)) => Poll::Ready(Err(err.into())),
						Poll::Pending => Poll::Pending,
					}
				})
				.await?
				{
					RawStep::Item(payload) => payload,
					// Group fully read: advance to the next group.
					RawStep::End => break,
					// Consumer closed mid-group: terminate without touching `close` again.
					RawStep::Stop => return Ok(()),
				};

				// Hold the lock only to buffer the frame; release it before the callback.
				let frame_id = State::lock().consume.raw_frame.insert(payload)?;
				callback.call(Ok(frame_id));
			}
		}
	}

	/// Poll the close and update channels, applying any subscription updates inline.
	///
	/// Returns `true` when the task must stop: either the consumer was closed
	/// (`close` fired) or the update channel was dropped. The caller must then
	/// unwind rather than poll `close` again, since a completed oneshot panics if
	/// re-polled. Borrows `track` only for the duration of the call so the caller
	/// can poll a track/group source afterwards.
	fn poll_raw_control(
		close: &mut oneshot::Receiver<()>,
		updates: &mut mpsc::UnboundedReceiver<Option<moq_net::Subscription>>,
		track: &mut moq_net::track::Subscriber,
		waiter: &moq_net::kio::Waiter,
	) -> bool {
		let mut cx = std::task::Context::from_waker(waiter.waker());
		if Pin::new(close).poll(&mut cx).is_ready() {
			return true;
		}

		loop {
			match updates.poll_recv(&mut cx) {
				Poll::Ready(Some(subscription)) => Self::apply_raw_subscription(track, subscription),
				Poll::Ready(None) => return true,
				Poll::Pending => return false,
			}
		}
	}

	pub fn raw_track_update(&mut self, track: Id, subscription: Option<moq_net::Subscription>) -> Result<(), Error> {
		let entry = self
			.raw_task
			.get_mut(track)
			.and_then(|entry| entry.as_mut())
			.ok_or(Error::TrackNotFound)?;
		entry.update.send(subscription).map_err(|_| Error::TrackNotFound)?;
		Ok(())
	}

	pub fn raw_track_close(&mut self, track: Id) -> Result<(), Error> {
		// Signal shutdown; the task delivers a final callback and removes itself.
		self.raw_task
			.get_mut(track)
			.and_then(|entry| entry.as_mut())
			.ok_or(Error::TrackNotFound)?
			.close
			.take()
			.ok_or(Error::TrackNotFound)?;
		Ok(())
	}

	/// Fill `dst` with a raw frame's payload. The pointer is valid until the
	/// frame is released with [`Self::raw_frame_close`].
	pub fn raw_frame(&self, frame: Id, dst: &mut moq_frame) -> Result<(), Error> {
		let payload = self.raw_frame.get(frame).ok_or(Error::FrameNotFound)?;

		*dst = moq_frame {
			payload: payload.as_ptr(),
			payload_size: payload.len(),
			timestamp_us: 0,
			keyframe: false,
		};

		Ok(())
	}

	pub fn raw_frame_close(&mut self, frame: Id) -> Result<(), Error> {
		self.raw_frame.remove(frame).ok_or(Error::FrameNotFound)?;
		Ok(())
	}

	/// Subscribe to a raw track's best-effort datagrams by name.
	///
	/// Parallel to [`Self::raw_track`], but delivers datagrams instead of frames.
	/// `on_datagram` is called with a datagram ID for each datagram in arrival order;
	/// each must be released with [`Self::datagram_close`].
	pub fn datagram_track(&mut self, broadcast: Id, name: &str, on_datagram: OnStatus) -> Result<Id, Error> {
		let broadcast = self.broadcast.get(broadcast).ok_or(Error::BroadcastNotFound)?.clone();
		let name = name.to_string();

		let channel = oneshot::channel();
		let entry = TaskEntry {
			close: Some(channel.0),
			callback: on_datagram,
		};
		let id = self.datagram_task.insert(Some(entry))?;

		// `subscribe` blocks on SUBSCRIBE_OK, so run it inside the task.
		tokio::spawn(async move {
			let res = async move {
				let track = broadcast.track(&name)?.subscribe(None).await?;
				Self::run_datagrams(on_datagram, track, channel.1).await
			}
			.await;

			// Deliver one final terminal callback (code <= 0), then drop the entry.
			// Pull it out from under the lock so the callback never runs while held.
			let entry = State::lock().consume.datagram_task.remove(id).flatten();
			if let Some(entry) = entry {
				entry.callback.call(res);
			}
		});

		Ok(id)
	}

	async fn run_datagrams(
		callback: OnStatus,
		mut track: moq_net::track::Subscriber,
		mut close: oneshot::Receiver<()>,
	) -> Result<(), Error> {
		loop {
			// `biased` so a pending close always wins over a ready datagram.
			let datagram = tokio::select! {
				biased;
				_ = &mut close => return Ok(()),
				datagram = track.recv_datagram() => match datagram? {
					Some(datagram) => datagram,
					None => return Ok(()),
				},
			};

			// Hold the lock only to buffer the datagram; release it before the callback.
			let id = State::lock().consume.datagram.insert(datagram)?;
			callback.call(Ok(id));
		}
	}

	pub fn datagram_track_close(&mut self, track: Id) -> Result<(), Error> {
		// Signal shutdown; the task delivers a final callback and removes itself.
		self.datagram_task
			.get_mut(track)
			.and_then(|entry| entry.as_mut())
			.ok_or(Error::TrackNotFound)?
			.close
			.take()
			.ok_or(Error::TrackNotFound)?;
		Ok(())
	}

	/// Fill `dst` with a datagram's payload, timestamp, and sequence. The pointer is
	/// valid until the datagram is released with [`Self::datagram_close`].
	pub fn datagram(&self, datagram: Id, dst: &mut moq_datagram) -> Result<(), Error> {
		let value = self.datagram.get(datagram).ok_or(Error::FrameNotFound)?;

		*dst = moq_datagram {
			payload: value.payload.as_ptr(),
			payload_size: value.payload.len(),
			timestamp_us: value.timestamp.as_micros() as u64,
			sequence: value.sequence,
		};

		Ok(())
	}

	pub fn datagram_close(&mut self, datagram: Id) -> Result<(), Error> {
		self.datagram.remove(datagram).ok_or(Error::FrameNotFound)?;
		Ok(())
	}

	/// Look up a video rendition by catalog index, returning the
	/// (broadcast, config, name) tuple needed to subscribe. Mirrors
	/// the index-based selection in `video_ordered`.
	pub fn video_rendition(
		&self,
		catalog: Id,
		index: usize,
	) -> Result<(moq_net::broadcast::Consumer, hang::catalog::VideoConfig, String), Error> {
		let consume = self.catalog.get(catalog).ok_or(Error::CatalogNotFound)?;
		let (name, config) = consume
			.catalog
			.video
			.renditions
			.iter()
			.nth(index)
			.ok_or(Error::NoIndex)?;
		Ok((consume.broadcast.clone(), config.clone(), name.clone()))
	}

	/// Look up an audio rendition by catalog index, returning the
	/// (broadcast, config, name) tuple needed to subscribe. Mirrors
	/// the index-based selection in `audio_ordered`.
	pub fn audio_rendition(
		&self,
		catalog: Id,
		index: usize,
	) -> Result<(moq_net::broadcast::Consumer, hang::catalog::AudioConfig, String), Error> {
		let consume = self.catalog.get(catalog).ok_or(Error::CatalogNotFound)?;
		let (name, config) = consume
			.catalog
			.audio
			.renditions
			.iter()
			.nth(index)
			.ok_or(Error::NoIndex)?;
		Ok((consume.broadcast.clone(), config.clone(), name.clone()))
	}
}
