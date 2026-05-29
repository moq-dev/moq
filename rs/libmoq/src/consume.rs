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

/// A spawned task entry: close sender to signal shutdown, callback to deliver status.
/// Close revokes the callback by taking the entire entry.
struct TaskEntry {
	#[allow(dead_code)] // Dropping the sender signals the receiver.
	close: oneshot::Sender<()>,
	callback: OnStatus,
}

#[derive(Default)]
pub struct Consume {
	/// Active broadcast consumers.
	broadcast: NonZeroSlab<moq_net::BroadcastConsumer>,

	/// Active catalog consumers and their broadcast references.
	catalog: NonZeroSlab<ConsumeCatalog>,

	/// Catalog consumer tasks. Close takes the entry to revoke the callback.
	catalog_task: NonZeroSlab<Option<TaskEntry>>,

	/// Track consumer tasks (video and audio).
	track_task: NonZeroSlab<Option<TaskEntry>>,

	/// Buffered frames ready for consumption.
	frame: NonZeroSlab<moq_mux::container::Frame>,
}

impl Consume {
	pub fn start(&mut self, broadcast: moq_net::BroadcastConsumer) -> Result<Id, Error> {
		self.broadcast.insert(broadcast)
	}

	pub fn catalog(&mut self, broadcast: Id, on_catalog: OnStatus) -> Result<Id, Error> {
		let broadcast = self.broadcast.get(broadcast).ok_or(Error::BroadcastNotFound)?.clone();
		let catalog = broadcast.subscribe_track(&hang::catalog::Catalog::default_track())?;

		let channel = oneshot::channel();
		let entry = TaskEntry {
			close: channel.0,
			callback: on_catalog,
		};
		let id = self.catalog_task.insert(Some(entry))?;

		tokio::spawn(async move {
			let res = tokio::select! {
				res = Self::run_catalog(id, broadcast, catalog.into()) => res,
				_ = channel.1 => Ok(()),
			};

			// The lock is dropped before the callback is invoked.
			if let Some(entry) = State::lock().consume.catalog_task.remove(id).flatten() {
				entry.callback.call(res);
			}
		});

		Ok(id)
	}

	async fn run_catalog(
		task_id: Id,
		broadcast: moq_net::BroadcastConsumer,
		mut catalog: moq_mux::catalog::hang::Consumer,
	) -> Result<(), Error> {
		while let Some(catalog) = catalog.next().await? {
			// Unfortunately we need to store the codec information on the heap.
			let audio_codec = catalog
				.audio
				.renditions
				.values()
				.map(|config| config.codec.to_string())
				.collect();

			let video_codec = catalog
				.video
				.renditions
				.values()
				.map(|config| config.codec.to_string())
				.collect();

			let catalog = ConsumeCatalog {
				broadcast: broadcast.clone(),
				catalog,
				audio_codec,
				video_codec,
			};

			let mut state = State::lock();

			// Stop if the callback was revoked by close.
			let Some(Some(entry)) = state.consume.catalog_task.get(task_id) else {
				return Ok(());
			};
			let callback = entry.callback;

			let snapshot_id = state.consume.catalog.insert(catalog)?;
			drop(state);

			// The lock is dropped before the callback is invoked.
			callback.call(Ok(snapshot_id));
		}

		Ok(())
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
		// Take the entire entry: drops the sender (signals shutdown) and revokes the callback.
		self.catalog_task
			.get_mut(catalog)
			.ok_or(Error::CatalogNotFound)?
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
		let rendition = consume
			.catalog
			.video
			.renditions
			.keys()
			.nth(index)
			.ok_or(Error::NoIndex)?;

		let track = consume.broadcast.subscribe_track(&moq_net::Track {
			name: rendition.clone(),
			priority: 1, // TODO: Remove priority
			..Default::default()
		})?;
		let track =
			moq_mux::container::Consumer::new(track, moq_mux::catalog::hang::Container::Legacy).with_latency(latency);

		let channel = oneshot::channel();
		let entry = TaskEntry {
			close: channel.0,
			callback: on_frame,
		};
		let id = self.track_task.insert(Some(entry))?;

		tokio::spawn(async move {
			let res = tokio::select! {
				res = Self::run_track(id, track) => res,
				_ = channel.1 => Ok(()),
			};

			// The lock is dropped before the callback is invoked.
			if let Some(entry) = State::lock().consume.track_task.remove(id).flatten() {
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
		let rendition = consume
			.catalog
			.audio
			.renditions
			.keys()
			.nth(index)
			.ok_or(Error::NoIndex)?;

		let track = consume.broadcast.subscribe_track(&moq_net::Track {
			name: rendition.clone(),
			priority: 2, // TODO: Remove priority
			..Default::default()
		})?;
		let track =
			moq_mux::container::Consumer::new(track, moq_mux::catalog::hang::Container::Legacy).with_latency(latency);

		let channel = oneshot::channel();
		let entry = TaskEntry {
			close: channel.0,
			callback: on_frame,
		};
		let id = self.track_task.insert(Some(entry))?;

		tokio::spawn(async move {
			let res = tokio::select! {
				res = Self::run_track(id, track) => res,
				_ = channel.1 => Ok(()),
			};

			// The lock is dropped before the callback is invoked.
			if let Some(entry) = State::lock().consume.track_task.remove(id).flatten() {
				entry.callback.call(res);
			}
		});

		Ok(id)
	}

	async fn run_track(
		task_id: Id,
		mut track: moq_mux::container::Consumer<moq_mux::catalog::hang::Container>,
	) -> Result<(), Error> {
		while let Some(frame) = track.read().await? {
			let mut state = State::lock();

			// Stop if the callback was revoked by close.
			let Some(Some(entry)) = state.consume.track_task.get(task_id) else {
				return Ok(());
			};
			let callback = entry.callback;

			let frame_id = state.consume.frame.insert(frame)?;
			drop(state);

			// The lock is dropped before the callback is invoked.
			callback.call(Ok(frame_id));
		}

		Ok(())
	}

	pub fn track_close(&mut self, track: Id) -> Result<(), Error> {
		self.track_task
			.get_mut(track)
			.ok_or(Error::TrackNotFound)?
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
