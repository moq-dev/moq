use std::ffi::c_char;
use tokio::sync::oneshot;

use crate::ffi::OnStatus;
use crate::{Error, Id, NonZeroSlab, State, moq_audio_config, moq_frame, moq_video_config};

struct ConsumeCatalog {
	broadcast: moq_net::BroadcastConsumer,

	catalog: hang::catalog::Catalog,

	/// We need to store the codec information on the heap unfortunately.
	audio_codec: Vec<String>,
	video_codec: Vec<String>,
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

#[derive(Default)]
pub struct Consume {
	/// Active broadcast consumers.
	broadcast: NonZeroSlab<moq_net::BroadcastConsumer>,

	/// Active catalog consumers and their broadcast references.
	catalog: NonZeroSlab<ConsumeCatalog>,

	/// Catalog consumer tasks. Close signals shutdown; the task delivers a final callback, then removes itself.
	catalog_task: NonZeroSlab<Option<TaskEntry>>,

	/// Track consumer tasks (video and audio).
	track_task: NonZeroSlab<Option<TaskEntry>>,

	/// Buffered frames ready for consumption.
	frame: NonZeroSlab<moq_mux::container::Frame>,

	/// Raw track consumer tasks (no media/container framing).
	raw_task: NonZeroSlab<Option<TaskEntry>>,

	/// Buffered raw frames ready for consumption.
	raw_frame: NonZeroSlab<bytes::Bytes>,
}

impl Consume {
	pub fn start(&mut self, broadcast: moq_net::BroadcastConsumer) -> Result<Id, Error> {
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
					.consume_track(hang::catalog::Catalog::DEFAULT_NAME)
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
		broadcast: moq_net::BroadcastConsumer,
		mut catalog: moq_mux::catalog::hang::Consumer,
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

			let snapshot = ConsumeCatalog {
				broadcast: broadcast.clone(),
				catalog: update,
				audio_codec,
				video_codec,
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
		let name = consume
			.catalog
			.video
			.renditions
			.keys()
			.nth(index)
			.ok_or(Error::NoIndex)?
			.clone();
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
					.consume_track(&name)
					.subscribe(moq_net::Subscription {
						priority: 1,
						..Default::default()
					})
					.await?;
				let track = moq_mux::container::Consumer::new(track, moq_mux::catalog::hang::Container::Legacy)
					.with_latency(latency);
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
		let name = consume
			.catalog
			.audio
			.renditions
			.keys()
			.nth(index)
			.ok_or(Error::NoIndex)?
			.clone();
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
					.consume_track(&name)
					.subscribe(moq_net::Subscription {
						priority: 2,
						..Default::default()
					})
					.await?;
				let track = moq_mux::container::Consumer::new(track, moq_mux::catalog::hang::Container::Legacy)
					.with_latency(latency);
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
	/// Frames are not chunked — the payload pointer is valid until the frame is closed
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
	/// in arrival order. Frames must be released with [`Self::raw_frame_close`].
	pub fn raw_track(&mut self, broadcast: Id, name: &str, on_frame: OnStatus) -> Result<Id, Error> {
		let broadcast = self.broadcast.get(broadcast).ok_or(Error::BroadcastNotFound)?.clone();
		let name = name.to_string();

		let channel = oneshot::channel();
		let entry = TaskEntry {
			close: Some(channel.0),
			callback: on_frame,
		};
		let id = self.raw_task.insert(Some(entry))?;

		// `subscribe` blocks on SUBSCRIBE_OK, so run it inside the task.
		tokio::spawn(async move {
			let res = async move {
				let track = broadcast
					.consume_track(&name)
					.subscribe(moq_net::Subscription::default())
					.await?;
				Self::run_raw(on_frame, track, channel.1).await
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

	async fn run_raw(
		callback: OnStatus,
		mut track: moq_net::TrackSubscriber,
		mut close: oneshot::Receiver<()>,
	) -> Result<(), Error> {
		// Deliver every frame in sequence order, reading all frames within each
		// group rather than the one-frame-per-group convenience. This is the
		// "raw track contents" model: the consumer sees exactly what the
		// producer wrote, regardless of how it was grouped.
		loop {
			// `biased` so a pending close always wins over a ready group.
			let mut group = tokio::select! {
				biased;
				_ = &mut close => return Ok(()),
				group = track.next_group() => match group? {
					Some(group) => group,
					None => return Ok(()),
				},
			};

			loop {
				let payload = tokio::select! {
					biased;
					_ = &mut close => return Ok(()),
					payload = group.read_frame() => match payload? {
						Some(payload) => payload,
						None => break,
					},
				};

				// Hold the lock only to buffer the frame; release it before the callback.
				let frame_id = State::lock().consume.raw_frame.insert(payload)?;
				callback.call(Ok(frame_id));
			}
		}
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

	/// Look up an audio rendition by catalog index, returning the
	/// (broadcast, config, name) tuple needed to subscribe — mirrors
	/// the index-based selection in `audio_ordered`.
	pub fn audio_rendition(
		&self,
		catalog: Id,
		index: usize,
	) -> Result<(moq_net::BroadcastConsumer, hang::catalog::AudioConfig, String), Error> {
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
